//! Trusted request context installed by the authenticated transport boundary.
//! Missing context is an error: background work must carry explicit attribution.
use access::RequestContext;
use api::AgentApiError;
use auth::{PrincipalKind, PrincipalRef};
use tracing::Instrument as _;

tokio::task_local! {
    static REQUEST_CONTEXT: RequestContext;
    /// Set when a decision of the request relied on `read_private_content`;
    /// the boundary audits the request as privileged.
    static PRIVILEGED: std::cell::Cell<bool>;
}

/// Runs the request under its trusted context. In-process callers have no
/// audit boundary, so the privileged marker is dropped here.
pub async fn with_request_context<F: Future>(context: RequestContext, future: F) -> F::Output {
    with_audited_request_context(context, future).await.0
}

/// Runs the request under its trusted context and reports whether any of its
/// decisions relied on the privileged-read capability, for the boundary to
/// audit.
pub async fn with_audited_request_context<F: Future>(
    context: RequestContext,
    future: F,
) -> (F::Output, bool) {
    let span = tracing::info_span!("request_authority",
        acting_principal = %context.acting_principal().id,
        authenticated_principal = %context.authenticated_principal.id,
        authentication = ?context.authentication,
        target_scope = ?context.target_scope());
    PRIVILEGED
        .scope(std::cell::Cell::new(false), async {
            let output = REQUEST_CONTEXT.scope(context, future).await;
            (output, PRIVILEGED.with(std::cell::Cell::get))
        })
        .instrument(span)
        .await
}

/// Record that the current request read something only the privileged-read
/// capability allowed. Outside a request scope there is nothing to mark.
pub fn mark_privileged() {
    let _ = PRIVILEGED.try_with(|privileged| privileged.set(true));
}

pub fn request_context() -> Result<RequestContext, AgentApiError> {
    REQUEST_CONTEXT.try_with(Clone::clone).map_err(|_| {
        AgentApiError::new(
            api::AgentApiErrorKind::Unauthenticated,
            "missing trusted request context",
        )
    })
}

pub fn request_principal() -> Result<PrincipalRef, AgentApiError> {
    let context = request_context()?;
    let principal = context.acting_principal();
    Ok(PrincipalRef {
        kind: match principal.kind {
            access::PrincipalKind::User => PrincipalKind::User,
            access::PrincipalKind::Service => PrincipalKind::ServiceAccount,
        },
        id: Some(principal.id.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn missing_context_fails_closed() {
        assert!(request_context().is_err());
        assert!(request_principal().is_err());
    }
}

#[cfg(test)]
mod isolation_tests {
    use super::*;
    fn context(id: u128) -> RequestContext {
        let principal = access::Principal {
            id: uuid::Uuid::from_u128(id),
            kind: access::PrincipalKind::User,
            status: access::PrincipalStatus::Active,
            display_name: "Test caller".into(),
            management_scope: access::AccessScope::Deployment,
            created_at_ms: 0,
        };
        RequestContext {
            rights: access::EffectiveAccess {
                principal: principal.clone(),
                scope: access::AccessScope::Deployment,
                roles: Default::default(),
                capabilities: Default::default(),
                policy_revision: 0,
            },
            authenticated_principal: principal,
            authentication: access::AuthenticationReference::ApiKey {
                key_prefix: format!("lsk_{id}"),
            },
            credential_scope: access::AccessScope::Deployment,
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_requests_do_not_share_context_or_spawned_task_authority() {
        let run = |id| {
            with_request_context(context(id), async move {
                tokio::task::yield_now().await;
                assert_eq!(
                    request_context().unwrap().acting_principal().id,
                    uuid::Uuid::from_u128(id)
                );
                assert!(
                    tokio::spawn(async { request_context() })
                        .await
                        .unwrap()
                        .is_err()
                );
            })
        };
        tokio::join!(run(1), run(2));
        assert!(request_context().is_err());
    }
}

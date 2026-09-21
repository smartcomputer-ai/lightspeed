//! Trusted request context installed by the authenticated transport boundary.
//! Missing context is an error: background work must carry explicit attribution.
use access::RequestContext;
use api::AgentApiError;
use auth::{PrincipalKind, PrincipalRef};
use tracing::Instrument as _;

tokio::task_local! {
    static REQUEST_CONTEXT: RequestContext;
}

pub async fn with_request_context<F: Future>(context: RequestContext, future: F) -> F::Output {
    let span = tracing::info_span!("request_authority",
        acting_principal = %context.acting_principal.id,
        authenticated_principal = %context.authenticated_principal.id,
        authentication = ?context.authentication,
        target_scope = ?context.target_scope);
    REQUEST_CONTEXT
        .scope(context, future)
        .instrument(span)
        .await
}

pub fn request_context() -> Result<RequestContext, AgentApiError> {
    REQUEST_CONTEXT
        .try_with(Clone::clone)
        .map_err(|_| AgentApiError::rejected("missing trusted request context"))
}

pub fn request_principal() -> Result<PrincipalRef, AgentApiError> {
    let principal = request_context()?.acting_principal;
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
            acting_principal: principal.clone(),
            authenticated_principal: principal,
            authentication: access::AuthenticationReference::ApiKey {
                key_prefix: format!("lsk_{id}"),
            },
            credential_scope: access::AccessScope::Deployment,
            target_scope: access::AccessScope::Deployment,
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_requests_do_not_share_context_or_spawned_task_authority() {
        let run = |id| {
            with_request_context(context(id), async move {
                tokio::task::yield_now().await;
                assert_eq!(
                    request_context().unwrap().acting_principal.id,
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

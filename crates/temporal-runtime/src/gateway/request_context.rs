//! Trusted request target and attribution, installed by the transport boundary.
use api::{AccessScope, AgentApiError, Attribution, CallerAccess};
use tracing::Instrument as _;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestContext {
    /// What the request addresses, independent of caller identity.
    pub scope: AccessScope,
    /// Configured component name; absent for unauthenticated or in-process work.
    pub token_name: Option<String>,
    pub actor: Option<String>,
}
impl RequestContext {
    pub fn local(scope: AccessScope) -> Self {
        Self {
            scope,
            token_name: None,
            actor: None,
        }
    }
    pub fn caller_access(&self) -> CallerAccess {
        CallerAccess {
            single: self.token_name.is_none(),
            token_name: self.token_name.clone(),
        }
    }
    pub fn attribution(&self) -> Attribution {
        match (&self.actor, &self.token_name) {
            (Some(id), _) => Attribution::Actor { id: id.clone() },
            (None, Some(name)) => Attribution::Token { name: name.clone() },
            (None, None) => Attribution::Local,
        }
    }
}

tokio::task_local! {
    static REQUEST_CONTEXT: RequestContext;
}

/// Runs the request under its trusted context.
pub async fn with_request_context<F: Future>(context: RequestContext, future: F) -> F::Output {
    let span = tracing::info_span!("request_authority",
        actor = ?context.actor,
        token_name = ?context.token_name,
        scope = ?context.scope);
    REQUEST_CONTEXT
        .scope(context, future)
        .instrument(span)
        .await
}

pub fn request_context() -> Result<RequestContext, AgentApiError> {
    REQUEST_CONTEXT.try_with(Clone::clone).map_err(|_| {
        AgentApiError::new(
            api::AgentApiErrorKind::Unauthenticated,
            "missing trusted request context",
        )
    })
}

/// Who the request acts as, for records it creates: its actor, else its token,
/// else local.
pub fn request_attribution() -> Result<Attribution, AgentApiError> {
    Ok(request_context()?.attribution())
}

/// Connection discovery does not load or require universe state.
pub fn connection_response(
    caller: CallerAccess,
    default_universe: Option<String>,
) -> api::ConnectionResponse {
    use api::*;
    ConnectionResponse {
        runtime: RuntimeKind::Runtime,
        contract_revision: HTTP_CONTRACT_REVISION.into(),
        default_universe,
        server_info: ServerInfo {
            name: "lightspeed-agent".to_owned(),
            version: format!("{}+{}", release_info::VERSION, release_info::GIT_SHA),
            git_sha: release_info::GIT_SHA.to_owned(),
            envd: EnvironmentDaemonInfo {
                version: release_info::VERSION.to_owned(),
                git_sha: release_info::GIT_SHA.to_owned(),
                protocol_version: environment_protocol::ENVD_PROTOCOL_VERSION,
                targets: release_info::envd_targets().map(str::to_owned).collect(),
            },
        },
        caller,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_requests_keep_their_own_attribution() {
        assert!(request_context().is_err());
        let run = |id: &str| {
            let id = id.to_owned();
            async move {
                let context = RequestContext {
                    scope: AccessScope::Deployment,
                    token_name: Some("platform".into()),
                    actor: Some(id.clone()),
                };
                with_request_context(context, async move {
                    tokio::task::yield_now().await;
                    assert_eq!(request_attribution().unwrap(), Attribution::Actor { id });
                    assert!(
                        tokio::spawn(async { request_context() })
                            .await
                            .unwrap()
                            .is_err()
                    );
                })
                .await
            }
        };
        tokio::join!(run("alice"), run("bob"));
        assert!(request_context().is_err());
    }
    #[test]
    fn attribution_falls_back_to_the_component_name() {
        let mut context = RequestContext::local(AccessScope::Deployment);
        assert_eq!(context.attribution(), Attribution::Local);
        context.token_name = Some("operator".into());
        assert_eq!(
            context.attribution(),
            Attribution::Token {
                name: "operator".into()
            }
        );
        assert_eq!(
            context.caller_access(),
            CallerAccess {
                single: false,
                token_name: Some("operator".into())
            }
        );
    }
}

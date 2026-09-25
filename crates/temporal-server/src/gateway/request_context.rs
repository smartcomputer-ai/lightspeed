//! Trusted request context installed by the authenticated transport boundary.
//! Missing context is an error: background work must carry explicit attribution.
use std::collections::BTreeSet;

use api::{AccessScope, AgentApiError, Attribution, MethodGroup};
use tracing::Instrument as _;

/// The key that authenticated a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyContext {
    /// Non-secret display prefix, for attribution and logs.
    pub prefix: String,
    pub groups: BTreeSet<MethodGroup>,
}

/// Trusted transport context, resolved once at the authenticated boundary
/// and never deserialized from input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestContext {
    /// What the request addresses: one universe, or the deployment.
    pub scope: AccessScope,
    /// The key that authenticated it; `None` for a local development request
    /// and for in-process callers, which have no key to hold groups.
    pub key: Option<KeyContext>,
    /// The actor a key allowed to assert one named for this request.
    pub actor: Option<String>,
}

impl RequestContext {
    /// A request without a key: local development, or an in-process call.
    pub fn local(scope: AccessScope) -> Self {
        Self {
            scope,
            key: None,
            actor: None,
        }
    }

    /// Whether the request's key may call `method`. `initialize` belongs to
    /// no group and is always allowed; a request without a key holds every
    /// group.
    pub fn permits(&self, method: &str) -> bool {
        match (&self.key, MethodGroup::of(method)) {
            (Some(key), Some(group)) => key.groups.contains(&group),
            _ => true,
        }
    }

    /// Who this request acts as: its actor, else its key, else local.
    pub fn attribution(&self) -> Attribution {
        match (&self.actor, &self.key) {
            (Some(id), _) => Attribution::Actor { id: id.clone() },
            (None, Some(key)) => Attribution::Key {
                prefix: key.prefix.clone(),
            },
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
        key = ?context.key.as_ref().map(|key| &key.prefix),
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

/// Who the request acts as, for records it creates: its actor, else its key,
/// else local.
pub fn request_attribution() -> Result<Attribution, AgentApiError> {
    Ok(request_context()?.attribution())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test(flavor = "current_thread")]
    async fn missing_context_fails_closed() {
        assert!(request_context().is_err());
        assert!(request_attribution().is_err());
    }

    fn context(id: u128) -> RequestContext {
        RequestContext {
            scope: AccessScope::Deployment,
            key: Some(KeyContext {
                prefix: format!("lsk_{id}"),
                groups: Default::default(),
            }),
            actor: Some(format!("actor-{id}")),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_requests_do_not_share_context_or_spawned_task_authority() {
        let run = |id| {
            with_request_context(context(id), async move {
                tokio::task::yield_now().await;
                assert_eq!(
                    request_context().unwrap().actor,
                    Some(format!("actor-{id}"))
                );
                assert_eq!(
                    request_attribution().unwrap(),
                    Attribution::Actor {
                        id: format!("actor-{id}")
                    }
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

    fn keyed(groups: &[MethodGroup], actor: Option<&str>) -> RequestContext {
        RequestContext {
            scope: AccessScope::Universe {
                universe_id: Uuid::from_u128(1),
            },
            key: Some(KeyContext {
                prefix: "lsk_abcdefgh".into(),
                groups: groups.iter().copied().collect(),
            }),
            actor: actor.map(str::to_owned),
        }
    }

    #[test]
    fn a_key_calls_only_its_groups_and_initialize() {
        let connector = keyed(&[MethodGroup::ChannelsInbound, MethodGroup::BlobsPut], None);
        assert!(connector.permits("channels/inbound/admit"));
        assert!(connector.permits("blobs/put"));
        assert!(connector.permits("initialize"));
        assert!(!connector.permits("blobs/read"));
        assert!(!connector.permits("session/read"));
        // A local request has no key and holds every group.
        assert!(
            RequestContext::local(AccessScope::Deployment).permits("deployment/universes/delete")
        );
    }

    #[test]
    fn attribution_prefers_the_actor_then_the_key() {
        assert_eq!(
            keyed(&[], Some("user-1")).attribution(),
            Attribution::Actor {
                id: "user-1".into()
            }
        );
        assert_eq!(
            keyed(&[], None).attribution(),
            Attribution::Key {
                prefix: "lsk_abcdefgh".into()
            }
        );
        assert_eq!(
            RequestContext::local(AccessScope::Deployment).attribution(),
            Attribution::Local
        );
    }
}

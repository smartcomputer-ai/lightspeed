//! Trusted request context installed by the authenticated transport boundary.
//! Missing context is an error: background work must carry explicit attribution.
use std::collections::BTreeSet;

use api::{AccessScope, AgentApiError, Attribution, CallerAccess, MethodGroup};
use tracing::Instrument as _;

/// The key that authenticated a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyContext {
    pub scope: AccessScope,
    /// The key's id, unique among the keys of `scope`. It outlives the
    /// key's secret, so attribution and logs name the key by it.
    pub key_id: String,
    /// Non-secret display prefix of the secret the request presented. It
    /// changes whenever the secret is replaced, so nothing durable holds it.
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

    /// What `initialize` reports: the key's groups, or every group for a
    /// request without a key, matching [`Self::permits`].
    pub fn caller_access(&self) -> CallerAccess {
        match &self.key {
            Some(key) => CallerAccess {
                scope: key.scope,
                single: false,
                key_id: Some(key.key_id.clone()),
                key_prefix: Some(key.prefix.clone()),
                groups: key.groups.iter().copied().collect(),
            },
            None => CallerAccess {
                scope: self.scope,
                single: true,
                key_id: None,
                key_prefix: None,
                groups: MethodGroup::ALL.to_vec(),
            },
        }
    }

    /// Who this request acts as: its actor, else its key, else local. A
    /// key is named by its id and its own scope, whichever universe the
    /// request addresses.
    pub fn attribution(&self) -> Attribution {
        match (&self.actor, &self.key) {
            (Some(id), _) => Attribution::Actor { id: id.clone() },
            (None, Some(key)) => Attribution::key(key.key_id.clone(), key.scope),
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
        key_id = ?context.key.as_ref().map(|key| &key.key_id),
        key_scope = ?context.key.as_ref().map(|key| key.scope.kind().as_str()),
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

/// Connection discovery does not load or require universe state.
pub fn initialize_response(caller: CallerAccess) -> api::AgentApiOutcome<api::InitializeResponse> {
    use api::*;
    AgentApiOutcome::new(InitializeResponse {
        protocol_version: api::PROTOCOL_VERSION.to_owned(),
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
        capabilities: ServerCapabilities {
            notifications: false,
            history_read: true,
            event_log: true,
            local_execution: false,
        },
        caller,
    })
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
                scope: AccessScope::Deployment,
                key_id: format!("key-{id}"),
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
                scope: AccessScope::Deployment,
                key_id: "platform".into(),
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
    fn ordinary_session_keys_cannot_delete_or_schedule_deletion() {
        let ordinary = keyed(&[MethodGroup::Session], None);
        assert!(ordinary.permits("session/close"));
        assert!(!ordinary.permits("session/delete"));
        assert!(!ordinary.permits("session/retention/put"));
        assert!(!ordinary.permits("deployment/sessions/purge"));
        let destructive = keyed(&[MethodGroup::SessionDelete], None);
        assert!(destructive.permits("session/delete"));
        assert!(destructive.permits("session/retention/put"));
        assert!(!destructive.permits("deployment/sessions/purge"));
    }

    #[test]
    fn caller_access_reports_what_permits_allows() {
        let connector = keyed(&[MethodGroup::ChannelsInbound, MethodGroup::BlobsPut], None);
        assert_eq!(
            connector.caller_access(),
            CallerAccess {
                scope: AccessScope::Deployment,
                single: false,
                key_id: Some("platform".into()),
                key_prefix: Some("lsk_abcdefgh".into()),
                groups: vec![MethodGroup::BlobsPut, MethodGroup::ChannelsInbound],
            }
        );
        let local = RequestContext::local(AccessScope::Deployment).caller_access();
        assert_eq!(local.key_id, None);
        assert_eq!(local.key_prefix, None);
        assert_eq!(local.groups, MethodGroup::ALL.to_vec());
    }

    #[test]
    fn attribution_prefers_the_actor_then_the_key() {
        assert_eq!(
            keyed(&[], Some("user-1")).attribution(),
            Attribution::Actor {
                id: "user-1".into()
            }
        );
        // The key is a deployment key acting in a universe: it is named by
        // its own scope, not by the universe the request addresses.
        assert_eq!(
            keyed(&[], None).attribution(),
            Attribution::Key {
                key_id: "platform".into(),
                scope_kind: api::AccessScopeKind::Deployment,
            }
        );
        assert_eq!(
            RequestContext::local(AccessScope::Deployment).attribution(),
            Attribution::Local
        );
    }

    #[test]
    fn a_key_is_attributed_the_same_before_and_after_its_secret_is_replaced() {
        let universe = AccessScope::Universe {
            universe_id: Uuid::from_u128(1),
        };
        let holding = |prefix: &str| RequestContext {
            scope: universe,
            key: Some(KeyContext {
                scope: universe,
                key_id: "ci-bot".into(),
                prefix: prefix.into(),
                groups: Default::default(),
            }),
            actor: None,
        };
        let before = holding("lsk_abcdefgh").attribution();
        assert_eq!(
            before,
            Attribution::Key {
                key_id: "ci-bot".into(),
                scope_kind: api::AccessScopeKind::Universe,
            }
        );
        assert_eq!(holding("lsk_ijklmnop").attribution(), before);
        // The prefix is still what `initialize` reports for the secret in use.
        assert_eq!(
            holding("lsk_ijklmnop")
                .caller_access()
                .key_prefix
                .as_deref(),
            Some("lsk_ijklmnop")
        );
    }
}

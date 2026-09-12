//! Internal environment lookup and liveness policy.
//!
//! This is a shared runtime service, not a provider/plugin extension seam.

use std::sync::Arc;

use environments::{
    EnvironmentAccessPolicy, EnvironmentId, EnvironmentProviderStore, EnvironmentRecord,
    EnvironmentRegistryError, EnvironmentSource, EnvironmentStatus, EnvironmentStore,
    ListEnvironments, PowerState, SetEnvironmentPower,
};
use store_pg::PgStore;
use thiserror::Error;

#[derive(Clone)]
pub(crate) struct EnvironmentResolver {
    environments: Arc<dyn EnvironmentStore>,
    providers: Arc<dyn EnvironmentProviderStore>,
    gateway: Option<crate::environment_gateway::EnvironmentGatewayClientConfig>,
    universe_id: uuid::Uuid,
}

impl EnvironmentResolver {
    pub(crate) fn universe_id(&self) -> uuid::Uuid {
        self.universe_id
    }
    pub(crate) fn new(
        environments: Arc<dyn EnvironmentStore>,
        providers: Arc<dyn EnvironmentProviderStore>,
    ) -> Self {
        Self {
            environments,
            providers,
            gateway: None,
            universe_id: uuid::Uuid::nil(),
        }
    }

    pub(crate) fn from_pg_store(store: Arc<PgStore>) -> Self {
        let universe_id = store.config().universe_id;
        let mut resolver = Self::new(store.clone(), store);
        resolver.universe_id = universe_id;
        resolver
    }

    pub(crate) fn with_gateway(
        mut self,
        gateway: crate::environment_gateway::EnvironmentGatewayClientConfig,
    ) -> Self {
        self.gateway = Some(gateway);
        self
    }

    pub(crate) async fn list_allowed(
        &self,
        policy: &EnvironmentAccessPolicy,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentResolveError> {
        let mut environments = self
            .environments
            .list_environments(ListEnvironments::default())
            .await?;
        environments.retain(|environment| policy.allows(environment));
        Ok(environments)
    }

    pub(crate) async fn read_allowed(
        &self,
        environment_id: &EnvironmentId,
        policy: &EnvironmentAccessPolicy,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self.environments.read_environment(environment_id).await?;
        if !policy.allows(&environment) {
            return Err(EnvironmentResolveError::NotAllowed {
                environment_id: environment.environment_id.to_string(),
                reason: policy.refusal(&environment),
            });
        }
        Ok(environment)
    }

    /// Validate selection using registry state only. Selecting or reselecting
    /// an environment never changes power or proves data-plane reachability.
    pub(crate) async fn selectable(
        &self,
        environment_id: &EnvironmentId,
        policy: &EnvironmentAccessPolicy,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self.read_allowed(environment_id, policy).await?;
        match environment.status {
            EnvironmentStatus::Failed => Err(EnvironmentResolveError::Failed {
                environment_id: environment.environment_id.to_string(),
                message: environment
                    .metadata
                    .get(LIFECYCLE_ERROR_METADATA_KEY)
                    .cloned()
                    .unwrap_or_else(|| "environment provisioning failed".to_owned()),
            }),
            EnvironmentStatus::Closing | EnvironmentStatus::Closed => {
                Err(EnvironmentResolveError::Closed {
                    environment_id: environment.environment_id.to_string(),
                })
            }
            _ => Ok(environment),
        }
    }

    /// Check readiness for actual use, requesting wake-up where supported and
    /// probing the data route. Selection itself uses only registry validation.
    pub(crate) async fn ready_for_use(
        &self,
        environment_id: &EnvironmentId,
        policy: &EnvironmentAccessPolicy,
        now_ms: i64,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self
            .resolve_for_connection(environment_id, policy, now_ms)
            .await?;
        if let Some(gateway) = &self.gateway {
            let connection = gateway.connection_for(self.universe_id, &environment);
            if let Ok(mut client) = environment_client::EnvironmentDataClient::connect(
                &connection.endpoint,
                gateway.connect_options("lightspeed-environment-readiness"),
            )
            .await
            {
                let _ = client.close().await;
                return Ok(environment);
            }
        }
        Err(EnvironmentResolveError::EnvironmentUnavailable {
            environment_id: environment.environment_id.as_str().to_owned(),
            status: "environment endpoint is not reachable".to_owned(),
        })
    }

    /// Validate lifecycle and policy immediately before opening a real
    /// data-plane connection. Unlike [`Self::ready_for_use`], this does not open
    /// a second connection merely to prove reachability.
    pub(crate) async fn resolve_for_connection(
        &self,
        environment_id: &EnvironmentId,
        policy: &EnvironmentAccessPolicy,
        now_ms: i64,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self.selectable(environment_id, policy).await?;
        if let Some(provider_id) = environment.provider_id() {
            self.providers.read_provider(provider_id).await?;
        }
        match environment.status {
            EnvironmentStatus::Provisioning | EnvironmentStatus::Booting => {
                return Err(EnvironmentResolveError::NotReady {
                    environment_id: environment.environment_id.as_str().to_owned(),
                    status: environment.status,
                });
            }
            EnvironmentStatus::Paused
            | EnvironmentStatus::Suspended
            | EnvironmentStatus::Offline
                if wake_on_use_applies(&environment) =>
            {
                if environment.desired_power != PowerState::Running {
                    self.environments
                        .set_environment_power(SetEnvironmentPower {
                            environment_id: environment.environment_id.clone(),
                            desired_power: PowerState::Running,
                            updated_at_ms: now_ms.max(0),
                        })
                        .await?;
                }
                return Err(EnvironmentResolveError::NotReady {
                    environment_id: environment.environment_id.as_str().to_owned(),
                    status: environment.status,
                });
            }
            EnvironmentStatus::Ready if environment.desired_power != PowerState::Running => {
                // Use cancels a pending power-down: the idle reaper has asked
                // for a lower power state but the reconciler has not converged
                // yet. Keeping the intent at `running` stops the reconciler
                // from freezing the environment under the call about to
                // start; the reaper re-evaluates from the daemon's next idle
                // report. Sessions are not consulted — any use counts.
                return self
                    .environments
                    .set_environment_power(SetEnvironmentPower {
                        environment_id: environment.environment_id.clone(),
                        desired_power: PowerState::Running,
                        updated_at_ms: now_ms.max(0),
                    })
                    .await
                    .map_err(EnvironmentResolveError::from);
            }
            _ => {}
        }
        Ok(environment)
    }
}

#[derive(Debug, Error)]
pub(crate) enum EnvironmentResolveError {
    #[error(transparent)]
    Store(#[from] EnvironmentRegistryError),

    #[error("environment {environment_id} is not allowed by session config: {reason}")]
    NotAllowed {
        environment_id: String,
        reason: String,
    },

    #[error("environment is unavailable: {environment_id} ({status})")]
    EnvironmentUnavailable {
        environment_id: String,
        status: String,
    },

    /// The environment exists and is being provisioned or booted; it is not
    /// yet reachable but selecting it is valid intent.
    #[error("environment is not ready yet: {environment_id} ({status:?})")]
    NotReady {
        environment_id: String,
        status: EnvironmentStatus,
    },

    #[error("environment failed to provision: {environment_id}: {message}")]
    Failed {
        environment_id: String,
        message: String,
    },

    #[error("environment is closed: {environment_id}")]
    Closed { environment_id: String },
}

/// Metadata key under which the lifecycle reconciler records the provider's
/// failure message on a `failed` environment.
pub(crate) const LIFECYCLE_ERROR_METADATA_KEY: &str = "lifecycleError";

/// A powered-down provisioned environment wakes on use when its provider
/// reported that it can be moved back to `running`. External environments and
/// providers without power control keep the reachability probe.
pub(crate) fn wake_on_use_applies(environment: &EnvironmentRecord) -> bool {
    matches!(environment.source, EnvironmentSource::Provisioned { .. })
        && environment.status.is_powered_down()
        && environment
            .incarnation
            .power_states
            .contains(&PowerState::Running)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use environment_protocol::shared::{EnvironmentTransport, ProviderTargetId};
    use environments::{
        CreateEnvironment, EnvironmentConnectionSpec, EnvironmentIncarnationId,
        EnvironmentProviderBindingId, EnvironmentProviderBindingStatus,
        EnvironmentProviderBindingStore, EnvironmentProviderId, EnvironmentProvisionRequestId,
        EnvironmentStatus, EnvironmentTemplateId, ObserveProvisionedEnvironment,
        PutEnvironmentProvider, PutEnvironmentProviderBinding,
    };

    use super::*;

    async fn resolver() -> (EnvironmentResolver, EnvironmentId) {
        let store = Arc::new(environments::InMemoryEnvironmentRegistryStore::new());
        let provider_id = EnvironmentProviderId::new("allowed");
        store
            .put_provider(PutEnvironmentProvider {
                provider_id: provider_id.clone(),
                display_name: None,
                controller_connection: EnvironmentConnectionSpec::new(
                    "http://controller.test",
                    EnvironmentTransport::Http,
                ),
                metadata: BTreeMap::new(),
                updated_at_ms: 10,
            })
            .await
            .expect("provider");
        store
            .put_provider_binding(PutEnvironmentProviderBinding {
                universe_id: store.universe_id(),
                binding_id: EnvironmentProviderBindingId::new("primary"),
                provider_id: provider_id.clone(),
                status: EnvironmentProviderBindingStatus::Enabled,
                expected_revision: None,
                metadata: BTreeMap::new(),
                updated_at_ms: 10,
            })
            .await
            .expect("binding");
        let environment_id = EnvironmentId::new("environment-1");
        let target_id = ProviderTargetId::new("target-1");
        store
            .create_environment(CreateEnvironment {
                request_id: EnvironmentProvisionRequestId::new("request-1"),
                environment_id: environment_id.clone(),
                incarnation_id: EnvironmentIncarnationId::new("incarnation-1"),
                binding_id: EnvironmentProviderBindingId::new("primary"),
                template_id: EnvironmentTemplateId::new("test-template"),
                display_name: None,
                metadata: BTreeMap::new(),

                idle_policy: None,
                created_at_ms: 10,
            })
            .await
            .expect("create environment");
        store
            .observe_provisioned_environment(ObserveProvisionedEnvironment {
                environment_id: environment_id.clone(),
                provider_target_id: target_id.clone(),
                status: EnvironmentStatus::Offline,
                power_states: Vec::new(),
                observed_at_ms: 10,
            })
            .await
            .expect("environment");
        (
            EnvironmentResolver::new(store.clone(), store),
            environment_id,
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn environment_skills_idle_discovery_reuses_observations_and_never_wakes() {
        use engine::{
            CoreAgentCommand,
            storage::{BlobStore, InMemoryBlobStore},
        };
        use futures_util::{SinkExt, StreamExt};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use tools::skills::environment::{EnvironmentSkillAvailability, EnvironmentSkillCatalog};
        let (resolver, environment_id) = resolver().await;
        let store = resolver.environments.clone();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".agents/skills/review")).unwrap();
        let doc = "---\nname: review\ndescription: Review code.\n---\nbody";
        let skill_path = root.join(".agents/skills/review/SKILL.md");
        std::fs::write(&skill_path, doc).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway = crate::environment_gateway::EnvironmentGatewayClientConfig::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "test",
        );
        let scans = Arc::new(AtomicUsize::new(0));
        let unchanged = Arc::new(AtomicUsize::new(0));
        let supported = Arc::new(AtomicBool::new(true));
        let stall = Arc::new(AtomicBool::new(false));
        let task = {
            let scans = scans.clone();
            let unchanged = unchanged.clone();
            let supported = supported.clone();
            let stall = stall.clone();
            let root = root.clone();
            tokio::spawn(async move {
                loop {
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
                    while let Some(Ok(message)) = socket.next().await {
                        let Ok(text) = message.to_text() else {
                            continue;
                        };
                        let Ok(request) = serde_json::from_str::<serde_json::Value>(text) else {
                            continue;
                        };
                        let Some(id) = request.get("id") else {
                            continue;
                        };
                        if stall.load(Ordering::SeqCst) {
                            std::future::pending::<()>().await;
                        }
                        let result = match request["method"].as_str().unwrap() {
                            "initialize" => {
                                serde_json::json!({ "protocolVersion": environment_protocol::shared::CURRENT_PROTOCOL_VERSION, "connectionId": "test", "capabilities": {"filesystemRead": true, "filesystemScan": supported.load(Ordering::SeqCst)}, "defaultCwd": root, "homeDirectory": root, "implementation": {"name": "test", "version": "1"} })
                            }
                            "fs/getMetadata" => {
                                let fs = environment_daemon::filesystem::LocalFileSystem::new(
                                    root.clone(),
                                    root.clone(),
                                    false,
                                );
                                serde_json::to_value(
                                    fs.get_metadata(
                                        serde_json::from_value(request["params"].clone()).unwrap(),
                                    )
                                    .await
                                    .unwrap(),
                                )
                                .unwrap()
                            }
                            "fs/scan" => {
                                scans.fetch_add(1, Ordering::SeqCst);
                                let fs = environment_daemon::filesystem::LocalFileSystem::new(
                                    root.clone(),
                                    root.clone(),
                                    false,
                                );
                                let result = fs
                                    .scan(
                                        serde_json::from_value(request["params"].clone()).unwrap(),
                                    )
                                    .await
                                    .unwrap();
                                if result.unchanged {
                                    unchanged.fetch_add(1, Ordering::SeqCst);
                                }
                                serde_json::to_value(result).unwrap()
                            }
                            other => panic!("unexpected discovery RPC: {other}"),
                        };
                        if socket
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            })
        };
        let blobs = InMemoryBlobStore::new();
        let session_id = engine::SessionId::new(uuid::Uuid::new_v4().to_string());
        let feature = engine::EnvironmentsFeature {
            skills: Some(Default::default()),
            ..Default::default()
        };
        let refresh = |current| {
            crate::environment_skills::refresh(
                &blobs,
                Some(&resolver),
                Some(&gateway),
                &session_id,
                Some(&feature),
                Some(&environment_id),
                current,
            )
        };
        let entry = |command| match command {
            Some(CoreAgentCommand::UpsertContext { entry, .. }) => entry,
            other => panic!("expected publication: {other:?}"),
        };
        // Offline inspection never changes desired power and never opens a data route.
        let before = store.read_environment(&environment_id).await.unwrap();
        let unavailable = entry(refresh(None).await.unwrap());
        assert_eq!(scans.load(Ordering::SeqCst), 0);
        assert_eq!(
            store.read_environment(&environment_id).await.unwrap(),
            before
        );
        assert!(refresh(Some(&unavailable)).await.unwrap().is_none());
        store
            .observe_provisioned_environment(ObserveProvisionedEnvironment {
                environment_id: environment_id.clone(),
                provider_target_id: ProviderTargetId::new("target-1"),
                status: EnvironmentStatus::Ready,
                power_states: vec![PowerState::Running, PowerState::Paused],
                observed_at_ms: 20,
            })
            .await
            .unwrap();
        let available = entry(refresh(Some(&unavailable)).await.unwrap());
        let catalog: EnvironmentSkillCatalog = serde_json::from_slice(
            &blobs
                .read_bytes(available.provenance_ref.as_ref().unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            catalog.availability,
            EnvironmentSkillAvailability::Available
        );
        assert_eq!(catalog.skills.len(), 1);
        assert!(refresh(Some(&available)).await.unwrap().is_none());
        assert_eq!(unchanged.load(Ordering::SeqCst), 1);
        std::fs::write(&skill_path, doc.replace("body", "edit")).unwrap();
        assert!(
            refresh(Some(&available)).await.unwrap().is_none(),
            "body changes do not publish"
        );
        std::fs::write(&skill_path, doc.replace("Review code.", "Review changes.")).unwrap();
        let edited = entry(refresh(Some(&available)).await.unwrap());
        assert_ne!(edited.content, available.content);
        // An incomplete scan reports unavailable and removes obsolete catalog paths.
        std::fs::write(&skill_path, vec![b'x'; 65537]).unwrap();
        let stale = entry(refresh(Some(&edited)).await.unwrap());
        let catalog: EnvironmentSkillCatalog = serde_json::from_slice(
            &blobs
                .read_bytes(stale.provenance_ref.as_ref().unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            catalog.availability,
            EnvironmentSkillAvailability::Unavailable
        );
        assert!(catalog.skills.is_empty());
        assert!(refresh(Some(&stale)).await.unwrap().is_none());
        // Missing fs/scan is explicit unavailable discovery, with no RPC fallback.
        supported.store(false, Ordering::SeqCst);
        let before = scans.load(Ordering::SeqCst);
        let unsupported = entry(refresh(Some(&stale)).await.unwrap());
        assert!(refresh(Some(&unsupported)).await.unwrap().is_none());
        assert_eq!(scans.load(Ordering::SeqCst), before);
        supported.store(true, Ordering::SeqCst);
        std::fs::remove_file(&skill_path).unwrap();
        let empty = entry(refresh(Some(&stale)).await.unwrap());
        let catalog: EnvironmentSkillCatalog = serde_json::from_slice(
            &blobs
                .read_bytes(empty.provenance_ref.as_ref().unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(catalog.skills.is_empty());
        assert_eq!(
            catalog.availability,
            EnvironmentSkillAvailability::Available
        );
        // Deselection removes only this catalog key.
        let cleared = crate::environment_skills::refresh(
            &blobs,
            Some(&resolver),
            Some(&gateway),
            &session_id,
            Some(&feature),
            None,
            Some(&empty),
        )
        .await
        .unwrap();
        assert!(
            matches!(cleared, Some(CoreAgentCommand::RemoveContext { key, .. }) if key.as_str() == "runtime.catalog.skills.environment")
        );
        let mut denied = feature.clone();
        denied.providers = Some(vec!["not-granted".into()]);
        let denied_entry = entry(
            crate::environment_skills::refresh(
                &blobs,
                Some(&resolver),
                Some(&gateway),
                &session_id,
                Some(&denied),
                Some(&environment_id),
                Some(&available),
            )
            .await
            .unwrap(),
        );
        let denied_catalog: EnvironmentSkillCatalog = serde_json::from_slice(
            &blobs
                .read_bytes(denied_entry.provenance_ref.as_ref().unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            denied_catalog.availability,
            EnvironmentSkillAvailability::Unavailable
        );
        assert!(
            denied_catalog.skills.is_empty(),
            "a revoked domain grant cannot retain advertised paths"
        );
        stall.store(true, Ordering::SeqCst);
        let start = std::time::Instant::now();
        let timed_out = entry(refresh(Some(&available)).await.unwrap());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(6),
            "unresponsive endpoint must remain bounded"
        );
        let timed_out_catalog: EnvironmentSkillCatalog = serde_json::from_slice(
            &blobs
                .read_bytes(timed_out.provenance_ref.as_ref().unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            timed_out_catalog.availability,
            EnvironmentSkillAvailability::Unavailable
        );
        task.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_filter_applies_to_list_read_and_selection() {
        let (resolver, environment_id) = resolver().await;
        let denied =
            EnvironmentAccessPolicy::new(Some(vec!["other".to_owned()]), None::<Vec<String>>);
        assert!(resolver.list_allowed(&denied).await.unwrap().is_empty());
        assert!(matches!(
            resolver.read_allowed(&environment_id, &denied).await,
            Err(EnvironmentResolveError::NotAllowed { .. })
        ));
        assert!(matches!(
            resolver.selectable(&environment_id, &denied).await,
            Err(EnvironmentResolveError::NotAllowed { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn offline_environment_without_gateway_is_unavailable_but_readable() {
        let (resolver, environment_id) = resolver().await;
        assert!(
            resolver
                .read_allowed(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL)
                .await
                .is_ok()
        );
        assert!(
            resolver
                .resolve_for_connection(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL, 111)
                .await
                .is_ok(),
            "execution resolution should defer reachability to the real connection"
        );
        assert!(matches!(
            resolver
                .ready_for_use(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL, 111)
                .await,
            Err(EnvironmentResolveError::EnvironmentUnavailable { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn selection_preserves_power_and_needs_no_gateway_in_any_nonterminal_state() {
        let (resolver, environment_id) = resolver().await;
        let store = resolver.environments.clone();
        store
            .set_environment_power(SetEnvironmentPower {
                environment_id: environment_id.clone(),
                desired_power: PowerState::Paused,
                updated_at_ms: 20,
            })
            .await
            .unwrap();
        for status in [
            EnvironmentStatus::Provisioning,
            EnvironmentStatus::Booting,
            EnvironmentStatus::Ready,
            EnvironmentStatus::Paused,
            EnvironmentStatus::Suspended,
            EnvironmentStatus::Offline,
            EnvironmentStatus::Unknown,
        ] {
            store
                .observe_provisioned_environment(ObserveProvisionedEnvironment {
                    environment_id: environment_id.clone(),
                    provider_target_id: ProviderTargetId::new("target-1"),
                    status,
                    power_states: vec![PowerState::Running, PowerState::Paused],
                    observed_at_ms: 30,
                })
                .await
                .unwrap();
            let before = store.read_environment(&environment_id).await.unwrap();
            for _ in 0..2 {
                let selected = resolver
                    .selectable(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL)
                    .await
                    .expect("selection requires only valid registry state");
                assert_eq!(selected, before);
                assert_eq!(
                    store.read_environment(&environment_id).await.unwrap(),
                    before
                );
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reselection_checks_current_access_and_terminal_status() {
        let (resolver, environment_id) = resolver().await;
        let store = resolver.environments.clone();
        resolver
            .selectable(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL)
            .await
            .unwrap();
        let denied = EnvironmentAccessPolicy::new(Some(vec!["other".into()]), None::<Vec<String>>);
        assert!(matches!(
            resolver.selectable(&environment_id, &denied).await,
            Err(EnvironmentResolveError::NotAllowed { .. })
        ));
        assert!(matches!(
            resolver
                .selectable(
                    &EnvironmentId::new("missing"),
                    &EnvironmentAccessPolicy::ALLOW_ALL
                )
                .await,
            Err(EnvironmentResolveError::Store(
                EnvironmentRegistryError::NotFound { .. }
            ))
        ));
        store
            .fail_environment_lifecycle(environments::FailEnvironmentLifecycle {
                environment_id: environment_id.clone(),
                message: "no capacity".into(),
                observed_at_ms: 40,
            })
            .await
            .unwrap();
        assert!(matches!(
            resolver
                .selectable(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL)
                .await,
            Err(EnvironmentResolveError::Failed { .. })
        ));
        store
            .begin_close_environment(environments::BeginCloseEnvironment {
                environment_id: environment_id.clone(),
                updated_at_ms: 50,
            })
            .await
            .unwrap();
        assert!(matches!(
            resolver
                .selectable(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL)
                .await,
            Err(EnvironmentResolveError::Closed { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn use_cancels_a_pending_power_down() {
        let (resolver, environment_id) = resolver().await;
        let store = resolver.environments.clone();
        store
            .observe_provisioned_environment(ObserveProvisionedEnvironment {
                environment_id: environment_id.clone(),
                provider_target_id: ProviderTargetId::new("target-1"),
                status: EnvironmentStatus::Ready,
                power_states: vec![PowerState::Running, PowerState::Paused],
                observed_at_ms: 20,
            })
            .await
            .expect("observe");
        // The reaper decided to pause; the reconciler has not acted yet.
        store
            .set_environment_power(SetEnvironmentPower {
                environment_id: environment_id.clone(),
                desired_power: PowerState::Paused,
                updated_at_ms: 21,
            })
            .await
            .expect("pause intent");

        let resolved = resolver
            .resolve_for_connection(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL, 30)
            .await
            .expect("a ready environment resolves for use");
        assert_eq!(resolved.status, EnvironmentStatus::Ready);
        assert_eq!(resolved.desired_power, PowerState::Running);
        let stored = store.read_environment(&environment_id).await.expect("read");
        assert_eq!(stored.desired_power, PowerState::Running);
        assert!(!stored.power_diverges());
        assert_eq!(stored.updated_at_ms, 30);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn powered_down_environment_with_power_control_wakes_on_use() {
        let (resolver, environment_id) = resolver().await;
        let store = resolver.environments.clone();
        let observe = |status: EnvironmentStatus, at: i64| {
            let store = store.clone();
            let environment_id = environment_id.clone();
            async move {
                store
                    .observe_provisioned_environment(ObserveProvisionedEnvironment {
                        environment_id,
                        provider_target_id: ProviderTargetId::new("target-1"),
                        status,
                        power_states: vec![PowerState::Running, PowerState::Paused],
                        observed_at_ms: at,
                    })
                    .await
                    .expect("observe");
            }
        };
        observe(EnvironmentStatus::Ready, 20).await;
        store
            .set_environment_power(SetEnvironmentPower {
                environment_id: environment_id.clone(),
                desired_power: PowerState::Paused,
                updated_at_ms: 21,
            })
            .await
            .expect("pause intent");
        observe(EnvironmentStatus::Paused, 22).await;

        // Using a paused environment requests a wake and reports it as
        // not ready instead of probing an unreachable daemon.
        assert!(matches!(
            resolver
                .ready_for_use(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL, 30)
                .await,
            Err(EnvironmentResolveError::NotReady {
                status: EnvironmentStatus::Paused,
                ..
            })
        ));
        let woken = store.read_environment(&environment_id).await.expect("read");
        assert_eq!(woken.desired_power, PowerState::Running);
        assert!(woken.power_diverges());
        // Activation admits it as intent.
        let record = resolver
            .selectable(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL)
            .await
            .expect("activation admits a paused environment");
        assert_eq!(record.status, EnvironmentStatus::Paused);

        // Once the provider observed it running again the ordinary probe
        // path applies (no gateway here → unavailable, not NotReady).
        observe(EnvironmentStatus::Ready, 40).await;
        assert!(matches!(
            resolver
                .ready_for_use(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL, 50)
                .await,
            Err(EnvironmentResolveError::EnvironmentUnavailable { .. })
        ));

        // A stopped environment whose provider offers no power control keeps
        // the old behaviour: no wake, plain unavailability.
        store
            .observe_provisioned_environment(ObserveProvisionedEnvironment {
                environment_id: environment_id.clone(),
                provider_target_id: ProviderTargetId::new("target-1"),
                status: EnvironmentStatus::Offline,
                power_states: Vec::new(),
                observed_at_ms: 60,
            })
            .await
            .expect("observe offline without power control");
        assert!(matches!(
            resolver
                .ready_for_use(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL, 70)
                .await,
            Err(EnvironmentResolveError::EnvironmentUnavailable { .. })
        ));
        assert_eq!(
            store
                .read_environment(&environment_id)
                .await
                .expect("read")
                .desired_power,
            PowerState::Running
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn readiness_is_status_aware() {
        let (resolver, environment_id) = resolver().await;
        let store = resolver.environments.clone();
        let observe = |status: EnvironmentStatus| {
            let store = store.clone();
            let environment_id = environment_id.clone();
            async move {
                store
                    .observe_provisioned_environment(ObserveProvisionedEnvironment {
                        environment_id,
                        provider_target_id: ProviderTargetId::new("target-1"),
                        status,
                        power_states: Vec::new(),
                        observed_at_ms: 20,
                    })
                    .await
                    .expect("observe");
            }
        };

        // A provisioning/booting environment is admitted as intent without a
        // probe and reported as not ready.
        observe(EnvironmentStatus::Provisioning).await;
        assert!(matches!(
            resolver
                .ready_for_use(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL, 30)
                .await,
            Err(EnvironmentResolveError::NotReady {
                status: EnvironmentStatus::Provisioning,
                ..
            })
        ));
        observe(EnvironmentStatus::Booting).await;
        assert!(matches!(
            resolver
                .ready_for_use(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL, 30)
                .await,
            Err(EnvironmentResolveError::NotReady {
                status: EnvironmentStatus::Booting,
                ..
            })
        ));
        let record = resolver
            .selectable(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL)
            .await
            .expect("activation admits a booting environment");
        assert_eq!(record.status, EnvironmentStatus::Booting);

        store
            .fail_environment_lifecycle(environments::FailEnvironmentLifecycle {
                environment_id: environment_id.clone(),
                message: "no capacity".to_owned(),
                observed_at_ms: 40,
            })
            .await
            .expect("fail");
        assert!(matches!(
            resolver.ready_for_use(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL, 50).await,
            Err(EnvironmentResolveError::Failed { message, .. }) if message == "no capacity"
        ));

        store
            .begin_close_environment(environments::BeginCloseEnvironment {
                environment_id: environment_id.clone(),
                updated_at_ms: 60,
            })
            .await
            .expect("close");
        assert!(matches!(
            resolver
                .ready_for_use(&environment_id, &EnvironmentAccessPolicy::ALLOW_ALL, 70)
                .await,
            Err(EnvironmentResolveError::Closed { .. })
        ));
    }
}

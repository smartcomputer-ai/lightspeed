//! Internal environment lookup and liveness policy.
//!
//! This is a shared runtime service, not a provider/plugin extension seam.

use std::sync::Arc;

use environment_client::{ClientError, DataClient};
use environments::{
    EnvironmentId, EnvironmentProviderStore, EnvironmentRecord, EnvironmentRegistryError,
    EnvironmentSource, EnvironmentStatus, EnvironmentStore, PowerState, SetEnvironmentPower,
};
use store_pg::PgStore;
use thiserror::Error;

#[derive(Clone)]
pub(crate) struct EnvironmentResolver {
    environments: Arc<dyn EnvironmentStore>,
    providers: Arc<dyn EnvironmentProviderStore>,
    gateway: Option<crate::environments::gateway::EnvironmentGatewayClientConfig>,
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
        gateway: crate::environments::gateway::EnvironmentGatewayClientConfig,
    ) -> Self {
        self.gateway = Some(gateway);
        self
    }

    /// The registry record. Whether a session may use the environment is a
    /// membership check against its attachment list, made by the caller.
    pub(crate) async fn read(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        Ok(self.environments.read_environment(environment_id).await?)
    }

    /// Validate selection using registry state only. Selecting or reselecting
    /// an environment never changes power or proves data-plane reachability.
    pub(crate) async fn selectable(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self.read(environment_id).await?;
        match environment.status {
            EnvironmentStatus::Failed => Err(EnvironmentResolveError::Failed {
                environment_id: environment.environment_id.to_string(),
                message: environment
                    .lifecycle_error
                    .as_ref()
                    .map(|error| error.message.clone())
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
    /// running the data-plane handshake over the route. A daemon of another
    /// protocol version is a typed blocker, never a Ready environment whose
    /// calls all fail. Selection itself uses only registry validation.
    pub(crate) async fn ready_for_use(
        &self,
        environment_id: &EnvironmentId,
        now_ms: i64,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self.resolve_for_connection(environment_id, now_ms).await?;
        let unavailable = |status: String| EnvironmentResolveError::EnvironmentUnavailable {
            environment_id: environment.environment_id.as_str().to_owned(),
            status,
        };
        let Some(gateway) = &self.gateway else {
            return Err(unavailable(
                "environment endpoint is not reachable".to_owned(),
            ));
        };
        let endpoint = gateway.endpoint_for(self.universe_id, &environment);
        match DataClient::connect_initialized(&endpoint, gateway.connect_options(READINESS_CLIENT))
            .await
        {
            Ok((client, _)) => {
                let _ = client.close().await;
                Ok(environment)
            }
            Err(ClientError::VersionMismatch { local, remote, .. }) => {
                Err(EnvironmentResolveError::VersionMismatch {
                    environment_id: environment.environment_id.as_str().to_owned(),
                    local,
                    remote,
                })
            }
            Err(error) => Err(unavailable(format!(
                "environment endpoint is not reachable: {error}"
            ))),
        }
    }

    /// Validate lifecycle and policy immediately before opening a real
    /// data-plane connection. Unlike [`Self::ready_for_use`], this does not open
    /// a second connection merely to prove reachability.
    pub(crate) async fn resolve_for_connection(
        &self,
        environment_id: &EnvironmentId,
        now_ms: i64,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self.selectable(environment_id).await?;
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

    /// The environment's envd speaks another data-plane protocol version;
    /// it cannot serve this runtime until it is replaced or upgraded.
    #[error(
        "environment {environment_id} runs envd protocol version {remote}; this runtime requires {local}"
    )]
    VersionMismatch {
        environment_id: String,
        local: u32,
        remote: u32,
    },
}

/// `clientInfo.name` of readiness handshakes.
const READINESS_CLIENT: &str = "lightspeed-environment-readiness";

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

    use auth::SecretValue;
    use environment_protocol::ids::ProviderTargetId;
    use environments::{
        CreateEnvironment, EnvironmentEndpoint, EnvironmentIncarnationId,
        EnvironmentProviderBindingId, EnvironmentProviderBindingStatus,
        EnvironmentProviderBindingStore, EnvironmentProviderId, EnvironmentProvisionRequestId,
        EnvironmentStatus, EnvironmentTemplateId, ObserveProvisionedEnvironment,
        PutEnvironmentProvider, PutEnvironmentProviderBinding,
    };

    use super::*;

    fn target_id() -> Option<ProviderTargetId> {
        Some(ProviderTargetId::new("target-1").unwrap())
    }

    fn observation(
        environment_id: &EnvironmentId,
        status: EnvironmentStatus,
        power_states: Vec<PowerState>,
        observed_at_ms: i64,
    ) -> ObserveProvisionedEnvironment {
        ObserveProvisionedEnvironment {
            environment_id: environment_id.clone(),
            provider_target_id: target_id(),
            status,
            power_states,
            daemon_build: None,
            observed_at_ms,
        }
    }

    async fn resolver() -> (EnvironmentResolver, EnvironmentId) {
        let store = Arc::new(environments::InMemoryEnvironmentRegistryStore::new());
        let provider_id = EnvironmentProviderId::new("allowed");
        store
            .put_provider(PutEnvironmentProvider {
                provider_id: provider_id.clone(),
                display_name: None,
                controller_endpoint: EnvironmentEndpoint::parse("ws://controller.test/control")
                    .unwrap(),
                metadata: BTreeMap::new(),
                credential: Some(SecretValue::new("provider-credential")),
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
            .observe_provisioned_environment(observation(
                &environment_id,
                EnvironmentStatus::Offline,
                Vec::new(),
                10,
            ))
            .await
            .expect("environment");
        (
            EnvironmentResolver::new(store.clone(), store),
            environment_id,
        )
    }

    /// Counters and switches of the fake gateway route below.
    #[derive(Clone, Default)]
    struct Route {
        connections: Arc<std::sync::atomic::AtomicUsize>,
        initializations: Arc<std::sync::atomic::AtomicUsize>,
        scans: Arc<std::sync::atomic::AtomicUsize>,
        unchanged_roots: Arc<std::sync::atomic::AtomicUsize>,
        stall_skills: Arc<std::sync::atomic::AtomicBool>,
        stall: Arc<std::sync::atomic::AtomicBool>,
        unsupported: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Route {
        fn counts(&self) -> (usize, usize, usize) {
            use std::sync::atomic::Ordering::SeqCst;
            (
                self.connections.load(SeqCst),
                self.initializations.load(SeqCst),
                self.scans.load(SeqCst),
            )
        }
    }

    /// Serve a gateway data route that answers with a real envd's handlers
    /// over `root`, counting connections, handshakes and scans.
    async fn serve_route(
        root: std::path::PathBuf,
        state_dir: std::path::PathBuf,
        route: Route,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use environment_protocol::{data::scan::ScanResponse, jsonrpc, paths::EnvironmentPath};
        use futures_util::{SinkExt, StreamExt};
        use std::sync::atomic::Ordering::SeqCst;
        let runtime =
            environment_daemon::DaemonRuntime::new(environment_daemon::config::DaemonConfig {
                listen: None,
                cwd: root.clone(),
                fs_root: root.clone(),
                state_dir,
                read_only_fs: true,
                registration: None,
                scrubbed_env: Vec::new(),
            })
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                route.connections.fetch_add(1, SeqCst);
                while let Some(result) = clients.try_join_next() {
                    result.unwrap();
                }
                let route = route.clone();
                let runtime = runtime.clone();
                let root = root.clone();
                clients.spawn(async move {
                    let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
                    let context = environment_daemon::dispatch::RequestContext::detached();
                    while let Some(Ok(message)) = socket.next().await {
                        let Ok(text) = message.to_text() else {
                            continue;
                        };
                        let Ok(jsonrpc::Message::Request(request)) = jsonrpc::parse(text) else {
                            continue;
                        };
                        if route.stall.load(SeqCst) {
                            std::future::pending::<()>().await;
                        }
                        let response = if request.method == "initialize" {
                            route.initializations.fetch_add(1, SeqCst);
                            let mut initialized = runtime.initialize_response();
                            initialized.home_directory =
                                Some(EnvironmentPath::new(root.to_str().unwrap()).unwrap());
                            if route.unsupported.load(SeqCst) {
                                initialized.capabilities.filesystem = None;
                            }
                            jsonrpc::Response::success(request.id, &initialized)
                        } else {
                            if request.method == "fs/scan" {
                                route.scans.fetch_add(1, SeqCst);
                                let skills = request.params["filter"]["include"]
                                    .as_array()
                                    .is_some_and(|include| include.iter().any(|p| p == "SKILL.md"));
                                if skills && route.stall_skills.load(SeqCst) {
                                    std::future::pending::<()>().await;
                                }
                            }
                            let result = environment_daemon::dispatch::handle_request(
                                &runtime,
                                &context,
                                &request.method,
                                request.params.clone(),
                            )
                            .await
                            .map(|reply| reply.value);
                            if let Ok(value) = &result
                                && let Ok(scan) = serde_json::from_value::<ScanResponse>(value.clone())
                            {
                                let unchanged = scan
                                    .roots
                                    .iter()
                                    .filter(|root| {
                                        matches!(
                                            root,
                                            environment_protocol::data::scan::ScanRootResult::Unchanged { .. }
                                        )
                                    })
                                    .count();
                                route.unchanged_roots.fetch_add(unchanged, SeqCst);
                            }
                            jsonrpc::Response::from_result(request.id, result)
                        };
                        if socket
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                response.to_text().into(),
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
        });
        (base_url, task)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn environment_discovery_shares_connection_preserves_freshness_and_never_wakes() {
        use harness::{
            CoreAgentCommand,
            storage::{BlobStore, InMemoryBlobStore},
        };
        use std::sync::atomic::Ordering::SeqCst;
        use tools::skills::environment::{EnvironmentSkillAvailability, EnvironmentSkillCatalog};
        let (resolver, environment_id) = resolver().await;
        let store = resolver.environments.clone();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("root");
        std::fs::create_dir_all(root.join(".agents/skills/review")).unwrap();
        let doc = "---\nname: review\ndescription: Review code.\n---\nbody";
        let skill_path = root.join(".agents/skills/review/SKILL.md");
        std::fs::write(&skill_path, doc).unwrap();
        let route = Route::default();
        let (base_url, task) = serve_route(
            root.clone(),
            temp.path().canonicalize().unwrap().join("state"),
            route.clone(),
        )
        .await;
        let gateway =
            crate::environments::gateway::EnvironmentGatewayClientConfig::new(base_url, "test");
        let blobs = InMemoryBlobStore::new();
        let session_id = harness::SessionId::new(uuid::Uuid::new_v4().to_string());
        let feature = harness::EnvironmentsFeature {
            skills: Some(Default::default()),
            environments: vec![harness::EnvironmentAttachment {
                environment_id: environment_id.as_str().to_owned(),
                default: false,
                access: harness::EnvironmentAccess::Read,
                working_directory: None,
            }],
            ..Default::default()
        };
        let refresh = async |current| {
            crate::environments::sources::refresh(
                &blobs,
                Some(&resolver),
                Some(&gateway),
                &session_id,
                Some(&feature),
                Some(&environment_id),
                current,
            )
            .await
            .map(|publication| publication.skill_command)
        };
        let entry = |command| match command {
            Some(CoreAgentCommand::UpsertContext { entry, .. }) => entry,
            other => panic!("expected publication: {other:?}"),
        };
        let catalog_of = async |entry: &harness::ContextEntryInput| -> EnvironmentSkillCatalog {
            serde_json::from_slice(
                &blobs
                    .read_bytes(entry.provenance_ref.as_ref().unwrap())
                    .await
                    .unwrap(),
            )
            .unwrap()
        };
        // Offline inspection never changes desired power and never opens a data route.
        let before = store.read_environment(&environment_id).await.unwrap();
        let unavailable = entry(refresh(None).await.unwrap());
        assert_eq!(route.counts(), (0, 0, 0));
        assert_eq!(
            store.read_environment(&environment_id).await.unwrap(),
            before
        );
        assert!(refresh(Some(&unavailable)).await.unwrap().is_none());
        store
            .observe_provisioned_environment(observation(
                &environment_id,
                EnvironmentStatus::Ready,
                vec![PowerState::Running, PowerState::Paused],
                20,
            ))
            .await
            .unwrap();
        let available = entry(refresh(Some(&unavailable)).await.unwrap());
        let catalog = catalog_of(&available).await;
        assert_eq!(
            catalog.availability,
            EnvironmentSkillAvailability::Available
        );
        assert_eq!(catalog.skills.len(), 1);
        // The working directory is proven by the scan's probe root, not by
        // a separate metadata call: one connection, one handshake, one scan.
        assert_eq!(route.counts(), (1, 1, 1));
        // An unchanged root answers by fingerprint and reuses the cached skills.
        assert!(refresh(Some(&available)).await.unwrap().is_none());
        assert_eq!(route.unchanged_roots.load(SeqCst), 1);
        std::fs::write(&skill_path, doc.replace("body", "edit")).unwrap();
        assert!(
            refresh(Some(&available)).await.unwrap().is_none(),
            "body changes do not publish"
        );
        std::fs::write(&skill_path, doc.replace("Review code.", "Review changes.")).unwrap();
        let edited = entry(refresh(Some(&available)).await.unwrap());
        assert_ne!(edited.content, available.content);
        // Both sources share setup, but retain their own scans and observe every edit.
        let mut both = feature.clone();
        both.prompts = Some(Default::default());
        std::fs::create_dir_all(root.join(".agents/prompts")).unwrap();
        let prompt_path = root.join(".agents/prompts/instructions.md");
        let prompt_key = harness::ContextEntryKey::new(
            tools::prompts::environment::ENVIRONMENT_PROMPT_CONTEXT_KEY,
        );
        let refresh_sources = async |config, current| {
            crate::environments::sources::refresh(
                &blobs,
                Some(&resolver),
                Some(&gateway),
                &session_id,
                Some(config),
                Some(&environment_id),
                current,
            )
            .await
            .unwrap()
        };
        for text in ["First instructions", "Updated instructions"] {
            std::fs::write(&prompt_path, text).unwrap();
            let before = route.counts();
            let result = refresh_sources(&both, Some(&edited)).await;
            assert!(result.skill_command.is_none());
            assert_eq!(route.counts(), (before.0 + 1, before.1 + 1, before.2 + 2));
            assert_eq!(
                blobs
                    .read_bytes(&result.prompt_entries[&prompt_key].content.content_ref)
                    .await
                    .unwrap(),
                text.as_bytes()
            );
        }
        let before = route.counts();
        let prompts_only = harness::EnvironmentsFeature {
            prompts: Some(Default::default()),
            environments: feature.environments.clone(),
            ..Default::default()
        };
        let result = refresh_sources(&prompts_only, None).await;
        assert!(result.skill_command.is_none());
        assert!(result.prompt_entries.contains_key(&prompt_key));
        assert_eq!(route.counts(), (before.0 + 1, before.1 + 1, before.2 + 1));

        let before = route.counts();
        let disabled_feature = harness::EnvironmentsFeature::default();
        let disabled = refresh_sources(&disabled_feature, None).await;
        assert!(disabled.skill_command.is_none());
        assert!(disabled.prompt_entries.is_empty());
        let mut controller_owned = edited.clone();
        controller_owned.origin = Some("controller".into());
        assert!(
            refresh_sources(&feature, Some(&controller_owned))
                .await
                .skill_command
                .is_none()
        );
        assert_eq!(
            route.counts(),
            before,
            "disabled and controller-owned sources need no connection"
        );
        let result = refresh_sources(&both, Some(&controller_owned)).await;
        assert!(result.skill_command.is_none());
        assert!(result.prompt_entries.contains_key(&prompt_key));
        assert_eq!(route.counts(), (before.0 + 1, before.1 + 1, before.2 + 1));

        // A timed-out skill scan does not delay the prompt scan, which runs
        // on a fresh connection.
        route.stall_skills.store(true, SeqCst);
        let before = route.counts();
        let result = refresh_sources(&both, Some(&edited)).await;
        let failed_skills = entry(result.skill_command);
        let failed_catalog = catalog_of(&failed_skills).await;
        assert_eq!(
            failed_catalog.availability,
            EnvironmentSkillAvailability::Stale
        );
        assert_eq!(failed_catalog.skills.len(), 1);
        assert_eq!(failed_skills.content, edited.content);
        assert_eq!(
            blobs
                .read_bytes(&result.prompt_entries[&prompt_key].content.content_ref)
                .await
                .unwrap(),
            b"Updated instructions"
        );
        assert_eq!(route.counts(), (before.0 + 2, before.1 + 2, before.2 + 2));
        route.stall_skills.store(false, SeqCst);

        // One oversized skill is skipped and reported under its root; the
        // root's other skills are still listed.
        std::fs::create_dir_all(root.join(".agents/skills/other")).unwrap();
        std::fs::write(
            root.join(".agents/skills/other/SKILL.md"),
            "---\nname: other\ndescription: Other skill.\n---\nbody",
        )
        .unwrap();
        std::fs::write(&skill_path, vec![b'x'; 65537]).unwrap();
        let degraded = entry(refresh(Some(&edited)).await.unwrap());
        let catalog = catalog_of(&degraded).await;
        assert_eq!(
            catalog.availability,
            EnvironmentSkillAvailability::Available
        );
        assert_eq!(
            catalog
                .skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["other"]
        );
        let skills_root = root.join(".agents/skills");
        assert!(
            catalog
                .warnings
                .iter()
                .any(|warning| warning.starts_with(skills_root.to_str().unwrap())),
            "{:?}",
            catalog.warnings
        );
        std::fs::remove_dir_all(root.join(".agents/skills/other")).unwrap();
        std::fs::write(&skill_path, doc).unwrap();

        // Without the filesystem family discovery is explicitly
        // unavailable, with no scan.
        route.unsupported.store(true, SeqCst);
        let before = route.counts().2;
        let unsupported = entry(refresh(Some(&degraded)).await.unwrap());
        assert_eq!(route.counts().2, before);
        assert_eq!(
            catalog_of(&unsupported).await.availability,
            EnvironmentSkillAvailability::Stale
        );
        route.unsupported.store(false, SeqCst);
        std::fs::remove_file(&skill_path).unwrap();
        let empty = entry(refresh(Some(&unsupported)).await.unwrap());
        let catalog = catalog_of(&empty).await;
        assert!(catalog.skills.is_empty());
        assert_eq!(
            catalog.availability,
            EnvironmentSkillAvailability::Available
        );
        // Deselection removes only this catalog key.
        let cleared = crate::environments::sources::refresh(
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
            matches!(cleared.skill_command, Some(CoreAgentCommand::RemoveContext { key, .. }) if key.as_str() == "runtime.catalog.skills.environment")
        );
        let mut denied = feature.clone();
        denied.environments.clear();
        let denied_entry = entry(
            crate::environments::sources::refresh(
                &blobs,
                Some(&resolver),
                Some(&gateway),
                &session_id,
                Some(&denied),
                Some(&environment_id),
                Some(&available),
            )
            .await
            .unwrap()
            .skill_command,
        );
        let denied_catalog = catalog_of(&denied_entry).await;
        assert_eq!(
            denied_catalog.availability,
            EnvironmentSkillAvailability::Unavailable
        );
        assert!(
            denied_catalog.skills.is_empty(),
            "a revoked domain grant cannot retain advertised paths"
        );
        route.stall.store(true, SeqCst);
        let start = std::time::Instant::now();
        let timed_out = entry(refresh(Some(&available)).await.unwrap());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(6),
            "unresponsive endpoint must remain bounded"
        );
        assert_eq!(
            catalog_of(&timed_out).await.availability,
            EnvironmentSkillAvailability::Stale
        );
        assert_eq!(timed_out.content, available.content);
        task.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn offline_environment_without_gateway_is_unavailable_but_readable() {
        let (resolver, environment_id) = resolver().await;
        assert!(resolver.read(&environment_id).await.is_ok());
        assert!(
            resolver
                .resolve_for_connection(&environment_id, 111)
                .await
                .is_ok(),
            "execution resolution should defer reachability to the real connection"
        );
        assert!(matches!(
            resolver.ready_for_use(&environment_id, 111).await,
            Err(EnvironmentResolveError::EnvironmentUnavailable { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn readiness_reports_a_daemon_of_another_protocol_version() {
        use environment_protocol::{error::ProtocolError, jsonrpc};
        use futures_util::{SinkExt, StreamExt};
        let (resolver, environment_id) = resolver().await;
        resolver
            .environments
            .observe_provisioned_environment(observation(
                &environment_id,
                EnvironmentStatus::Ready,
                vec![PowerState::Running],
                20,
            ))
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let remote = environment_protocol::ENVD_PROTOCOL_VERSION + 1;
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            while let Some(Ok(message)) = socket.next().await {
                let Ok(text) = message.to_text() else {
                    continue;
                };
                let Ok(jsonrpc::Message::Request(request)) = jsonrpc::parse(text) else {
                    continue;
                };
                let response = jsonrpc::Response::failure(
                    Some(request.id),
                    ProtocolError::version_mismatch(
                        remote,
                        environment_protocol::ENVD_PROTOCOL_VERSION,
                    ),
                );
                let _ = socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        response.to_text().into(),
                    ))
                    .await;
            }
        });
        let resolver = resolver.with_gateway(
            crate::environments::gateway::EnvironmentGatewayClientConfig::new(base_url, "test"),
        );
        match resolver.ready_for_use(&environment_id, 30).await {
            Err(EnvironmentResolveError::VersionMismatch {
                local,
                remote: reported,
                ..
            }) => {
                assert_eq!(local, environment_protocol::ENVD_PROTOCOL_VERSION);
                assert_eq!(reported, remote);
            }
            other => panic!("expected a version mismatch, got {other:?}"),
        }
        task.abort();
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
                .observe_provisioned_environment(observation(
                    &environment_id,
                    status,
                    vec![PowerState::Running, PowerState::Paused],
                    30,
                ))
                .await
                .unwrap();
            let before = store.read_environment(&environment_id).await.unwrap();
            for _ in 0..2 {
                let selected = resolver
                    .selectable(&environment_id)
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
    async fn reselection_checks_terminal_status() {
        let (resolver, environment_id) = resolver().await;
        let store = resolver.environments.clone();
        resolver.selectable(&environment_id).await.unwrap();
        assert!(matches!(
            resolver.selectable(&EnvironmentId::new("missing")).await,
            Err(EnvironmentResolveError::Store(
                EnvironmentRegistryError::NotFound { .. }
            ))
        ));
        store
            .fail_environment_lifecycle(environments::FailEnvironmentLifecycle {
                environment_id: environment_id.clone(),
                message: "no capacity".into(),
                daemon_build: None,
                observed_at_ms: 40,
            })
            .await
            .unwrap();
        assert!(matches!(
            resolver.selectable(&environment_id).await,
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
            resolver.selectable(&environment_id).await,
            Err(EnvironmentResolveError::Closed { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn use_cancels_a_pending_power_down() {
        let (resolver, environment_id) = resolver().await;
        let store = resolver.environments.clone();
        store
            .observe_provisioned_environment(observation(
                &environment_id,
                EnvironmentStatus::Ready,
                vec![PowerState::Running, PowerState::Paused],
                20,
            ))
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
            .resolve_for_connection(&environment_id, 30)
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
                    .observe_provisioned_environment(observation(
                        &environment_id,
                        status,
                        vec![PowerState::Running, PowerState::Paused],
                        at,
                    ))
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
            resolver.ready_for_use(&environment_id, 30).await,
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
            .selectable(&environment_id)
            .await
            .expect("activation admits a paused environment");
        assert_eq!(record.status, EnvironmentStatus::Paused);

        // Once the provider observed it running again the ordinary probe
        // path applies (no gateway here → unavailable, not NotReady).
        observe(EnvironmentStatus::Ready, 40).await;
        assert!(matches!(
            resolver.ready_for_use(&environment_id, 50).await,
            Err(EnvironmentResolveError::EnvironmentUnavailable { .. })
        ));

        // A stopped environment whose provider offers no power control keeps
        // the old behaviour: no wake, plain unavailability.
        store
            .observe_provisioned_environment(observation(
                &environment_id,
                EnvironmentStatus::Offline,
                Vec::new(),
                60,
            ))
            .await
            .expect("observe offline without power control");
        assert!(matches!(
            resolver.ready_for_use(&environment_id, 70).await,
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
                    .observe_provisioned_environment(observation(
                        &environment_id,
                        status,
                        Vec::new(),
                        20,
                    ))
                    .await
                    .expect("observe");
            }
        };

        // A provisioning/booting environment is admitted as intent without a
        // probe and reported as not ready.
        observe(EnvironmentStatus::Provisioning).await;
        assert!(matches!(
            resolver.ready_for_use(&environment_id, 30).await,
            Err(EnvironmentResolveError::NotReady {
                status: EnvironmentStatus::Provisioning,
                ..
            })
        ));
        observe(EnvironmentStatus::Booting).await;
        assert!(matches!(
            resolver.ready_for_use(&environment_id, 30).await,
            Err(EnvironmentResolveError::NotReady {
                status: EnvironmentStatus::Booting,
                ..
            })
        ));
        let record = resolver
            .selectable(&environment_id)
            .await
            .expect("activation admits a booting environment");
        assert_eq!(record.status, EnvironmentStatus::Booting);

        store
            .fail_environment_lifecycle(environments::FailEnvironmentLifecycle {
                environment_id: environment_id.clone(),
                message: "no capacity".to_owned(),
                daemon_build: None,
                observed_at_ms: 40,
            })
            .await
            .expect("fail");
        assert!(matches!(
            resolver.ready_for_use(&environment_id, 50).await,
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
            resolver.ready_for_use(&environment_id, 70).await,
            Err(EnvironmentResolveError::Closed { .. })
        ));
    }
}

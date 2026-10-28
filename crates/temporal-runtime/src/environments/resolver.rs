//! Internal environment lookup and the single use gate.
//!
//! This is a shared runtime service, not a provider/plugin extension seam.
//! Every consumer asks [`EnvironmentRecord::use_gate`] through it; nothing
//! here writes to a row.

use std::sync::Arc;

use environment_client::{ClientError, DataClient};
use environments::{
    EnvironmentGoneReason, EnvironmentId, EnvironmentRecord, EnvironmentRegistryError,
    EnvironmentState, EnvironmentStore, EnvironmentUse, EnvironmentWaitReason,
};
use store_pg::PgStore;
use thiserror::Error;

#[derive(Clone)]
pub(crate) struct EnvironmentResolver {
    environments: Arc<dyn EnvironmentStore>,
    gateway: Option<crate::environments::gateway::EnvironmentGatewayClientConfig>,
    universe_id: uuid::Uuid,
}

impl EnvironmentResolver {
    pub(crate) fn universe_id(&self) -> uuid::Uuid {
        self.universe_id
    }

    pub(crate) fn new(environments: Arc<dyn EnvironmentStore>) -> Self {
        Self {
            environments,
            gateway: None,
            universe_id: uuid::Uuid::nil(),
        }
    }

    pub(crate) fn from_pg_store(store: Arc<PgStore>) -> Self {
        let universe_id = store.config().universe_id;
        let mut resolver = Self::new(store);
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

    /// Selection: rejects closing, closed and failed environments; accepts
    /// one that is not usable yet and a registered one whose daemon is away.
    /// Selecting never changes power or proves reachability.
    pub(crate) async fn selectable(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self.read(environment_id).await?;
        match environment.state {
            EnvironmentState::Failed => {
                Err(failed(&environment, environment.state_message.clone()))
            }
            EnvironmentState::Closing | EnvironmentState::Closed => {
                Err(EnvironmentResolveError::Closed {
                    environment_id: environment.environment_id.to_string(),
                })
            }
            EnvironmentState::Provisioning | EnvironmentState::Open => Ok(environment),
        }
    }

    /// The gate before a real connection: usable → the record; not yet →
    /// `NotReady`; gone → `Closed`, `Failed` or `Away`. Never writes.
    pub(crate) async fn resolve_for_connection(
        &self,
        environment_id: &EnvironmentId,
        now_ms: i64,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self.read(environment_id).await?;
        match environment.use_gate(now_ms) {
            EnvironmentUse::Use => Ok(environment),
            gate => Err(gate_error(&environment, gate)),
        }
    }

    /// [`Self::resolve_for_connection`], then one handshake over the worker
    /// route. The dial is the wake signal: a powered-down target answers a
    /// typed refusal while it wakes, which maps through
    /// [`EnvironmentRecord::bound_route_refusal`]. The handshake proves
    /// nothing durable and its result is never written. A daemon of another
    /// protocol version is a typed error, never a usable environment whose
    /// calls all fail.
    pub(crate) async fn ready_for_use(
        &self,
        environment_id: &EnvironmentId,
        now_ms: i64,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self.resolve_for_connection(environment_id, now_ms).await?;
        let Some(gateway) = &self.gateway else {
            return Err(EnvironmentResolveError::Unreachable {
                environment_id: environment.environment_id.to_string(),
                status: "no environment gateway is configured".to_owned(),
            });
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
                    environment_id: environment.environment_id.to_string(),
                    local,
                    remote,
                })
            }
            Err(error) => Err(
                route_refusal_error(&environment, &error).unwrap_or_else(|| {
                    EnvironmentResolveError::Unreachable {
                        environment_id: environment.environment_id.to_string(),
                        status: error.to_string(),
                    }
                }),
            ),
        }
    }
}

/// A worker-route dial error that carries a typed refusal, as a resolve
/// error (through [`EnvironmentRecord::bound_route_refusal`]); `None` when
/// it carries none.
pub(crate) fn route_refusal_error(
    record: &EnvironmentRecord,
    error: &ClientError,
) -> Option<EnvironmentResolveError> {
    let refusal = error.data_route_refusal()?;
    match record.bound_route_refusal(refusal, error.retry_after_ms()) {
        EnvironmentUse::Use => None,
        gate => Some(gate_error(record, gate)),
    }
}

/// The resolve error a gate answer other than `Use` is.
fn gate_error(record: &EnvironmentRecord, gate: EnvironmentUse) -> EnvironmentResolveError {
    let environment_id = record.environment_id.to_string();
    match gate {
        EnvironmentUse::Use => EnvironmentResolveError::Unreachable {
            environment_id,
            status: "the environment is usable".to_owned(),
        },
        EnvironmentUse::Wait {
            reason,
            retry_after_ms,
            until_ms,
        } => EnvironmentResolveError::NotReady {
            environment_id,
            reason,
            retry_after_ms,
            until_ms,
        },
        EnvironmentUse::Gone { reason, message } => match reason {
            EnvironmentGoneReason::Closing | EnvironmentGoneReason::Closed => {
                EnvironmentResolveError::Closed { environment_id }
            }
            EnvironmentGoneReason::Failed => failed(record, message),
            EnvironmentGoneReason::Away => EnvironmentResolveError::Away {
                environment_id,
                message: message.unwrap_or_else(|| "the daemon is away".to_owned()),
            },
        },
    }
}

fn failed(record: &EnvironmentRecord, message: Option<String>) -> EnvironmentResolveError {
    EnvironmentResolveError::Failed {
        environment_id: record.environment_id.to_string(),
        message: message.unwrap_or_else(|| "the environment failed".to_owned()),
    }
}

#[derive(Debug, Error)]
pub(crate) enum EnvironmentResolveError {
    #[error(transparent)]
    Store(#[from] EnvironmentRegistryError),

    /// Not usable yet: re-check after `retry_after_ms`; `until_ms` bounds
    /// the wait when the gate knows a bound.
    #[error("environment is not ready yet: {environment_id} ({reason})")]
    NotReady {
        environment_id: String,
        reason: EnvironmentWaitReason,
        retry_after_ms: u64,
        until_ms: Option<i64>,
    },

    #[error("environment failed: {environment_id}: {message}")]
    Failed {
        environment_id: String,
        message: String,
    },

    #[error("environment is closed: {environment_id}")]
    Closed { environment_id: String },

    /// Terminal: the registered daemon is away past its bound (gate
    /// `Gone{Away}`, a `disconnected` refusal for an ephemeral key, or the
    /// gateway's `away` refusal).
    #[error("environment {environment_id} is away: {message}")]
    Away {
        environment_id: String,
        message: String,
    },

    /// Retryable: the dial failed without a typed refusal (connect error,
    /// timeout, the gateway's bodiless 503 for a provider 5xx/429/timeout,
    /// 502, 504, a gateway restart). `status` is a short display string.
    #[error("environment is unreachable: {environment_id} ({status})")]
    Unreachable {
        environment_id: String,
        status: String,
    },

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use environment_protocol::{
        control::target::{PowerState, TargetPhase},
        ids::ProviderTargetId,
        metadata::DescriptiveText,
        routes::DataRouteRefusal,
    };
    use environments::{
        BeginCloseEnvironment, CreateEnvironment, CreateEnvironmentRegistrationKey,
        CreateRegisteredEnvironment, DaemonBuild, EnvironmentCloseReason, EnvironmentEndpoint,
        EnvironmentProviderBindingStore, EnvironmentProviderId, EnvironmentProviderStore,
        EnvironmentProvisionRequestId, EnvironmentRegistrationKeyId,
        EnvironmentRegistrationKeyStore, EnvironmentSource, EnvironmentTemplateId, FailEnvironment,
        MarkRegisteredDisconnected, ObservedTarget, OpenEnvironment, ProviderCredentialChange,
        PutEnvironmentProvider, PutEnvironmentProviderBinding, REGISTERED_RECONNECT_WAIT_MS,
        RecordTargetObservation, RegisteredIdentityMode, RegistrationKeyPolicy,
        WAIT_RETRY_AFTER_MS, mint_registration_key,
    };

    use super::*;

    fn build() -> DaemonBuild {
        DaemonBuild {
            version: None,
            git_sha: None,
            protocol_version: environment_protocol::ENVD_PROTOCOL_VERSION,
        }
    }

    /// A resolver over an in-memory store holding one provisioning
    /// environment.
    async fn resolver() -> (EnvironmentResolver, EnvironmentId) {
        let store = Arc::new(environments::InMemoryEnvironmentRegistryStore::new());
        let provider_id = EnvironmentProviderId::new("allowed");
        store
            .put_provider(PutEnvironmentProvider {
                provider_id: provider_id.clone(),
                display_name: None,
                controller_endpoint: EnvironmentEndpoint::parse("ws://controller.test/control")
                    .unwrap(),
                data_route_base: None,
                credential: ProviderCredentialChange::Keep,
                updated_at_ms: 10,
            })
            .await
            .expect("provider");
        store
            .put_provider_binding(PutEnvironmentProviderBinding {
                universe_id: store.universe_id(),
                provider_id: provider_id.clone(),
                enabled: true,
                updated_at_ms: 10,
            })
            .await
            .expect("binding");
        let environment_id = EnvironmentId::new("environment-1");
        store
            .create_environment(CreateEnvironment {
                request_id: EnvironmentProvisionRequestId::new("request-1"),
                environment_id: environment_id.clone(),
                provider_id,
                template_id: EnvironmentTemplateId::new("test-template"),
                idle: None,
                display_name: None,
                metadata: BTreeMap::new(),
                created_at_ms: 10,
            })
            .await
            .expect("create environment");
        (EnvironmentResolver::new(store), environment_id)
    }

    async fn open(store: &Arc<dyn EnvironmentStore>, environment_id: &EnvironmentId, at_ms: i64) {
        store
            .open_environment(OpenEnvironment {
                environment_id: environment_id.clone(),
                daemon_build: build(),
                at_ms,
            })
            .await
            .expect("open");
    }

    async fn observe(
        store: &Arc<dyn EnvironmentStore>,
        environment_id: &EnvironmentId,
        power: PowerState,
        at_ms: i64,
    ) {
        store
            .record_target_observation(RecordTargetObservation {
                environment_id: environment_id.clone(),
                provider_target_id: Some(ProviderTargetId::new("target-1").unwrap()),
                observed: ObservedTarget {
                    phase: TargetPhase::Active,
                    power: Some(power),
                    message: None,
                    observed_at_ms: at_ms,
                },
                public_endpoint: None,
                observed_at_ms: at_ms,
            })
            .await
            .expect("observe");
    }

    /// A registered environment of a key in `mode` whose daemon
    /// disconnected at `disconnected_at_ms`.
    async fn disconnected_registered(
        mode: RegisteredIdentityMode,
        disconnected_at_ms: i64,
    ) -> (EnvironmentResolver, EnvironmentId) {
        let store = Arc::new(environments::InMemoryEnvironmentRegistryStore::new());
        let minted = mint_registration_key(
            EnvironmentRegistrationKeyId::new("rk"),
            RegistrationKeyPolicy {
                display_name: "fleet".to_owned(),
                identity_mode: mode,
                max_active_environments: None,
                ephemeral_disconnect_grace_ms: None,
                offline_close_after_ms: None,
                expires_at_ms: None,
            },
            1,
        )
        .unwrap();
        let secret_hash = minted.secret.secret_hash;
        store
            .create_registration_key(CreateEnvironmentRegistrationKey {
                secret_hash: secret_hash.clone(),
                record: minted.record,
            })
            .await
            .unwrap();
        let environment_id = EnvironmentId::new("registered-1");
        store
            .create_registered_environment(CreateRegisteredEnvironment {
                registration_key_id: EnvironmentRegistrationKeyId::new("rk"),
                secret_hash,
                environment_id: environment_id.clone(),
                daemon_public_key: "ab".repeat(32),
                display_name: None,
                metadata: BTreeMap::new(),
                daemon_build: build(),
                created_at_ms: 10,
            })
            .await
            .unwrap();
        store
            .mark_registered_disconnected(MarkRegisteredDisconnected {
                environment_id: environment_id.clone(),
                at_ms: disconnected_at_ms,
            })
            .await
            .unwrap();
        (EnvironmentResolver::new(store), environment_id)
    }

    fn refused(
        status: u16,
        refusal: Option<DataRouteRefusal>,
        retry_after_ms: Option<u64>,
    ) -> ClientError {
        ClientError::UpgradeRejected {
            status,
            refusal,
            retry_after_ms,
        }
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
        // Neither a provisioning nor a paused environment is dialed:
        // discovery never wakes a machine and never writes the row.
        let before = store.read_environment(&environment_id).await.unwrap();
        let unavailable = entry(refresh(None).await.unwrap());
        assert_eq!(route.counts(), (0, 0, 0));
        assert_eq!(
            store.read_environment(&environment_id).await.unwrap(),
            before
        );
        assert!(refresh(Some(&unavailable)).await.unwrap().is_none());
        open(&store, &environment_id, 20).await;
        observe(&store, &environment_id, PowerState::Paused, 21).await;
        assert!(refresh(Some(&unavailable)).await.unwrap().is_none());
        assert_eq!(route.counts(), (0, 0, 0));
        observe(&store, &environment_id, PowerState::Running, 22).await;
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
    async fn an_open_environment_without_a_gateway_resolves_but_is_unreachable() {
        let (resolver, environment_id) = resolver().await;
        open(&resolver.environments, &environment_id, 20).await;
        assert!(resolver.read(&environment_id).await.is_ok());
        assert!(
            resolver
                .resolve_for_connection(&environment_id, 111)
                .await
                .is_ok(),
            "execution resolution defers reachability to the real connection"
        );
        assert!(matches!(
            resolver.ready_for_use(&environment_id, 111).await,
            Err(EnvironmentResolveError::Unreachable { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn readiness_reports_a_daemon_of_another_protocol_version() {
        use environment_protocol::{error::ProtocolError, jsonrpc};
        use futures_util::{SinkExt, StreamExt};
        let (resolver, environment_id) = resolver().await;
        open(&resolver.environments, &environment_id, 20).await;
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

    /// Serve one HTTP refusal per connection instead of an upgrade.
    async fn serve_refusal(
        status: &'static str,
        body: &'static str,
        retry_after: Option<u64>,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match socket.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let retry = retry_after
                    .map(|seconds| format!("Retry-After: {seconds}\r\n"))
                    .unwrap_or_default();
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{retry}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        base_url
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_typed_route_refusal_maps_to_wait_or_gone_and_a_bodiless_one_is_unreachable() {
        let (resolver, environment_id) = resolver().await;
        open(&resolver.environments, &environment_id, 20).await;
        let with_route = |base_url: String| {
            resolver.clone().with_gateway(
                crate::environments::gateway::EnvironmentGatewayClientConfig::new(base_url, "test"),
            )
        };
        let waking = with_route(
            serve_refusal("503 Service Unavailable", r#"{"state":"waking"}"#, Some(3)).await,
        );
        assert!(matches!(
            waking.ready_for_use(&environment_id, 30).await,
            Err(EnvironmentResolveError::NotReady {
                reason: EnvironmentWaitReason::Waking,
                retry_after_ms: 3_000,
                until_ms: None,
                ..
            })
        ));
        let gone = with_route(serve_refusal("410 Gone", r#"{"state":"gone"}"#, None).await);
        assert!(matches!(
            gone.ready_for_use(&environment_id, 30).await,
            Err(EnvironmentResolveError::Closed { .. })
        ));
        let failed = with_route(
            serve_refusal(
                "409 Conflict",
                r#"{"state":"failed","message":"disk"}"#,
                None,
            )
            .await,
        );
        assert!(matches!(
            failed.ready_for_use(&environment_id, 30).await,
            Err(EnvironmentResolveError::Failed { message, .. }) if message == "disk"
        ));
        let bodiless = with_route(serve_refusal("503 Service Unavailable", "", None).await);
        assert!(matches!(
            bodiless.ready_for_use(&environment_id, 30).await,
            Err(EnvironmentResolveError::Unreachable { .. })
        ));
        // Nothing the readiness dial learns is written.
        let record = resolver.read(&environment_id).await.unwrap();
        assert_eq!(record.state, EnvironmentState::Open);
        assert_eq!(record.updated_at_ms, 20);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_disconnected_refusal_follows_the_identity_mode() {
        let disconnected = refused(503, Some(DataRouteRefusal::Disconnected {}), Some(2_000));
        let (persistent, persistent_id) =
            disconnected_registered(RegisteredIdentityMode::Persistent, 50).await;
        // The row still reads connected in the window this refusal covers.
        let mut record = persistent.read(&persistent_id).await.unwrap();
        if let EnvironmentSource::Registered(source) = &mut record.source {
            source.connected = true;
        }
        assert!(matches!(
            route_refusal_error(&record, &disconnected),
            Some(EnvironmentResolveError::NotReady {
                reason: EnvironmentWaitReason::Disconnected,
                until_ms: None,
                ..
            })
        ));
        let (ephemeral, ephemeral_id) =
            disconnected_registered(RegisteredIdentityMode::Ephemeral, 50).await;
        let record = ephemeral.read(&ephemeral_id).await.unwrap();
        assert!(matches!(
            route_refusal_error(&record, &disconnected),
            Some(EnvironmentResolveError::Away { .. })
        ));
        assert!(matches!(
            route_refusal_error(
                &record,
                &refused(410, Some(DataRouteRefusal::Away {}), None)
            ),
            Some(EnvironmentResolveError::Away { .. })
        ));
        assert!(route_refusal_error(&record, &refused(502, None, None)).is_none());
        let failed = DataRouteRefusal::Failed {
            message: DescriptiveText::new("broken").unwrap(),
        };
        assert!(matches!(
            route_refusal_error(&record, &refused(409, Some(failed), None)),
            Some(EnvironmentResolveError::Failed { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_disconnected_persistent_daemon_is_waited_for_until_its_bound() {
        let (resolver, environment_id) =
            disconnected_registered(RegisteredIdentityMode::Persistent, 1_000).await;
        let until = 1_000 + REGISTERED_RECONNECT_WAIT_MS;
        match resolver
            .resolve_for_connection(&environment_id, 2_000)
            .await
        {
            Err(EnvironmentResolveError::NotReady {
                reason: EnvironmentWaitReason::Disconnected,
                retry_after_ms,
                until_ms,
                ..
            }) => {
                assert_eq!(retry_after_ms, WAIT_RETRY_AFTER_MS);
                assert_eq!(until_ms, Some(until));
            }
            other => panic!("expected a bounded wait, got {other:?}"),
        }
        assert!(matches!(
            resolver
                .resolve_for_connection(&environment_id, until)
                .await,
            Err(EnvironmentResolveError::Away { .. })
        ));
        // Selection still accepts it: the daemon may come back.
        assert!(resolver.selectable(&environment_id).await.is_ok());
        let (ephemeral, ephemeral_id) =
            disconnected_registered(RegisteredIdentityMode::Ephemeral, 1_000).await;
        assert!(matches!(
            ephemeral.resolve_for_connection(&ephemeral_id, 1_001).await,
            Err(EnvironmentResolveError::Away { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_gate_waits_while_provisioning_and_selection_never_writes() {
        let (resolver, environment_id) = resolver().await;
        let store = resolver.environments.clone();
        assert!(matches!(
            resolver.ready_for_use(&environment_id, 30).await,
            Err(EnvironmentResolveError::NotReady {
                reason: EnvironmentWaitReason::Provisioning,
                retry_after_ms: WAIT_RETRY_AFTER_MS,
                until_ms: None,
                ..
            })
        ));
        for step in 0..3 {
            match step {
                1 => open(&store, &environment_id, 20).await,
                2 => observe(&store, &environment_id, PowerState::Suspended, 21).await,
                _ => {}
            }
            let before = store.read_environment(&environment_id).await.unwrap();
            for _ in 0..2 {
                let selected = resolver
                    .selectable(&environment_id)
                    .await
                    .expect("selection requires only valid registry state");
                assert_eq!(selected, before);
                resolver
                    .resolve_for_connection(&environment_id, 30)
                    .await
                    .ok();
                assert_eq!(
                    store.read_environment(&environment_id).await.unwrap(),
                    before
                );
            }
        }
        // A suspended environment is usable: the dial wakes it.
        assert!(
            resolver
                .resolve_for_connection(&environment_id, 30)
                .await
                .is_ok()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_and_closed_environments_are_gone() {
        let (resolver, environment_id) = resolver().await;
        let store = resolver.environments.clone();
        assert!(matches!(
            resolver.selectable(&EnvironmentId::new("missing")).await,
            Err(EnvironmentResolveError::Store(
                EnvironmentRegistryError::NotFound { .. }
            ))
        ));
        store
            .fail_environment(FailEnvironment {
                environment_id: environment_id.clone(),
                message: "no capacity".into(),
                daemon_build: None,
                at_ms: 40,
            })
            .await
            .unwrap();
        assert!(matches!(
            resolver.selectable(&environment_id).await,
            Err(EnvironmentResolveError::Failed { message, .. }) if message == "no capacity"
        ));
        assert!(matches!(
            resolver.ready_for_use(&environment_id, 50).await,
            Err(EnvironmentResolveError::Failed { message, .. }) if message == "no capacity"
        ));
        store
            .begin_close_environment(BeginCloseEnvironment {
                environment_id: environment_id.clone(),
                reason: EnvironmentCloseReason::Requested,
                at_ms: 50,
            })
            .await
            .unwrap();
        assert!(matches!(
            resolver.selectable(&environment_id).await,
            Err(EnvironmentResolveError::Closed { .. })
        ));
        assert!(matches!(
            resolver.resolve_for_connection(&environment_id, 60).await,
            Err(EnvironmentResolveError::Closed { .. })
        ));
    }
}

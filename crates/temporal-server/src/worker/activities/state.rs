use std::sync::Arc;

use auth::{
    AuthGrantStore, AuthProviderKind, AuthProviderStore, AuthTokenBroker, GitHubAppRuntime,
    GrantRefreshLock, HttpGitHubApiClient, HttpOAuthTokenClient, OAuthClientStore,
    OAuthRefreshRuntime, RegistryTokenBroker, SecretStore,
};
use engine::{
    CoreAgentLlm, CoreAgentTools, ProviderApiKind,
    storage::{BlobGraphStore, BlobStore, SessionStore},
};
use environments::EnvironmentStore;
use llm_clients::{
    anthropic::messages as am,
    openai::{completions as oai_completions, responses as oai},
};
use llm_runtime::{
    AnthropicMessagesLlmAdapter, LlmAdapterRegistry, LlmRuntime, McpInventoryResolver,
    ModelProviderResolver, OpenAiCompletionsLlmAdapter, OpenAiResponsesLlmAdapter,
    secrets::SecretResolver,
};
use store_pg::PgStore;
use vfs::VfsWorkspaceStore;

use crate::worker::mcp::{McpPrivateNetworkPolicy, NativeMcpInventoryResolver, NativeMcpRuntime};
use crate::{
    config::pg_store_from_env,
    credential_injection::EnvironmentCredentialResolver,
    environment_gateway::EnvironmentGatewayClientConfig,
    subagents::{SubagentChildRuntime, SubagentService},
    worker::{BrokerSecretResolver, SessionTools, StoredProviderKeyResolver},
};

use super::preprocess::{
    AudioTranscoder, AudioTranscriber, OpenAiAudioTranscriber, UnavailableAudioTranscriber,
    default_audio_transcoder_from_env, default_openai_audio_transcriber,
};
use crate::universe::DeploymentClients;

#[derive(Clone)]
pub struct StorageActivityDeps {
    pub(super) sessions: Arc<dyn SessionStore>,
    pub(super) blobs: Arc<dyn BlobStore>,
    pub(super) blob_graph: Option<Arc<dyn BlobGraphStore>>,
}

#[derive(Clone)]
pub struct LlmActivityDeps {
    pub(super) llm: Arc<dyn CoreAgentLlm>,
    pub(super) blobs: Arc<dyn BlobStore>,
}

#[derive(Clone)]
pub struct ToolActivityDeps {
    pub(super) tools: Arc<dyn CoreAgentTools>,
    pub(super) blobs: Arc<dyn BlobStore>,
    /// Records output-to-asset edges for native MCP results.
    pub(super) blob_graph: Option<Arc<dyn BlobGraphStore>>,
    /// The hosted runtime behind `tools` when it is the real `SessionTools`:
    /// grants the per-call not-ready outcome and the environment readiness
    /// wait. Absent for injected fake runtimes, which never report a
    /// not-ready environment.
    pub(super) hosted: Option<Arc<SessionTools>>,
    pub(super) native_mcp: Option<Arc<NativeMcpRuntime>>,
}

#[derive(Clone)]
pub struct RuntimeProjectionActivityDeps {
    pub(super) environment_resolver: Option<crate::environment_resolver::EnvironmentResolver>,
    pub(super) environment_gateway: Option<EnvironmentGatewayClientConfig>,
    pub(super) blobs: Arc<dyn BlobStore>,
    /// Records catalog-, report-, and projection-to-child edges.
    pub(super) blob_graph: Option<Arc<dyn BlobGraphStore>>,
    pub(super) workspace_store: Arc<dyn VfsWorkspaceStore>,
    /// Profile registry for the sub-agent catalog; absent in minimal test
    /// states, where the catalog lists ids without descriptions.
    pub(super) profiles: Option<Arc<dyn ::profiles::ProfileStore>>,
}

#[derive(Clone)]
pub struct PreprocessActivityDeps {
    pub(super) blobs: Arc<dyn BlobStore>,
    pub(super) transcriber: Arc<dyn AudioTranscriber>,
    pub(super) transcoder: Option<Arc<dyn AudioTranscoder>>,
}

/// Narrow live-resource dependencies used by environment-job activities.
/// Session history is intentionally absent: job preparation consumes its
/// pinned execution context and may read only CAS, environment catalog, and
/// credential resources.
#[derive(Clone)]
pub struct EnvironmentJobActivityDeps {
    pub(super) blobs: Arc<dyn BlobStore>,
    pub(super) blob_graph: Option<Arc<dyn BlobGraphStore>>,
    pub(super) environments: Arc<dyn EnvironmentStore>,
    pub(super) credentials: EnvironmentCredentialResolver,
    pub(super) gateway: Option<EnvironmentGatewayClientConfig>,
    pub(super) universe_id: uuid::Uuid,
}

/// Temporal-client deps for the generic start-on-call adapter: starting an
/// admitted versioned recipe, describing the derived execution, running the
/// fixed recovery query, and cancelling the exact execution.
#[derive(Clone)]
pub struct WorkflowToolExecutionDeps {
    pub(super) client: temporalio_client::Client,
}

/// Deps for the sub-agent execution activities.
#[derive(Clone)]
pub struct SubagentActivityDeps {
    pub(super) service: SubagentService,
}

#[derive(Clone)]
pub struct ActivityState {
    storage: StorageActivityDeps,
    llm: LlmActivityDeps,
    tools: ToolActivityDeps,
    runtime_projection: Option<RuntimeProjectionActivityDeps>,
    preprocess: PreprocessActivityDeps,
    environment_jobs: Option<EnvironmentJobActivityDeps>,
    workflow_tool_executions: Option<WorkflowToolExecutionDeps>,
    subagents: Option<SubagentActivityDeps>,
}

impl ActivityState {
    pub fn new(
        sessions: Arc<dyn SessionStore>,
        blobs: Arc<dyn BlobStore>,
        llm: Arc<dyn CoreAgentLlm>,
        tools: Arc<dyn CoreAgentTools>,
    ) -> Self {
        Self {
            storage: StorageActivityDeps {
                sessions,
                blobs: blobs.clone(),
                blob_graph: None,
            },
            llm: LlmActivityDeps {
                llm,
                blobs: blobs.clone(),
            },
            tools: ToolActivityDeps {
                tools,
                blobs: blobs.clone(),
                blob_graph: None,
                hosted: None,
                native_mcp: None,
            },
            runtime_projection: None,
            preprocess: PreprocessActivityDeps {
                blobs: blobs.clone(),
                transcriber: Arc::new(UnavailableAudioTranscriber),
                transcoder: None,
            },
            environment_jobs: None,
            workflow_tool_executions: None,
            subagents: None,
        }
    }

    pub fn with_runtime_projection_deps(
        mut self,
        workspace_store: Arc<dyn VfsWorkspaceStore>,
    ) -> Self {
        self.runtime_projection = Some(RuntimeProjectionActivityDeps {
            environment_resolver: None,
            environment_gateway: None,
            blobs: self.storage.blobs.clone(),
            blob_graph: self.storage.blob_graph.clone(),
            workspace_store,
            profiles: None,
        });
        self
    }

    pub fn with_profile_store(mut self, profiles: Arc<dyn ::profiles::ProfileStore>) -> Self {
        if let Some(projection) = self.runtime_projection.as_mut() {
            projection.profiles = Some(profiles);
        }
        self
    }

    pub fn with_audio_transcriber(mut self, transcriber: Arc<dyn AudioTranscriber>) -> Self {
        self.preprocess.transcriber = transcriber;
        self
    }

    pub fn with_workflow_tool_executions(mut self, client: temporalio_client::Client) -> Self {
        self.workflow_tool_executions = Some(WorkflowToolExecutionDeps { client });
        self
    }

    pub fn with_subagent_runtime(mut self, runtime: Arc<dyn SubagentChildRuntime>) -> Self {
        self.subagents = Some(SubagentActivityDeps {
            service: SubagentService::new(
                self.storage.sessions.clone(),
                self.storage.blobs.clone(),
                runtime,
            )
            .with_blob_graph(self.storage.blob_graph.clone()),
        });
        self
    }

    pub fn with_audio_transcoder(mut self, transcoder: Arc<dyn AudioTranscoder>) -> Self {
        self.preprocess.transcoder = Some(transcoder);
        self
    }

    pub fn from_pg_store(
        store: Arc<PgStore>,
        llm: Arc<dyn CoreAgentLlm>,
        tools: Arc<dyn CoreAgentTools>,
    ) -> Self {
        let universe_id = store.config().universe_id;
        let sessions: Arc<dyn SessionStore> = store.clone();
        let blobs: Arc<dyn BlobStore> = store.clone();
        let blob_graph: Arc<dyn BlobGraphStore> = store.clone();
        let environment_job_blobs = blobs.clone();
        let environment_job_environments: Arc<dyn EnvironmentStore> = store.clone();
        let environment_job_credentials =
            EnvironmentCredentialResolver::from_pg_store(store.clone());
        let workspace_store: Arc<dyn VfsWorkspaceStore> = store.clone();
        let profile_store: Arc<dyn ::profiles::ProfileStore> = store.clone();
        let mut state = Self::new(sessions, blobs, llm, tools);
        state.storage.blob_graph = Some(blob_graph.clone());
        state.tools.blob_graph = Some(blob_graph.clone());
        let mut state = state
            .with_runtime_projection_deps(workspace_store)
            .with_profile_store(profile_store);
        if let Some(projection) = state.runtime_projection.as_mut() {
            projection.environment_resolver = Some(
                crate::environment_resolver::EnvironmentResolver::from_pg_store(store.clone()),
            );
        }
        state.environment_jobs = Some(EnvironmentJobActivityDeps {
            blobs: environment_job_blobs,
            blob_graph: Some(blob_graph),
            environments: environment_job_environments,
            credentials: environment_job_credentials,
            gateway: None,
            universe_id,
        });
        state
    }

    /// Register the hosted runtime so per-call activities can distinguish a
    /// not-ready environment and wait for readiness.
    pub fn with_hosted_tools(mut self, hosted: Arc<SessionTools>) -> Self {
        self.tools.hosted = Some(hosted);
        self
    }

    /// Use one gateway for idle source discovery and durable environment jobs.
    pub fn with_environment_gateway(mut self, gateway: EnvironmentGatewayClientConfig) -> Self {
        if let Some(projection) = self.runtime_projection.as_mut() {
            projection.environment_gateway = Some(gateway.clone());
        }
        if let Some(environment_jobs) = self.environment_jobs.as_mut() {
            environment_jobs.gateway = Some(gateway);
        }
        self
    }

    pub fn with_native_mcp(mut self, native_mcp: Arc<NativeMcpRuntime>) -> Self {
        self.tools.native_mcp = Some(native_mcp);
        self
    }

    /// Attach the production native MCP executor while retaining explicitly
    /// supplied LLM/tool doubles. Used by end-to-end workflow tests and
    /// specialized deployments that wrap the standard runtime boundaries.
    pub fn with_native_mcp_from_pg_store(mut self, store: Arc<PgStore>) -> anyhow::Result<Self> {
        let broker = registry_token_broker(store.clone())?;
        let mcp_servers: Arc<dyn mcp::McpRegistryStore> = store.clone();
        let secrets: Arc<dyn SecretResolver> =
            Arc::new(BrokerSecretResolver::new(broker, mcp_servers.clone()));
        let private_networks = McpPrivateNetworkPolicy::from_env().map_err(anyhow::Error::msg)?;
        let trusted_header =
            crate::gateway::service::mcp_discovery::ConfiguratorTrustedHeaderPolicy::from_env()
                .map_err(anyhow::Error::msg)?;
        let universe_id = store.config().universe_id;
        let inventory = Arc::new(NativeMcpInventoryResolver::new(
            secrets.clone(),
            private_networks.clone(),
            trusted_header.clone(),
            universe_id,
        ));
        self.tools.native_mcp = Some(Arc::new(NativeMcpRuntime::new(
            mcp_servers,
            secrets,
            inventory,
            private_networks,
            trusted_header,
            universe_id,
        )));
        Ok(self)
    }

    pub fn from_pg_store_with_default_runtime(store: Arc<PgStore>) -> anyhow::Result<Self> {
        let blobs: Arc<dyn BlobStore> = store.clone();
        let broker = registry_token_broker(store.clone())?;
        let mcp_servers: Arc<dyn mcp::McpRegistryStore> = store.clone();
        let secrets: Arc<dyn SecretResolver> = Arc::new(BrokerSecretResolver::new(
            broker.clone(),
            mcp_servers.clone(),
        ));
        let private_networks = McpPrivateNetworkPolicy::from_env().map_err(anyhow::Error::msg)?;
        let trusted_header =
            crate::gateway::service::mcp_discovery::ConfiguratorTrustedHeaderPolicy::from_env()
                .map_err(anyhow::Error::msg)?;
        let universe_id = store.config().universe_id;
        let native_inventory = Arc::new(NativeMcpInventoryResolver::new(
            secrets.clone(),
            private_networks.clone(),
            trusted_header.clone(),
            universe_id,
        ));
        let inventory: Arc<dyn McpInventoryResolver> = native_inventory.clone();
        let native_mcp = Arc::new(NativeMcpRuntime::new(
            mcp_servers,
            secrets.clone(),
            native_inventory,
            private_networks,
            trusted_header,
            universe_id,
        ));
        let provider_keys = stored_provider_key_resolver(store.clone(), broker);
        let transcriber = default_audio_transcriber(provider_keys.clone())?;
        let transcoder = default_audio_transcoder_from_env()?;
        let llm = default_llm_runtime(blobs, Some(secrets), Some(provider_keys), Some(inventory))?;
        let hosted = Arc::new(SessionTools::from_pg_store(store.clone()));
        let tools: Arc<dyn CoreAgentTools> = hosted.clone();
        let mut state = Self::from_pg_store(store, llm, tools)
            .with_hosted_tools(hosted)
            .with_native_mcp(native_mcp)
            .with_audio_transcriber(transcriber);
        if let Some(transcoder) = transcoder {
            state = state.with_audio_transcoder(transcoder);
        }
        Ok(state)
    }

    /// Build a universe's activity state over the deployment's shared HTTP
    /// clients. Marginal per-universe cost is the resolver layers and tool
    /// registry only; every HTTP client is shared.
    pub fn from_pg_store_with_shared_clients(
        store: Arc<PgStore>,
        subagent_runtime: Option<Arc<dyn SubagentChildRuntime>>,
        clients: &DeploymentClients,
        temporal_client: temporalio_client::Client,
        gateway: EnvironmentGatewayClientConfig,
    ) -> anyhow::Result<Self> {
        let blobs: Arc<dyn BlobStore> = store.clone();
        let broker = registry_token_broker_with_clients(
            store.clone(),
            clients.oauth_token.clone(),
            clients.github.clone(),
        );
        let mcp_servers: Arc<dyn mcp::McpRegistryStore> = store.clone();
        let secrets: Arc<dyn SecretResolver> = Arc::new(BrokerSecretResolver::new(
            broker.clone(),
            mcp_servers.clone(),
        ));
        let private_networks = McpPrivateNetworkPolicy::from_env().map_err(anyhow::Error::msg)?;
        let trusted_header =
            crate::gateway::service::mcp_discovery::ConfiguratorTrustedHeaderPolicy::from_env()
                .map_err(anyhow::Error::msg)?;
        let universe_id = store.config().universe_id;
        let native_inventory = Arc::new(NativeMcpInventoryResolver::new(
            secrets.clone(),
            private_networks.clone(),
            trusted_header.clone(),
            universe_id,
        ));
        let inventory: Arc<dyn McpInventoryResolver> = native_inventory.clone();
        let native_mcp = Arc::new(NativeMcpRuntime::new(
            mcp_servers,
            secrets.clone(),
            native_inventory,
            private_networks,
            trusted_header,
            universe_id,
        ));
        let provider_keys = stored_provider_key_resolver(store.clone(), broker);
        let transcriber: Arc<dyn AudioTranscriber> = Arc::new(OpenAiAudioTranscriber::new(
            clients.openai_audio.clone(),
            provider_keys.clone(),
        ));
        let llm = llm_runtime_with_clients(
            blobs,
            Some(secrets),
            Some(provider_keys),
            Some(inventory),
            clients.openai.clone(),
            clients.openai_completions.clone(),
            clients.anthropic.clone(),
            crate::config::llm_debug_dumps_from_env()?,
        );
        let temporal_client_for_workflow_tools = temporal_client.clone();
        let hosted = Arc::new(
            SessionTools::from_pg_store(store.clone()).with_environment_gateway(gateway.clone()),
        );
        let tools: Arc<dyn CoreAgentTools> = hosted.clone();
        let mut state = Self::from_pg_store(store, llm, tools)
            .with_hosted_tools(hosted)
            .with_native_mcp(native_mcp)
            .with_audio_transcriber(transcriber)
            .with_workflow_tool_executions(temporal_client_for_workflow_tools);
        if let Some(subagent_runtime) = subagent_runtime {
            state = state.with_subagent_runtime(subagent_runtime);
        }
        state = state.with_environment_gateway(gateway);
        if let Some(transcoder) = clients.audio_transcoder.clone() {
            state = state.with_audio_transcoder(transcoder);
        }
        Ok(state)
    }

    pub async fn from_env() -> anyhow::Result<Self> {
        let store = pg_store_from_env().await?;
        Self::from_pg_store_with_default_runtime(store)
    }

    pub(super) fn storage(&self) -> &StorageActivityDeps {
        &self.storage
    }

    pub(super) fn llm(&self) -> &LlmActivityDeps {
        &self.llm
    }

    pub(super) fn tools(&self) -> &ToolActivityDeps {
        &self.tools
    }

    pub(super) fn runtime_projection(&self) -> Option<&RuntimeProjectionActivityDeps> {
        self.runtime_projection.as_ref()
    }

    pub(super) fn preprocess(&self) -> &PreprocessActivityDeps {
        &self.preprocess
    }

    pub(super) fn environment_jobs(&self) -> Option<&EnvironmentJobActivityDeps> {
        self.environment_jobs.as_ref()
    }

    pub(super) fn workflow_tool_executions(&self) -> Option<&WorkflowToolExecutionDeps> {
        self.workflow_tool_executions.as_ref()
    }

    pub(super) fn subagents(&self) -> Option<&SubagentActivityDeps> {
        self.subagents.as_ref()
    }
}

fn stored_provider_key_resolver(
    store: Arc<PgStore>,
    broker: Arc<dyn AuthTokenBroker>,
) -> Arc<dyn ModelProviderResolver> {
    let providers: Arc<dyn AuthProviderStore> = store.clone();
    let secrets: Arc<dyn SecretStore> = store;
    Arc::new(StoredProviderKeyResolver::new(providers, secrets, broker))
}

fn registry_token_broker(store: Arc<PgStore>) -> anyhow::Result<Arc<dyn AuthTokenBroker>> {
    let token_client: Arc<dyn auth::OAuthTokenClient> = Arc::new(
        HttpOAuthTokenClient::new()
            .map_err(|error| anyhow::anyhow!("construct oauth token client: {error}"))?,
    );
    let github_api: Arc<dyn auth::GitHubApiClient> = Arc::new(
        HttpGitHubApiClient::new()
            .map_err(|error| anyhow::anyhow!("construct github api client: {error}"))?,
    );
    Ok(registry_token_broker_with_clients(
        store,
        token_client,
        github_api,
    ))
}

fn registry_token_broker_with_clients(
    store: Arc<PgStore>,
    token_client: Arc<dyn auth::OAuthTokenClient>,
    github_api: Arc<dyn auth::GitHubApiClient>,
) -> Arc<dyn AuthTokenBroker> {
    let grants: Arc<dyn AuthGrantStore> = store.clone();
    let secrets: Arc<dyn SecretStore> = store.clone();
    let clients: Arc<dyn OAuthClientStore> = store.clone();
    let providers: Arc<dyn AuthProviderStore> = store.clone();
    let locks: Arc<dyn GrantRefreshLock> = store;
    let broker = RegistryTokenBroker::new(grants.clone(), secrets.clone(), locks)
        .with_oauth_refresh(OAuthRefreshRuntime::new(clients, token_client))
        .with_token_source(
            AuthProviderKind::GitHubApp,
            Arc::new(GitHubAppRuntime::new(
                providers, github_api, grants, secrets,
            )),
        );
    Arc::new(broker)
}

/// Builds the default LLM runtime. Adapters register unconditionally:
/// requests resolve a stored `model:<provider_id>` key first and fall back to
/// the env-configured client key, so a deployment can run on stored keys
/// alone. When neither exists, requests fail with a typed error before
/// provider I/O.
fn default_llm_runtime(
    blobs: Arc<dyn BlobStore>,
    secrets: Option<Arc<dyn SecretResolver>>,
    provider_keys: Option<Arc<dyn ModelProviderResolver>>,
    inventory: Option<Arc<dyn McpInventoryResolver>>,
) -> anyhow::Result<Arc<dyn CoreAgentLlm>> {
    let openai = Arc::new(oai::Client::new(oai::Config::from_env_allow_missing_key())?);
    let openai_completions = Arc::new(oai_completions::Client::new(
        oai_completions::Config::from_env_allow_missing_key(),
    )?);
    let anthropic = Arc::new(am::Client::new(am::Config::from_env_allow_missing_key())?);
    let debug_dumps = crate::config::llm_debug_dumps_from_env()?;
    Ok(llm_runtime_with_clients(
        blobs,
        secrets,
        provider_keys,
        inventory,
        openai,
        openai_completions,
        anthropic,
        debug_dumps,
    ))
}

#[allow(clippy::too_many_arguments)]
fn llm_runtime_with_clients(
    blobs: Arc<dyn BlobStore>,
    secrets: Option<Arc<dyn SecretResolver>>,
    provider_keys: Option<Arc<dyn ModelProviderResolver>>,
    inventory: Option<Arc<dyn McpInventoryResolver>>,
    openai: Arc<oai::Client>,
    openai_completions: Arc<oai_completions::Client>,
    anthropic: Arc<am::Client>,
    debug_dumps: bool,
) -> Arc<dyn CoreAgentLlm> {
    let mut registry = LlmAdapterRegistry::new();

    let mut adapter =
        OpenAiResponsesLlmAdapter::new(openai, blobs.clone()).with_debug_dumps(debug_dumps);
    if let Some(secrets) = &secrets {
        adapter = adapter.with_secret_resolver(secrets.clone());
    }
    if let Some(provider_keys) = &provider_keys {
        adapter = adapter.with_provider_key_resolver(provider_keys.clone());
    }
    if let Some(inventory) = &inventory {
        adapter = adapter.with_mcp_inventory_resolver(inventory.clone());
    }
    let adapter = Arc::new(adapter);
    registry.insert_generation_adapter(ProviderApiKind::OpenAiResponses, adapter.clone());
    registry.insert_compaction_adapter(ProviderApiKind::OpenAiResponses, adapter);

    let mut adapter = OpenAiCompletionsLlmAdapter::new(openai_completions, blobs.clone())
        .with_debug_dumps(debug_dumps);
    if let Some(provider_keys) = &provider_keys {
        adapter = adapter.with_provider_key_resolver(provider_keys.clone());
    }
    if let Some(inventory) = &inventory {
        adapter = adapter.with_mcp_inventory_resolver(inventory.clone());
    }
    let adapter = Arc::new(adapter);
    registry.insert_generation_adapter(ProviderApiKind::OpenAiCompletions, adapter.clone());
    registry.insert_compaction_adapter(ProviderApiKind::OpenAiCompletions, adapter);

    let mut adapter =
        AnthropicMessagesLlmAdapter::new(anthropic, blobs).with_debug_dumps(debug_dumps);
    if let Some(secrets) = &secrets {
        adapter = adapter.with_secret_resolver(secrets.clone());
    }
    if let Some(provider_keys) = &provider_keys {
        adapter = adapter.with_provider_key_resolver(provider_keys.clone());
    }
    if let Some(inventory) = &inventory {
        adapter = adapter.with_mcp_inventory_resolver(inventory.clone());
    }
    let adapter = Arc::new(adapter);
    registry.insert_generation_adapter(ProviderApiKind::AnthropicMessages, adapter.clone());
    registry.insert_compaction_adapter(ProviderApiKind::AnthropicMessages, adapter);

    Arc::new(LlmRuntime::new(registry))
}

fn default_audio_transcriber(
    provider_keys: Arc<dyn ModelProviderResolver>,
) -> anyhow::Result<Arc<dyn AudioTranscriber>> {
    default_openai_audio_transcriber(provider_keys)
}

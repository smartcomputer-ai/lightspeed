use std::sync::Arc;

use auth::{
    AuthGrantStore, AuthProviderKind, AuthProviderStore, AuthTokenBroker, GitHubAppRuntime,
    GrantRefreshLock, HttpGitHubApiClient, HttpOAuthTokenClient, OAuthClientStore,
    OAuthRefreshRuntime, RegistryTokenBroker, SecretStore,
};
use engine::{
    CoreAgentLlm, CoreAgentTools, ProviderApiKind,
    storage::{BlobStore, SessionStore},
};
use environments::SessionEnvironmentBindingStore;
use llm_clients::{anthropic::messages as am, openai::responses as oai};
use llm_runtime::{
    AnthropicMessagesLlmAdapter, LlmAdapterRegistry, LlmRuntime, OpenAiResponsesLlmAdapter,
    provider_keys::ProviderKeyResolver, secrets::SecretResolver,
};
use store_pg::PgStore;
use vfs::{VfsMountStore, VfsWorkspaceStore};

use crate::{
    config::pg_store_from_env,
    fleet::FleetChildRuntime,
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
}

#[derive(Clone)]
pub struct RuntimeProjectionActivityDeps {
    pub(super) blobs: Arc<dyn BlobStore>,
    pub(super) workspace_store: Arc<dyn VfsWorkspaceStore>,
    pub(super) mount_store: Arc<dyn VfsMountStore>,
    pub(super) environment_bindings: Arc<dyn SessionEnvironmentBindingStore>,
    pub(super) environment_instances: Arc<dyn environments::EnvironmentInstanceStore>,
}

#[derive(Clone)]
pub struct PreprocessActivityDeps {
    pub(super) blobs: Arc<dyn BlobStore>,
    pub(super) transcriber: Arc<dyn AudioTranscriber>,
    pub(super) transcoder: Option<Arc<dyn AudioTranscoder>>,
}

#[derive(Clone)]
pub struct ActivityState {
    storage: StorageActivityDeps,
    llm: LlmActivityDeps,
    tools: ToolActivityDeps,
    runtime_projection: Option<RuntimeProjectionActivityDeps>,
    preprocess: PreprocessActivityDeps,
    environment_jobs: Option<Arc<PgStore>>,
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
            },
            llm: LlmActivityDeps {
                llm,
                blobs: blobs.clone(),
            },
            tools: ToolActivityDeps {
                tools,
                blobs: blobs.clone(),
            },
            runtime_projection: None,
            preprocess: PreprocessActivityDeps {
                blobs: blobs.clone(),
                transcriber: Arc::new(UnavailableAudioTranscriber),
                transcoder: None,
            },
            environment_jobs: None,
        }
    }

    pub fn with_runtime_projection_deps(
        mut self,
        workspace_store: Arc<dyn VfsWorkspaceStore>,
        mount_store: Arc<dyn VfsMountStore>,
        environment_bindings: Arc<dyn SessionEnvironmentBindingStore>,
        environment_instances: Arc<dyn environments::EnvironmentInstanceStore>,
    ) -> Self {
        self.runtime_projection = Some(RuntimeProjectionActivityDeps {
            blobs: self.storage.blobs.clone(),
            workspace_store,
            mount_store,
            environment_bindings,
            environment_instances,
        });
        self
    }

    pub fn with_audio_transcriber(mut self, transcriber: Arc<dyn AudioTranscriber>) -> Self {
        self.preprocess.transcriber = transcriber;
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
        let sessions: Arc<dyn SessionStore> = store.clone();
        let blobs: Arc<dyn BlobStore> = store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = store.clone();
        let mount_store: Arc<dyn VfsMountStore> = store.clone();
        let environment_bindings: Arc<dyn SessionEnvironmentBindingStore> = store.clone();
        let environment_instances: Arc<dyn environments::EnvironmentInstanceStore> = store.clone();
        let mut state = Self::new(sessions, blobs, llm, tools).with_runtime_projection_deps(
            workspace_store,
            mount_store,
            environment_bindings,
            environment_instances,
        );
        state.environment_jobs = Some(store);
        state
    }

    pub fn from_pg_store_with_default_runtime(store: Arc<PgStore>) -> anyhow::Result<Self> {
        let blobs: Arc<dyn BlobStore> = store.clone();
        let broker = registry_token_broker(store.clone())?;
        let secrets: Arc<dyn SecretResolver> = Arc::new(BrokerSecretResolver::new(broker.clone()));
        let provider_keys = stored_provider_key_resolver(store.clone(), broker);
        let transcriber = default_audio_transcriber(provider_keys.clone())?;
        let transcoder = default_audio_transcoder_from_env()?;
        let llm = default_llm_runtime(blobs, Some(secrets), Some(provider_keys))?;
        let tools = session_tools(store.clone());
        let mut state = Self::from_pg_store(store, llm, tools).with_audio_transcriber(transcriber);
        if let Some(transcoder) = transcoder {
            state = state.with_audio_transcoder(transcoder);
        }
        Ok(state)
    }

    pub fn from_pg_store_with_default_runtime_and_fleet(
        store: Arc<PgStore>,
        fleet_runtime: Arc<dyn FleetChildRuntime>,
    ) -> anyhow::Result<Self> {
        let blobs: Arc<dyn BlobStore> = store.clone();
        let broker = registry_token_broker(store.clone())?;
        let secrets: Arc<dyn SecretResolver> = Arc::new(BrokerSecretResolver::new(broker.clone()));
        let provider_keys = stored_provider_key_resolver(store.clone(), broker);
        let transcriber = default_audio_transcriber(provider_keys.clone())?;
        let transcoder = default_audio_transcoder_from_env()?;
        let llm = default_llm_runtime(blobs, Some(secrets), Some(provider_keys))?;
        let tools = session_tools_with_fleet(store.clone(), fleet_runtime);
        let mut state = Self::from_pg_store(store, llm, tools).with_audio_transcriber(transcriber);
        if let Some(transcoder) = transcoder {
            state = state.with_audio_transcoder(transcoder);
        }
        Ok(state)
    }

    /// Build a universe's activity state over the deployment's shared HTTP
    /// clients. Marginal per-universe cost is the resolver layers and tool
    /// registry only; every HTTP client is shared (P90 follow-up).
    pub fn from_pg_store_with_shared_clients(
        store: Arc<PgStore>,
        fleet_runtime: Option<Arc<dyn FleetChildRuntime>>,
        clients: &DeploymentClients,
        temporal_client: temporalio_client::Client,
        task_queue: String,
        universe_id: uuid::Uuid,
    ) -> anyhow::Result<Self> {
        let blobs: Arc<dyn BlobStore> = store.clone();
        let broker = registry_token_broker_with_clients(
            store.clone(),
            clients.oauth_token.clone(),
            clients.github.clone(),
        );
        let secrets: Arc<dyn SecretResolver> = Arc::new(BrokerSecretResolver::new(broker.clone()));
        let provider_keys = stored_provider_key_resolver(store.clone(), broker);
        let transcriber: Arc<dyn AudioTranscriber> = Arc::new(OpenAiAudioTranscriber::new(
            clients.openai_audio.clone(),
            provider_keys.clone(),
        ));
        let llm = llm_runtime_with_clients(
            blobs,
            Some(secrets),
            Some(provider_keys),
            clients.openai.clone(),
            clients.anthropic.clone(),
        );
        let tools: Arc<dyn CoreAgentTools> = match fleet_runtime {
            Some(fleet_runtime) => Arc::new(
                SessionTools::from_pg_store_with_fleet_runtime(store.clone(), fleet_runtime)
                    .with_environment_job_workflow_runtime(
                        temporal_client.clone(),
                        task_queue.clone(),
                        universe_id,
                    ),
            ),
            None => Arc::new(
                SessionTools::from_pg_store(store.clone()).with_environment_job_workflow_runtime(
                    temporal_client,
                    task_queue,
                    universe_id,
                ),
            ),
        };
        let mut state = Self::from_pg_store(store, llm, tools).with_audio_transcriber(transcriber);
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

    pub(super) fn environment_jobs(&self) -> Option<&Arc<PgStore>> {
        self.environment_jobs.as_ref()
    }
}

fn session_tools(store: Arc<PgStore>) -> Arc<dyn CoreAgentTools> {
    Arc::new(SessionTools::from_pg_store(store))
}

fn session_tools_with_fleet(
    store: Arc<PgStore>,
    fleet_runtime: Arc<dyn FleetChildRuntime>,
) -> Arc<dyn CoreAgentTools> {
    Arc::new(SessionTools::from_pg_store_with_fleet_runtime(
        store,
        fleet_runtime,
    ))
}

fn stored_provider_key_resolver(
    store: Arc<PgStore>,
    broker: Arc<dyn AuthTokenBroker>,
) -> Arc<dyn ProviderKeyResolver> {
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
    provider_keys: Option<Arc<dyn ProviderKeyResolver>>,
) -> anyhow::Result<Arc<dyn CoreAgentLlm>> {
    let openai = Arc::new(oai::Client::new(oai::Config::from_env_allow_missing_key())?);
    let anthropic = Arc::new(am::Client::new(am::Config::from_env_allow_missing_key())?);
    Ok(llm_runtime_with_clients(
        blobs,
        secrets,
        provider_keys,
        openai,
        anthropic,
    ))
}

fn llm_runtime_with_clients(
    blobs: Arc<dyn BlobStore>,
    secrets: Option<Arc<dyn SecretResolver>>,
    provider_keys: Option<Arc<dyn ProviderKeyResolver>>,
    openai: Arc<oai::Client>,
    anthropic: Arc<am::Client>,
) -> Arc<dyn CoreAgentLlm> {
    let mut registry = LlmAdapterRegistry::new();

    let mut adapter = OpenAiResponsesLlmAdapter::new(openai, blobs.clone());
    if let Some(secrets) = &secrets {
        adapter = adapter.with_secret_resolver(secrets.clone());
    }
    if let Some(provider_keys) = &provider_keys {
        adapter = adapter.with_provider_key_resolver(provider_keys.clone());
    }
    let adapter = Arc::new(adapter);
    registry.insert_generation_adapter(ProviderApiKind::OpenAiResponses, adapter.clone());
    registry.insert_compaction_adapter(ProviderApiKind::OpenAiResponses, adapter);

    let mut adapter = AnthropicMessagesLlmAdapter::new(anthropic, blobs);
    if let Some(secrets) = &secrets {
        adapter = adapter.with_secret_resolver(secrets.clone());
    }
    if let Some(provider_keys) = &provider_keys {
        adapter = adapter.with_provider_key_resolver(provider_keys.clone());
    }
    let adapter = Arc::new(adapter);
    registry.insert_generation_adapter(ProviderApiKind::AnthropicMessages, adapter.clone());
    registry.insert_compaction_adapter(ProviderApiKind::AnthropicMessages, adapter);

    Arc::new(LlmRuntime::new(registry))
}

fn default_audio_transcriber(
    provider_keys: Arc<dyn ProviderKeyResolver>,
) -> anyhow::Result<Arc<dyn AudioTranscriber>> {
    default_openai_audio_transcriber(provider_keys)
}

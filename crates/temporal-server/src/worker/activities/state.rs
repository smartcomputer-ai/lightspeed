use std::sync::Arc;

use auth_registry::{
    AuthGrantStore, AuthProviderKind, AuthProviderStore, AuthTokenBroker, GitHubAppRuntime,
    GrantRefreshLock, HttpGitHubApiClient, HttpOAuthTokenClient, OAuthClientStore,
    OAuthRefreshRuntime, RegistryTokenBroker, SecretStore,
};
use engine::{
    CoreAgentLlm, CoreAgentTools, ProviderApiKind,
    storage::{BlobStore, SessionStore},
};
use llm_clients::{anthropic::messages as am, openai::responses as oai};
use llm_runtime::{
    AnthropicMessagesLlmAdapter, LlmAdapterRegistry, LlmRuntime, OpenAiResponsesLlmAdapter,
    provider_keys::ProviderKeyResolver, secrets::SecretResolver,
};
use store_pg::PgStore;
use vfs::{VfsMountStore, VfsWorkspaceStore};

use crate::{
    config::pg_store_from_env,
    worker::{BrokerSecretResolver, SessionTools, StoredProviderKeyResolver},
};

use super::preprocess::{
    AudioTranscoder, AudioTranscriber, UnavailableAudioTranscriber,
    default_audio_transcoder_from_env, default_openai_audio_transcriber,
};

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
pub struct SkillCatalogActivityDeps {
    pub(super) blobs: Arc<dyn BlobStore>,
    pub(super) workspace_store: Arc<dyn VfsWorkspaceStore>,
    pub(super) mount_store: Arc<dyn VfsMountStore>,
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
    skill_catalog: Option<SkillCatalogActivityDeps>,
    preprocess: PreprocessActivityDeps,
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
            skill_catalog: None,
            preprocess: PreprocessActivityDeps {
                blobs: blobs.clone(),
                transcriber: Arc::new(UnavailableAudioTranscriber),
                transcoder: None,
            },
        }
    }

    pub fn with_skill_catalog_deps(
        mut self,
        workspace_store: Arc<dyn VfsWorkspaceStore>,
        mount_store: Arc<dyn VfsMountStore>,
    ) -> Self {
        self.skill_catalog = Some(SkillCatalogActivityDeps {
            blobs: self.storage.blobs.clone(),
            workspace_store,
            mount_store,
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
        let mount_store: Arc<dyn VfsMountStore> = store;
        Self::new(sessions, blobs, llm, tools).with_skill_catalog_deps(workspace_store, mount_store)
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

    pub(super) fn skill_catalog(&self) -> Option<&SkillCatalogActivityDeps> {
        self.skill_catalog.as_ref()
    }

    pub(super) fn preprocess(&self) -> &PreprocessActivityDeps {
        &self.preprocess
    }
}

fn session_tools(store: Arc<PgStore>) -> Arc<dyn CoreAgentTools> {
    Arc::new(SessionTools::from_pg_store(store))
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
    let grants: Arc<dyn AuthGrantStore> = store.clone();
    let secrets: Arc<dyn SecretStore> = store.clone();
    let clients: Arc<dyn OAuthClientStore> = store.clone();
    let providers: Arc<dyn AuthProviderStore> = store.clone();
    let locks: Arc<dyn GrantRefreshLock> = store;
    let token_client = HttpOAuthTokenClient::new()
        .map_err(|error| anyhow::anyhow!("construct oauth token client: {error}"))?;
    let github_api = HttpGitHubApiClient::new()
        .map_err(|error| anyhow::anyhow!("construct github api client: {error}"))?;
    let broker = RegistryTokenBroker::new(grants.clone(), secrets.clone(), locks)
        .with_oauth_refresh(OAuthRefreshRuntime::new(clients, Arc::new(token_client)))
        .with_token_source(
            AuthProviderKind::GitHubApp,
            Arc::new(GitHubAppRuntime::new(
                providers,
                Arc::new(github_api),
                grants,
                secrets,
            )),
        );
    Ok(Arc::new(broker))
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
    let mut registry = LlmAdapterRegistry::new();

    let openai = oai::Client::new(oai::Config::from_env_allow_missing_key())?;
    let mut adapter = OpenAiResponsesLlmAdapter::new(Arc::new(openai), blobs.clone());
    if let Some(secrets) = &secrets {
        adapter = adapter.with_secret_resolver(secrets.clone());
    }
    if let Some(provider_keys) = &provider_keys {
        adapter = adapter.with_provider_key_resolver(provider_keys.clone());
    }
    let adapter = Arc::new(adapter);
    registry.insert_generation_adapter(ProviderApiKind::OpenAiResponses, adapter.clone());
    registry.insert_compaction_adapter(ProviderApiKind::OpenAiResponses, adapter);

    let anthropic = am::Client::new(am::Config::from_env_allow_missing_key())?;
    let mut adapter = AnthropicMessagesLlmAdapter::new(Arc::new(anthropic), blobs);
    if let Some(secrets) = &secrets {
        adapter = adapter.with_secret_resolver(secrets.clone());
    }
    if let Some(provider_keys) = &provider_keys {
        adapter = adapter.with_provider_key_resolver(provider_keys.clone());
    }
    let adapter = Arc::new(adapter);
    registry.insert_generation_adapter(ProviderApiKind::AnthropicMessages, adapter.clone());
    registry.insert_compaction_adapter(ProviderApiKind::AnthropicMessages, adapter);

    Ok(Arc::new(LlmRuntime::new(registry)))
}

fn default_audio_transcriber(
    provider_keys: Arc<dyn ProviderKeyResolver>,
) -> anyhow::Result<Arc<dyn AudioTranscriber>> {
    default_openai_audio_transcriber(provider_keys)
}

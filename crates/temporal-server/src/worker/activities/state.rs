use std::sync::Arc;

use auth_registry::{
    AuthGrantStore, AuthProviderKind, AuthProviderStore, GitHubAppRuntime, GrantRefreshLock,
    HttpGitHubApiClient, HttpOAuthTokenClient, OAuthClientStore, OAuthRefreshRuntime,
    RegistryTokenBroker, SecretStore,
};
use engine::{
    CoreAgentLlm, CoreAgentTools, ProviderApiKind,
    storage::{BlobStore, SessionStore},
};
use llm_clients::openai::responses as oai;
use llm_runtime::{
    LlmAdapterRegistry, LlmRuntime, OpenAiResponsesLlmAdapter, secrets::SecretResolver,
};
use store_pg::PgStore;
use vfs::{VfsMountStore, VfsWorkspaceStore};

use crate::{
    config::pg_store_from_env,
    worker::{BrokerSecretResolver, SessionTools},
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
pub struct ActivityState {
    storage: StorageActivityDeps,
    llm: LlmActivityDeps,
    tools: ToolActivityDeps,
    skill_catalog: Option<SkillCatalogActivityDeps>,
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
            tools: ToolActivityDeps { tools, blobs },
            skill_catalog: None,
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
        let secrets = broker_secret_resolver(store.clone())?;
        let llm = openai_responses_llm(blobs, Some(secrets))?;
        let tools = session_tools(store.clone());
        Ok(Self::from_pg_store(store, llm, tools))
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
}

fn session_tools(store: Arc<PgStore>) -> Arc<dyn CoreAgentTools> {
    Arc::new(SessionTools::from_pg_store(store))
}

fn broker_secret_resolver(store: Arc<PgStore>) -> anyhow::Result<Arc<dyn SecretResolver>> {
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
    Ok(Arc::new(BrokerSecretResolver::new(Arc::new(broker))))
}

fn openai_responses_llm(
    blobs: Arc<dyn BlobStore>,
    secrets: Option<Arc<dyn SecretResolver>>,
) -> anyhow::Result<Arc<dyn CoreAgentLlm>> {
    let openai = oai::Client::new(oai::Config::from_env()?)?;
    let mut adapter = OpenAiResponsesLlmAdapter::new(Arc::new(openai), blobs);
    if let Some(secrets) = secrets {
        adapter = adapter.with_secret_resolver(secrets);
    }
    let adapter = Arc::new(adapter);
    let runtime = LlmRuntime::new(
        LlmAdapterRegistry::new()
            .with_generation_adapter(ProviderApiKind::OpenAiResponses, adapter.clone())
            .with_compaction_adapter(ProviderApiKind::OpenAiResponses, adapter),
    );
    Ok(Arc::new(runtime))
}

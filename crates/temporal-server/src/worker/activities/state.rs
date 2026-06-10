use std::sync::Arc;

use engine::{
    CoreAgentLlm, CoreAgentTools, ProviderApiKind,
    storage::{BlobStore, SessionStore},
};
use llm_clients::{anthropic::messages as am, openai::responses as oai};
use llm_runtime::{
    AnthropicMessagesLlmAdapter, LlmAdapterRegistry, LlmRuntime, OpenAiResponsesLlmAdapter,
};
use store_pg::PgStore;
use vfs::{VfsMountStore, VfsWorkspaceStore};

use crate::{config::pg_store_from_env, worker::SessionTools};

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
        let llm = default_llm_runtime(blobs)?;
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

/// Builds the default LLM runtime from environment credentials. Registers an
/// adapter per provider API kind whose credentials are configured and fails
/// when none are available.
fn default_llm_runtime(blobs: Arc<dyn BlobStore>) -> anyhow::Result<Arc<dyn CoreAgentLlm>> {
    let mut registry = LlmAdapterRegistry::new();

    if std::env::var("OPENAI_API_KEY").is_ok_and(|key| !key.trim().is_empty()) {
        let openai = oai::Client::new(oai::Config::from_env()?)?;
        let adapter = Arc::new(OpenAiResponsesLlmAdapter::new(
            Arc::new(openai),
            blobs.clone(),
        ));
        registry.insert_generation_adapter(ProviderApiKind::OpenAiResponses, adapter.clone());
        registry.insert_compaction_adapter(ProviderApiKind::OpenAiResponses, adapter);
    }
    if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|key| !key.trim().is_empty()) {
        let anthropic = am::Client::new(am::Config::from_env()?)?;
        let adapter = Arc::new(AnthropicMessagesLlmAdapter::new(Arc::new(anthropic), blobs));
        registry.insert_generation_adapter(ProviderApiKind::AnthropicMessages, adapter.clone());
        registry.insert_compaction_adapter(ProviderApiKind::AnthropicMessages, adapter);
    }

    if registry
        .generation_adapter(&ProviderApiKind::OpenAiResponses)
        .is_none()
        && registry
            .generation_adapter(&ProviderApiKind::AnthropicMessages)
            .is_none()
    {
        anyhow::bail!(
            "no LLM provider credentials configured: set OPENAI_API_KEY and/or ANTHROPIC_API_KEY"
        );
    }
    Ok(Arc::new(LlmRuntime::new(registry)))
}

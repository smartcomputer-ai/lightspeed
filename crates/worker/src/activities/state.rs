use std::sync::Arc;

use engine::{
    CoreAgentLlm, CoreAgentTools, ProviderApiKind,
    storage::{BlobStore, SessionStore},
};
use llm_clients::openai::responses as oai;
use llm_runtime::{LlmAdapterRegistry, LlmRuntime, OpenAiResponsesLlmAdapter};
use store_pg::PgStore;

use crate::{SessionMountedVfsTools, pg_store_from_env};

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
pub struct ActivityState {
    storage: StorageActivityDeps,
    llm: LlmActivityDeps,
    tools: ToolActivityDeps,
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
        }
    }

    pub fn from_pg_store(
        store: Arc<PgStore>,
        llm: Arc<dyn CoreAgentLlm>,
        tools: Arc<dyn CoreAgentTools>,
    ) -> Self {
        let sessions: Arc<dyn SessionStore> = store.clone();
        let blobs: Arc<dyn BlobStore> = store;
        Self::new(sessions, blobs, llm, tools)
    }

    pub async fn from_env() -> anyhow::Result<Self> {
        let store = pg_store_from_env().await?;
        let blobs: Arc<dyn BlobStore> = store.clone();
        let llm = openai_responses_llm(blobs)?;
        let tools = session_mounted_vfs_tools(store.clone());
        Ok(Self::from_pg_store(store, llm, tools))
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
}

fn session_mounted_vfs_tools(store: Arc<PgStore>) -> Arc<dyn CoreAgentTools> {
    Arc::new(SessionMountedVfsTools::from_pg_store(store))
}

fn openai_responses_llm(blobs: Arc<dyn BlobStore>) -> anyhow::Result<Arc<dyn CoreAgentLlm>> {
    let openai = oai::Client::new(oai::Config::from_env()?)?;
    let adapter = Arc::new(OpenAiResponsesLlmAdapter::new(Arc::new(openai), blobs));
    let runtime = LlmRuntime::new(
        LlmAdapterRegistry::new()
            .with_generation_adapter(ProviderApiKind::OpenAiResponses, adapter),
    );
    Ok(Arc::new(runtime))
}

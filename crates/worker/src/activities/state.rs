use std::{env, sync::Arc};

use engine::{
    CoreAgentLlm, CoreAgentTools, ProviderApiKind,
    storage::{BlobStore, SessionStore},
};
use llm_clients::openai::responses as oai;
use llm_runtime::{LlmAdapterRegistry, LlmRuntime, OpenAiResponsesLlmAdapter};
use store_pg::PgStore;

use crate::{FakeLlm, FakeTools, pg_store_from_env};

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
        let llm = llm_from_env(blobs.clone())?;
        let tools = Arc::new(FakeTools::new(blobs)) as Arc<dyn CoreAgentTools>;
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

fn llm_from_env(blobs: Arc<dyn BlobStore>) -> anyhow::Result<Arc<dyn CoreAgentLlm>> {
    let llm_mode = env::var("FORGE_LLM").unwrap_or_else(|_| "fake".to_owned());
    match llm_mode.as_str() {
        "fake" => Ok(Arc::new(FakeLlm::new(blobs))),
        "openai" => {
            let openai = oai::Client::new(oai::Config::from_env()?)?;
            let adapter = Arc::new(OpenAiResponsesLlmAdapter::new(Arc::new(openai), blobs));
            let runtime = LlmRuntime::new(
                LlmAdapterRegistry::new()
                    .with_generation_adapter(ProviderApiKind::OpenAiResponses, adapter),
            );
            Ok(Arc::new(runtime))
        }
        other => Err(anyhow::anyhow!("unsupported FORGE_LLM value: {other}")),
    }
}

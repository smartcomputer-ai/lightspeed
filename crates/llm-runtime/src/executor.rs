use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use engine::{
    ContextCompactionRequest, ContextCompactionResult, CoreAgentIoError, CoreAgentLlm,
    LlmGenerationRequest, LlmGenerationResult, ProviderApiKind,
};

use crate::{error::LlmAdapterResult, result::LlmGenerationExecution};

#[async_trait]
pub trait LlmGenerationAdapter: Send + Sync {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> LlmAdapterResult<LlmGenerationExecution>;
}

#[async_trait]
pub trait LlmCompactionAdapter: Send + Sync {
    async fn compact_context(
        &self,
        request: ContextCompactionRequest,
    ) -> LlmAdapterResult<ContextCompactionResult>;
}

#[derive(Clone, Default)]
pub struct LlmAdapterRegistry {
    generation: BTreeMap<ProviderApiKind, Arc<dyn LlmGenerationAdapter>>,
    compaction: BTreeMap<ProviderApiKind, Arc<dyn LlmCompactionAdapter>>,
}

impl LlmAdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_generation_adapter(
        mut self,
        api_kind: ProviderApiKind,
        adapter: Arc<dyn LlmGenerationAdapter>,
    ) -> Self {
        self.insert_generation_adapter(api_kind, adapter);
        self
    }

    pub fn with_compaction_adapter(
        mut self,
        api_kind: ProviderApiKind,
        adapter: Arc<dyn LlmCompactionAdapter>,
    ) -> Self {
        self.insert_compaction_adapter(api_kind, adapter);
        self
    }

    pub fn insert_generation_adapter(
        &mut self,
        api_kind: ProviderApiKind,
        adapter: Arc<dyn LlmGenerationAdapter>,
    ) {
        self.generation.insert(api_kind, adapter);
    }

    pub fn insert_compaction_adapter(
        &mut self,
        api_kind: ProviderApiKind,
        adapter: Arc<dyn LlmCompactionAdapter>,
    ) {
        self.compaction.insert(api_kind, adapter);
    }

    pub fn generation_adapter(
        &self,
        api_kind: &ProviderApiKind,
    ) -> Option<&Arc<dyn LlmGenerationAdapter>> {
        self.generation.get(api_kind)
    }

    pub fn compaction_adapter(
        &self,
        api_kind: &ProviderApiKind,
    ) -> Option<&Arc<dyn LlmCompactionAdapter>> {
        self.compaction.get(api_kind)
    }
}

#[derive(Clone)]
pub struct LlmRuntime {
    registry: LlmAdapterRegistry,
}

impl LlmRuntime {
    pub fn new(registry: LlmAdapterRegistry) -> Self {
        Self { registry }
    }

    async fn generate_request(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        let Some(adapter) = self
            .registry
            .generation_adapter(&request.request.model.api_kind)
        else {
            return Err(CoreAgentIoError::Failed {
                message: format!(
                    "no LLM generation adapter registered for {:?}",
                    request.request.model.api_kind
                ),
            });
        };

        adapter
            .generate(request)
            .await
            .map(|execution| execution.result)
            .map_err(|error| CoreAgentIoError::Failed {
                message: error.to_string(),
            })
    }

    async fn compact_context_request(
        &self,
        request: ContextCompactionRequest,
    ) -> Result<ContextCompactionResult, CoreAgentIoError> {
        let Some(adapter) = self
            .registry
            .compaction_adapter(&request.request.model.api_kind)
        else {
            return Err(CoreAgentIoError::Failed {
                message: format!(
                    "no LLM compaction adapter registered for {:?}",
                    request.request.model.api_kind
                ),
            });
        };

        adapter
            .compact_context(request)
            .await
            .map_err(|error| CoreAgentIoError::Failed {
                message: error.to_string(),
            })
    }
}

#[async_trait]
impl CoreAgentLlm for LlmRuntime {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        self.generate_request(request).await
    }

    async fn compact_context(
        &self,
        request: ContextCompactionRequest,
    ) -> Result<ContextCompactionResult, CoreAgentIoError> {
        self.compact_context_request(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LlmAdapterError;
    use engine::{ContextSnapshot, LlmRequest, ModelSelection, RunId, SessionId, TurnId};

    struct FailingAdapter;

    #[async_trait]
    impl LlmGenerationAdapter for FailingAdapter {
        async fn generate(
            &self,
            _request: LlmGenerationRequest,
        ) -> LlmAdapterResult<LlmGenerationExecution> {
            Err(LlmAdapterError::Provider {
                message: "boom".to_owned(),
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn adapter_errors_are_returned_to_the_worker() {
        let registry = LlmAdapterRegistry::new()
            .with_generation_adapter(ProviderApiKind::OpenAiResponses, Arc::new(FailingAdapter));
        let runtime = LlmRuntime::new(registry);

        let error = CoreAgentLlm::generate(&runtime, request())
            .await
            .expect_err("adapter errors must not become anonymous failed generations");

        assert!(error.to_string().contains("provider call failed: boom"));
    }

    fn request() -> LlmGenerationRequest {
        LlmGenerationRequest {
            session_id: SessionId::new("session-a"),
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            request: LlmRequest {
                model: ModelSelection {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    provider_id: "openai".to_owned(),
                    model: "gpt-test".to_owned(),
                },
                request_fingerprint: "sha256:test".to_owned(),
                context: ContextSnapshot {
                    api_kind: ProviderApiKind::OpenAiResponses,
                    context_revision: 0,
                    entries: Vec::new(),
                    token_estimate: None,
                },
                tools: Vec::new(),
                tool_choice: None,
                output_limit: None,
                provider_response_id: None,
                compaction: None,
                params: None,
            },
        }
    }
}

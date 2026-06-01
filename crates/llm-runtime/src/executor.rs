use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use engine::{
    CoreAgentIoError, CoreAgentLlm, LlmGenerationRequest, LlmGenerationResult, ProviderApiKind,
};

use crate::{error::LlmAdapterResult, result::LlmGenerationExecution};

#[async_trait]
pub trait LlmGenerationAdapter: Send + Sync {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> LlmAdapterResult<LlmGenerationExecution>;
}

#[derive(Clone, Default)]
pub struct LlmAdapterRegistry {
    generation: BTreeMap<ProviderApiKind, Arc<dyn LlmGenerationAdapter>>,
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

    pub fn insert_generation_adapter(
        &mut self,
        api_kind: ProviderApiKind,
        adapter: Arc<dyn LlmGenerationAdapter>,
    ) {
        self.generation.insert(api_kind, adapter);
    }

    pub fn generation_adapter(
        &self,
        api_kind: &ProviderApiKind,
    ) -> Option<&Arc<dyn LlmGenerationAdapter>> {
        self.generation.get(api_kind)
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
}

#[async_trait]
impl CoreAgentLlm for LlmRuntime {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        self.generate_request(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LlmAdapterError;
    use engine::{
        LlmRequest, LlmRequestKind, ModelProviderOptions, ModelSelection, OpenAiResponsesRequest,
        ResolvedContextWindow, RunId, SessionId, TurnId,
    };

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
                    options: ModelProviderOptions::None,
                },
                request_fingerprint: "sha256:test".to_owned(),
                kind: LlmRequestKind::OpenAiResponses(OpenAiResponsesRequest {
                    instructions_ref: None,
                    input_window: ResolvedContextWindow {
                        api_kind: ProviderApiKind::OpenAiResponses,
                        items: Vec::new(),
                        token_estimate: None,
                    },
                    previous_response_id: None,
                    tools: Vec::new(),
                    tool_choice: None,
                    reasoning: None,
                    text: None,
                    include: Vec::new(),
                    max_output_tokens: None,
                    max_tool_calls: None,
                    temperature: None,
                    top_p: None,
                    metadata: BTreeMap::new(),
                    parallel_tool_calls: None,
                    store: None,
                    stream: None,
                    truncation: None,
                    context_management: None,
                    extra: BTreeMap::new(),
                }),
            },
        }
    }
}

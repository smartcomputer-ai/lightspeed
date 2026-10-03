use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use harness::{
    ContextCompactionRequest, ContextCompactionResult, CoreAgentIoError, CoreAgentLlm,
    LlmGenerationRequest, LlmGenerationResult, ProviderApiKind,
};

use crate::{
    error::{LlmAdapterError, LlmAdapterResult},
    result::LlmGenerationExecution,
};

#[async_trait]
pub trait LlmGenerationAdapter: Send + Sync {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> LlmAdapterResult<LlmGenerationExecution>;
}

#[async_trait]
pub trait LlmCompactionAdapter: Send + Sync {
    fn blobs(&self) -> Option<&dyn harness::storage::BlobStore> {
        None
    }
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
            .map_err(io_error_from_adapter_error)
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

        tokio::time::timeout(
            std::time::Duration::from_secs(600),
            crate::compaction::compact(adapter.as_ref(), request),
        )
        .await
        .map_err(|_| CoreAgentIoError::Failed {
            message: "compaction exceeded its ten-minute operation budget".into(),
        })?
        .map_err(io_error_from_adapter_error)
    }
}

/// Preserves the client-derived retry disposition and request rejections
/// across the generic I/O boundary. Only provider errors with explicit
/// transient evidence become `Retryable`; a provider refusing the request
/// itself becomes `Rejected` with the provider's message unchanged; every
/// other adapter error stays terminal.
fn io_error_from_adapter_error(error: LlmAdapterError) -> CoreAgentIoError {
    match &error {
        LlmAdapterError::ContextLimit { message } => CoreAgentIoError::ContextLimit {
            message: message.clone(),
        },
        LlmAdapterError::Provider { source } if source.retryable() => CoreAgentIoError::Retryable {
            retry_after: source.retry_after(),
            message: error.to_string(),
        },
        LlmAdapterError::Provider { source } => match source.request_rejection() {
            Some(rejection)
                if rejection.kind == llm_clients::ProviderFailureKind::ContextLength =>
            {
                CoreAgentIoError::ContextLimit {
                    message: rejection.message.clone(),
                }
            }
            Some(rejection) => CoreAgentIoError::Rejected {
                message: rejection.message.clone(),
            },
            None => CoreAgentIoError::Failed {
                message: error.to_string(),
            },
        },
        _ => CoreAgentIoError::Failed {
            message: error.to_string(),
        },
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
    use harness::{ContextSnapshot, LlmRequest, ModelSelection, RunId, SessionId, TurnId};

    struct FailingAdapter {
        error: llm_clients::LlmApiError,
    }

    #[async_trait]
    impl LlmGenerationAdapter for FailingAdapter {
        async fn generate(
            &self,
            _request: LlmGenerationRequest,
        ) -> LlmAdapterResult<LlmGenerationExecution> {
            Err(LlmAdapterError::Provider {
                source: Box::new(self.error.clone()),
            })
        }
    }

    async fn failing_generate(error: llm_clients::LlmApiError) -> CoreAgentIoError {
        let registry = LlmAdapterRegistry::new().with_generation_adapter(
            ProviderApiKind::OpenAiResponses,
            Arc::new(FailingAdapter { error }),
        );
        let runtime = LlmRuntime::new(registry);
        CoreAgentLlm::generate(&runtime, request())
            .await
            .expect_err("adapter errors must not become anonymous failed generations")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_adapter_errors_stay_terminal() {
        let error = failing_generate(llm_clients::TransportError::new("boom", false).into()).await;
        assert!(matches!(&error, CoreAgentIoError::Failed { .. }));
        assert!(error.to_string().contains("provider call failed"));
        assert!(error.to_string().contains("boom"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retryable_provider_errors_preserve_the_retry_disposition() {
        let error =
            failing_generate(llm_clients::TransportError::new("connection reset", true).into())
                .await;
        let CoreAgentIoError::Retryable {
            message,
            retry_after,
        } = error
        else {
            panic!("retryable client errors must map to CoreAgentIoError::Retryable");
        };
        assert!(message.contains("connection reset"));
        assert_eq!(retry_after, None);
    }

    fn http_error(
        status: u16,
        kind: llm_clients::ProviderFailureKind,
        message: &str,
    ) -> llm_clients::LlmApiError {
        llm_clients::ProviderHttpError {
            api_kind: "anthropic:messages".to_owned(),
            status,
            kind,
            message: message.to_owned(),
            error_code: None,
            error_type: Some("invalid_request_error".to_owned()),
            retryable: kind.default_retryable(),
            retry_after: None,
            raw_json: None,
            raw_text: None,
            headers: Default::default(),
        }
        .into()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_request_rejections_keep_the_provider_message() {
        let message = "messages.3.content.1.image.source.base64: image dimensions exceed max \
                       allowed size for many-image requests: 2000 pixels";
        let error = failing_generate(http_error(
            400,
            llm_clients::ProviderFailureKind::InvalidRequest,
            message,
        ))
        .await;
        assert_eq!(
            error,
            CoreAgentIoError::Rejected {
                message: message.to_owned()
            }
        );

        let context = "prompt is too long: maximum context length is 200000 tokens";
        let error = failing_generate(http_error(
            400,
            llm_clients::ProviderFailureKind::ContextLength,
            context,
        ))
        .await;
        assert_eq!(
            error,
            CoreAgentIoError::ContextLimit {
                message: context.to_owned()
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn other_terminal_provider_errors_stay_failed() {
        for (status, kind) in [
            (401, llm_clients::ProviderFailureKind::Authentication),
            (403, llm_clients::ProviderFailureKind::AccessDenied),
            (404, llm_clients::ProviderFailureKind::NotFound),
            (400, llm_clients::ProviderFailureKind::ContentFilter),
        ] {
            let error = failing_generate(http_error(status, kind, "denied")).await;
            assert!(
                matches!(error, CoreAgentIoError::Failed { .. }),
                "{status}: {error:?}"
            );
        }
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
                reasoning_effort: None,
                parallel_tool_use: None,
                processing_tier: None,
                provider_response_id: None,
                compaction: None,
                params: None,
            },
        }
    }
}

use std::{collections::BTreeMap, sync::Arc};

use agent_core::{
    CoreAgentIoError, CoreAgentLlm, LlmGenerationRequest, LlmGenerationResult, ProviderApiKind,
};
use async_trait::async_trait;

use crate::{
    error::LlmAdapterResult,
    result::{LlmGenerationExecution, failed_generation_result},
};

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

    async fn generate_request(&self, request: LlmGenerationRequest) -> LlmGenerationResult {
        let Some(adapter) = self
            .registry
            .generation_adapter(&request.request.model.api_kind)
        else {
            return failed_generation_result(request.run_id, request.turn_id);
        };

        let run_id = request.run_id;
        let turn_id = request.turn_id;
        match adapter.generate(request).await {
            Ok(execution) => execution.result,
            Err(_error) => failed_generation_result(run_id, turn_id),
        }
    }
}

#[async_trait]
impl CoreAgentLlm for LlmRuntime {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, CoreAgentIoError> {
        Ok(self.generate_request(request).await)
    }
}

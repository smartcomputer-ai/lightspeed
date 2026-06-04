use engine::{
    BlobRef, LlmFinish, LlmGenerationFacts, LlmGenerationResult, LlmGenerationStatus, RunId, TurnId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmGenerationExecution {
    pub result: LlmGenerationResult,
    pub provider_request_ref: BlobRef,
    pub raw_response_ref: BlobRef,
}

pub fn failed_generation_result(run_id: RunId, turn_id: TurnId) -> LlmGenerationResult {
    LlmGenerationResult {
        run_id,
        turn_id,
        status: LlmGenerationStatus::Failed,
        failure_ref: None,
        context_entries: Vec::new(),
        facts: LlmGenerationFacts {
            provider_response_id: None,
            finish: LlmFinish::Failed,
            usage: None,
            tool_calls: Vec::new(),
            context_token_estimate: None,
        },
    }
}

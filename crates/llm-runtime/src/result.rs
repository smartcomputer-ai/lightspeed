use std::collections::BTreeMap;

use engine::{
    BlobRef, ContextEntryId, LlmFinish, LlmGenerationFacts, LlmGenerationRequest,
    LlmGenerationResult, LlmGenerationStatus, RunId, TurnId, storage::BlobStore,
};
use serde::Serialize;

use crate::{blob_io::put_json, error::LlmAdapterResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmGenerationExecution {
    pub result: LlmGenerationResult,
    /// The raw provider request and response, stored only when the adapter
    /// runs with debug dumps enabled. Nothing durable references them, so
    /// they live exactly one collection grace period.
    pub debug_dumps: Option<LlmDebugDumps>,
}

/// Refs of one generation's raw provider exchange. The request is the
/// redacted form: resolved credentials never reach the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmDebugDumps {
    pub provider_request_ref: BlobRef,
    pub raw_response_ref: BlobRef,
}

/// Capture the outgoing request for a debug dump before it is consumed by
/// the transport. Costs nothing when dumps are off.
pub(crate) fn debug_dump_request<T>(
    enabled: bool,
    request: &T,
) -> LlmAdapterResult<Option<serde_json::Value>>
where
    T: Serialize + ?Sized,
{
    if !enabled {
        return Ok(None);
    }
    serde_json::to_value(request).map(Some).map_err(|error| {
        crate::error::LlmAdapterError::InvalidProviderRequest {
            message: format!("failed to encode provider request dump: {error}"),
        }
    })
}

/// Store the captured request and the raw response as unrooted blobs and log
/// their refs against the session, run, and turn they belong to.
pub(crate) async fn store_debug_dumps(
    blobs: &dyn BlobStore,
    request_dump: Option<serde_json::Value>,
    raw_response: &serde_json::Value,
    generation: &LlmGenerationRequest,
    provider: &'static str,
) -> LlmAdapterResult<Option<LlmDebugDumps>> {
    let Some(request_dump) = request_dump else {
        return Ok(None);
    };
    let provider_request_ref = put_json(blobs, &request_dump).await?;
    let raw_response_ref = put_json(blobs, raw_response).await?;
    tracing::debug!(
        target: "llm_runtime",
        provider,
        session_id = %generation.session_id,
        run_id = %generation.run_id,
        turn_id = %generation.turn_id,
        %provider_request_ref,
        %raw_response_ref,
        "stored LLM debug dumps"
    );
    Ok(Some(LlmDebugDumps {
        provider_request_ref,
        raw_response_ref,
    }))
}

pub fn failed_generation_result(run_id: RunId, turn_id: TurnId) -> LlmGenerationResult {
    LlmGenerationResult {
        run_id,
        turn_id,
        status: LlmGenerationStatus::Failed,
        failure_ref: None,
        context_entries: Vec::new(),
        facts: LlmGenerationFacts {
            duration_ms: None,
            provider_response_id: None,
            finish: LlmFinish::Failed,
            usage: None,
            tool_calls: Vec::new(),
            approval_requests: Vec::new(),
            context_token_estimate: None,
        },
    }
}

/// Failure text for a turn the provider cut off at its output cap, in the
/// worker's provider-error blob layout so clients render it like any other
/// model failure. `cap` is the cap the request carried (`None` when the
/// provider applied its own maximum).
pub fn truncation_failure_text(
    run_id: RunId,
    turn_id: TurnId,
    provider: &str,
    response_id: &str,
    cap: Option<u64>,
    output_tokens: Option<u32>,
    reasoning_tokens: Option<u32>,
) -> String {
    let cap = cap
        .map(|cap| cap.to_string())
        .unwrap_or_else(|| "the model maximum".to_owned());
    let spent = match (output_tokens, reasoning_tokens) {
        (Some(output), Some(reasoning)) => {
            format!(" after {output} output tokens ({reasoning} thinking)")
        }
        (Some(output), None) => format!(" after {output} output tokens"),
        _ => String::new(),
    };
    format!(
        "core agent LLM generation failed\nrun_id={run_id}\nturn_id={turn_id}\n\
         error={provider} response {response_id} was cut off at max output tokens {cap}{spent}; \
         the partial output is kept — raise maxOutputTokens or lower reasoningEffort\n"
    )
}

/// The entries of a truncated turn that are safe and useful to keep: the
/// assistant's partial text. Tool calls have no results to replay against,
/// and thinking or provider-opaque blocks from an unfinished turn are not
/// replay-safe.
pub async fn partial_output_entries(
    blobs: &dyn BlobStore,
    entries: Vec<engine::ContextEntryInput>,
) -> LlmAdapterResult<Vec<engine::ContextEntryInput>> {
    let mut partial = Vec::new();
    for mut entry in entries {
        if !matches!(
            entry.kind,
            engine::ContextEntryKind::Message {
                role: engine::ContextMessageRole::Assistant
            }
        ) {
            continue;
        }
        if entry.content.media_type.as_deref() == Some("application/json") {
            let raw = crate::blob_io::read_json(blobs, &entry.content.content_ref).await?;
            let text = match entry.content.provider_kind.as_deref() {
                Some(engine::ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND) => {
                    llm_clients::content::anthropic_text_blocks(&raw)
                }
                Some(engine::OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND) => {
                    llm_clients::content::openai_response_message(&raw)
                }
                Some(llm_clients::content::OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND) => {
                    llm_clients::content::openai_completion_message(&raw)
                }
                _ => None,
            }
            .ok_or_else(|| crate::error::LlmAdapterError::InvalidProviderRequest {
                message: "invalid partial assistant content".to_owned(),
            })?;
            entry.content.content_ref = crate::blob_io::put_text(blobs, &text).await?;
            entry.content.media_type = Some("text/plain".to_owned());
            entry.content.provider_kind = None;
        }
        partial.push(entry);
    }
    Ok(partial)
}

/// Where each context entry was lowered in a provider request: the index of
/// the message or input item that carries it.
pub(crate) type RequestPositions = Vec<(ContextEntryId, usize)>;

/// Log, beside a provider's rejection, which entries each request position
/// holds. Provider messages cite request positions (`messages.3.content.1`),
/// not entry IDs, and only the adapter knows how entries were merged into
/// provider messages. The mapping is a log line rather than durable state
/// because its shape is provider-specific.
pub(crate) fn log_request_rejection(
    request: &LlmGenerationRequest,
    field: &str,
    positions: &[(ContextEntryId, usize)],
    message: &str,
) {
    let positions = format_request_positions(field, positions);
    tracing::warn!(
        session_id = %request.session_id,
        run_id = %request.run_id,
        turn_id = %request.turn_id,
        model = %request.request.model.model,
        provider_message = message,
        entry_positions = %positions,
        "provider rejected the request"
    );
}

/// `messages.0=[1,2] messages.1=[3]`: the entry IDs each position holds.
fn format_request_positions(field: &str, positions: &[(ContextEntryId, usize)]) -> String {
    let mut by_position = BTreeMap::<usize, Vec<String>>::new();
    for (entry_id, index) in positions {
        by_position
            .entry(*index)
            .or_default()
            .push(entry_id.to_string());
    }
    by_position
        .into_iter()
        .map(|(index, entries)| format!("{field}.{index}=[{}]", entries.join(",")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_positions_group_entries_by_position() {
        let positions =
            [(1, 0), (2, 0), (5, 1), (7, 3)].map(|(id, index)| (ContextEntryId::new(id), index));
        assert_eq!(
            format_request_positions("messages", &positions),
            "messages.0=[1,2] messages.1=[5] messages.3=[7]"
        );
    }
}

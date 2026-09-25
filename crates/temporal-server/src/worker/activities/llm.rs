use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use engine::{CoreAgentIoError, LlmGenerationResult, LlmUsage, SessionId};
use temporalio_sdk::activities::ActivityError;

use crate::worker::LlmGenerateActivityRequest;

use super::{
    common::{activity_error, failed_generation_result_from_error, transient_provider_failure},
    state::LlmActivityDeps,
};

pub(super) async fn generate(
    deps: &LlmActivityDeps,
    attempt: u32,
    request: LlmGenerateActivityRequest,
) -> Result<LlmGenerationResult, ActivityError> {
    let request = request.request;
    let session_id = request.session_id.clone();
    let started = std::time::Instant::now();
    match deps.llm.generate(request.clone()).await {
        Ok(mut result) => {
            result.facts.duration_ms = Some(elapsed_ms(started));
            if let Some(usage) = result.facts.usage.as_ref() {
                observe_prompt_cache(&session_id, usage);
            }
            Ok(result)
        }
        // Transient provider errors become the typed retryable activity
        // failure; Temporal owns the durable backoff.
        Err(CoreAgentIoError::Retryable {
            message,
            retry_after,
        }) => Err(transient_provider_failure(
            "LLM generation",
            attempt,
            message,
            retry_after,
        )),
        // Terminal errors complete the activity with a failed generation
        // result and are never retried.
        Err(error) => failed_generation_result_from_error(deps.blobs.as_ref(), request, error)
            .await
            .map(|mut result| {
                result.facts.duration_ms = Some(elapsed_ms(started));
                result
            })
            .map_err(activity_error),
    }
}

fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Prompts at least this long are expected to hit the provider cache once
/// the session has warmed it (Anthropic's floor is 1024–2048 tokens, OpenAI's
/// 1024); smaller prompts never cache and are not worth a warning.
const PROMPT_CACHE_WARN_MIN_INPUT_TOKENS: u32 = 2048;

#[derive(Clone, Copy)]
struct LastPromptCacheSample {
    input_tokens: u32,
    cached_input_tokens: u32,
}

/// Process-local memory of the last generation per session, enough to spot a
/// broken prefix: a large prompt that reads nothing from the cache right
/// after a turn that did. Not durable and not shared across workers by
/// design — it is a cheap regression detector, not accounting.
fn prompt_cache_samples() -> &'static Mutex<HashMap<SessionId, LastPromptCacheSample>> {
    static SAMPLES: OnceLock<Mutex<HashMap<SessionId, LastPromptCacheSample>>> = OnceLock::new();
    SAMPLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn observe_prompt_cache(session_id: &SessionId, usage: &LlmUsage) {
    let Some(input_tokens) = usage.input_tokens else {
        return;
    };
    let cached_input_tokens = usage.cached_input_tokens.unwrap_or(0);
    let cached_share = if input_tokens == 0 {
        0.0
    } else {
        f64::from(cached_input_tokens) / f64::from(input_tokens)
    };
    tracing::debug!(
        session_id = %session_id,
        input_tokens,
        cached_input_tokens,
        cache_write_input_tokens = usage.cache_write_input_tokens.unwrap_or(0),
        cached_share = format_args!("{:.0}%", cached_share * 100.0),
        "llm prompt cache"
    );
    let sample = LastPromptCacheSample {
        input_tokens,
        cached_input_tokens,
    };
    let previous = match prompt_cache_samples().lock() {
        Ok(mut samples) => samples.insert(session_id.clone(), sample),
        Err(_) => return,
    };
    if let Some(previous) = previous
        && previous.cached_input_tokens > 0
        && cached_input_tokens == 0
        && input_tokens >= PROMPT_CACHE_WARN_MIN_INPUT_TOKENS
    {
        tracing::warn!(
            session_id = %session_id,
            input_tokens,
            previous_input_tokens = previous.input_tokens,
            previous_cached_input_tokens = previous.cached_input_tokens,
            "prompt cache miss after a hit: the rendered prefix changed (instructions rewrite, \
             compaction, or a catalog rewritten in place)"
        );
    }
}

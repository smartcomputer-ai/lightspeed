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
    let LlmGenerateActivityRequest {
        request,
        attached_resources,
    } = request;
    let session_id = request.session_id.clone();
    // The turn boundary: the run's execution authority must still hold
    // before the model is called. The turn that was authorized completes;
    // this one does not begin.
    if let Some((access, universe_id)) = &deps.access
        && let Some(revoked) =
            revoked_authority(access, *universe_id, &session_id, &attached_resources).await?
    {
        return revoked_generation_result(deps.blobs.as_ref(), request, &revoked).await;
    }
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

/// Why a model call for the session may not begin, or `None` while its
/// execution principal is active with resource use in the universe and may
/// use every resource the session has attached that still exists. A
/// personal root runs as its owner, so the identity's rights cover both the
/// identity and its membership. The turn check and compaction both ask it.
pub(super) async fn revoked_authority(
    store: &store_pg::PgAccessStore,
    universe_id: uuid::Uuid,
    session_id: &engine::SessionId,
    attached: &[access::ResourceRef],
) -> Result<Option<String>, ActivityError> {
    let refusal = store
        .execution_use(
            universe_id,
            &access::ResourceRef::Session(session_id.as_str().to_owned()),
            attached,
            store_pg::UseCheck::Continuation,
        )
        .await
        .map_err(|error| {
            activity_error(anyhow::Error::new(error).context("resolve run authority"))
        })?;
    Ok(refusal.map(|refusal| match refusal {
        store_pg::UseRefusal::Identity => {
            "the session's execution identity is disabled or may no longer use resources".to_owned()
        }
        store_pg::UseRefusal::Resource { resource, .. } => format!(
            "the session's execution identity may no longer use {} {}",
            resource.label(),
            resource.id()
        ),
    }))
}

async fn revoked_generation_result(
    blobs: &dyn engine::storage::BlobStore,
    request: engine::LlmGenerationRequest,
    reason: &str,
) -> Result<LlmGenerationResult, ActivityError> {
    let failure_ref = super::common::write_error_blob(
        blobs,
        format!(
            "run authority revoked before the model call: {reason}\nrun_id={}\nturn_id={}\n",
            request.run_id, request.turn_id
        ),
    )
    .await
    .map_err(|error| {
        activity_error(anyhow::Error::new(error).context("record revoked authority"))
    })?;
    Ok(LlmGenerationResult {
        run_id: request.run_id,
        turn_id: request.turn_id,
        status: engine::LlmGenerationStatus::AuthorityRevoked,
        failure_ref: Some(failure_ref),
        context_entries: Vec::new(),
        facts: engine::LlmGenerationFacts {
            duration_ms: None,
            provider_response_id: None,
            finish: engine::LlmFinish::Failed,
            usage: None,
            context_token_estimate: None,
            tool_calls: Vec::new(),
            approval_requests: Vec::new(),
        },
    })
}

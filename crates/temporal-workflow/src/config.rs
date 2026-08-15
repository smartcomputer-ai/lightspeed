use std::time::Duration;

use engine::{ModelSelection, RunConfig, SessionConfig, ToolExecutionClass, ToolExecutionSpec};
use temporalio_common::protos::temporal::api::common::v1::RetryPolicy;
use temporalio_sdk::{ActivityCloseTimeouts, ActivityOptions};

pub const DEFAULT_TASK_QUEUE: &str = "lightspeed-agent";
pub const DEFAULT_TEMPORAL_TARGET: &str = "localhost:7233";
pub const DEFAULT_TEMPORAL_NAMESPACE: &str = "default";
pub const DEFAULT_MODEL: &str = "gpt-5.5";
pub const DEFAULT_CONTINUE_AS_NEW_HISTORY_THRESHOLD: u32 = 10_000;
pub const DEFAULT_ACTIVITY_START_TO_CLOSE_TIMEOUT: Duration = Duration::from_secs(360);

/// Conservative ceiling on the serialized compact bootstrap result (replayed
/// `CoreAgentState` plus small indices). Temporal's default activity-result
/// payload limit is 2 MiB; we guard well below it so a near-limit reduced state
/// fails with a typed `SessionBootstrapPayloadTooLarge` error before Temporal
/// rejects the activity completion with an opaque size error. Reduced state is
/// bounded by active context (entry metadata + content refs), not by total log
/// length, so this budget should never be hit in normal operation.
pub const DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES: u64 = 1_500_000;

pub const FAKE_TOOL_NAME: &str = "agent_echo";

pub fn default_run_config() -> RunConfig {
    RunConfig::default()
}

/// The secure-by-default session config: a model that can process runs and
/// nothing else. Every capability is an explicitly granted feature.
pub fn default_session_config(model: ModelSelection) -> SessionConfig {
    SessionConfig {
        model,
        generation: Default::default(),
        limits: Default::default(),
        context: Default::default(),
        features: Default::default(),
    }
}

pub fn default_instructions() -> &'static str {
    "You are Lightspeed, a concise personal assistant. Use available tools when useful, then answer plainly."
}

pub fn activity_options() -> ActivityOptions {
    ActivityOptions::start_to_close_timeout(DEFAULT_ACTIVITY_START_TO_CLOSE_TIMEOUT)
}

// Tool execution classes (P114). Every tool activity carries a bounded
// operation deadline, a Temporal start-to-close, a schedule-to-close total
// deadline, and a bounded retry policy:
//
//   operation soft deadline < start-to-close <= schedule-to-close
//
// The operation deadline is enforced by the worker and returns an ordinary
// terminal tool failure; Temporal timeouts and exhausted retries are converted
// at the workflow boundary into terminal call results.

/// Worker-enforced operation deadline for interactive (filesystem, control,
/// concurrency) calls. Applies to local and remote transports alike.
pub const TOOL_INTERACTIVE_OPERATION_TIMEOUT: Duration = Duration::from_secs(90);
pub const TOOL_INTERACTIVE_START_TO_CLOSE: Duration = Duration::from_secs(120);
pub const TOOL_INTERACTIVE_SCHEDULE_TO_CLOSE: Duration = Duration::from_secs(600);

/// Worker-enforced operation deadline for bounded network-shaped calls (web,
/// environment jobs, fleet messaging).
pub const TOOL_REMOTE_OPERATION_TIMEOUT: Duration = Duration::from_secs(120);
pub const TOOL_REMOTE_START_TO_CLOSE: Duration = Duration::from_secs(150);
pub const TOOL_REMOTE_SCHEDULE_TO_CLOSE: Duration = Duration::from_secs(600);

/// Deployment-owned ceiling on a validated `run_process` timeout. The worker
/// clamps requested process timeouts to the same ceiling
/// (`tools::ToolLimits::max_process_timeout_ms`, asserted equal by a
/// temporal-server test); the process activity deadline derives from it.
pub const PROCESS_TIMEOUT_CEILING: Duration = Duration::from_secs(30 * 60);
/// Transport/completion grace added on top of the process timeout ceiling.
pub const TOOL_PROCESS_GRACE: Duration = Duration::from_secs(60);

/// Bounded attempts for calls whose admitted binding marks re-dispatch safe
/// (read-only operations). Everything else uses one attempt until downstream
/// idempotency exists. No tool activity has an unlimited retry policy.
pub const TOOL_RETRY_SAFE_MAX_ATTEMPTS: i32 = 3;

/// Deployment-owned ceiling on concurrently executing per-call tool
/// activities within one batch. Parallel-safe groups wider than this run as a
/// topped-up window, so one model turn cannot fan out unbounded host
/// connections or scans.
pub const MAX_CONCURRENT_TOOL_CALLS_PER_BATCH: usize = 8;

/// Bounded options for materializing a boundary-failure error blob. The
/// failure-conversion path must never reintroduce Temporal's default
/// unlimited retries; when these attempts are exhausted the workflow falls
/// back to the engine's well-known boundary-failure blob.
pub fn boundary_error_blob_activity_options() -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::Both {
        start_to_close: Duration::from_secs(30),
        schedule_to_close: Duration::from_secs(120),
    })
    .retry_policy(tool_retry_policy(TOOL_RETRY_SAFE_MAX_ATTEMPTS))
    .build()
}

// LLM provider activity policy (P116). Generation and compaction execute one
// bounded provider call per attempt; Temporal owns durable backoff between
// attempts. Only the typed `llm_provider_transient` application failure is
// retryable — terminal provider errors complete the activity with a failed
// generation result and never retry.

/// Start-to-close covers one provider attempt: the configured transport
/// request ceiling (120s) plus completion grace.
pub const LLM_START_TO_CLOSE: Duration = Duration::from_secs(150);
/// The authoritative total retry budget across all attempts and backoff.
pub const LLM_SCHEDULE_TO_CLOSE: Duration = Duration::from_secs(15 * 60);
pub const LLM_RETRY_INITIAL_INTERVAL: Duration = Duration::from_secs(2);
pub const LLM_RETRY_MAX_INTERVAL: Duration = Duration::from_secs(60);
pub const LLM_RETRY_BACKOFF_COEFFICIENT: f64 = 2.0;
pub const LLM_RETRY_MAX_ATTEMPTS: i32 = 15;

/// Temporal options for the `llm_generate` and `context_compact` provider
/// activities. Bounded in attempts and wall-clock time; never Temporal's
/// default unlimited retries.
pub fn llm_activity_options() -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::Both {
        start_to_close: LLM_START_TO_CLOSE,
        schedule_to_close: LLM_SCHEDULE_TO_CLOSE,
    })
    .retry_policy(RetryPolicy {
        initial_interval: Some(
            LLM_RETRY_INITIAL_INTERVAL
                .try_into()
                .expect("static duration fits the proto range"),
        ),
        backoff_coefficient: LLM_RETRY_BACKOFF_COEFFICIENT,
        maximum_interval: Some(
            LLM_RETRY_MAX_INTERVAL
                .try_into()
                .expect("static duration fits the proto range"),
        ),
        maximum_attempts: LLM_RETRY_MAX_ATTEMPTS,
        non_retryable_error_types: Vec::new(),
    })
    .build()
}

fn tool_retry_policy(max_attempts: i32) -> RetryPolicy {
    RetryPolicy {
        initial_interval: Some(
            Duration::from_secs(1)
                .try_into()
                .expect("static duration fits the proto range"),
        ),
        backoff_coefficient: 2.0,
        maximum_interval: Some(
            Duration::from_secs(10)
                .try_into()
                .expect("static duration fits the proto range"),
        ),
        maximum_attempts: max_attempts,
        non_retryable_error_types: Vec::new(),
    }
}

/// Worker-enforced operation deadline for one tool call of the given class.
pub fn tool_call_operation_timeout(class: ToolExecutionClass) -> Duration {
    match class {
        ToolExecutionClass::Interactive => TOOL_INTERACTIVE_OPERATION_TIMEOUT,
        ToolExecutionClass::RemoteInteractive => TOOL_REMOTE_OPERATION_TIMEOUT,
        ToolExecutionClass::Process => PROCESS_TIMEOUT_CEILING + TOOL_PROCESS_GRACE,
    }
}

/// Temporal options for one per-call tool activity, selected from the
/// admitted execution binding rather than model-controlled input.
pub fn tool_call_activity_options(execution: ToolExecutionSpec) -> ActivityOptions {
    let (start_to_close, schedule_to_close) = match execution.class {
        ToolExecutionClass::Interactive => (
            TOOL_INTERACTIVE_START_TO_CLOSE,
            TOOL_INTERACTIVE_SCHEDULE_TO_CLOSE,
        ),
        ToolExecutionClass::RemoteInteractive => {
            (TOOL_REMOTE_START_TO_CLOSE, TOOL_REMOTE_SCHEDULE_TO_CLOSE)
        }
        ToolExecutionClass::Process => {
            let start_to_close = PROCESS_TIMEOUT_CEILING + TOOL_PROCESS_GRACE * 2;
            (start_to_close, start_to_close + TOOL_PROCESS_GRACE * 2)
        }
    };
    let max_attempts = if execution.retry_safe && execution.class != ToolExecutionClass::Process {
        TOOL_RETRY_SAFE_MAX_ATTEMPTS
    } else {
        1
    };
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::Both {
        start_to_close,
        schedule_to_close,
    })
    .retry_policy(tool_retry_policy(max_attempts))
    .build()
}

/// Temporal options for a batch-unit tool activity (await batches and
/// workflow-tool batches execute as one unit). Bounded: one attempt and an
/// explicit total deadline, never Temporal's default unlimited retries.
pub fn tool_batch_activity_options() -> ActivityOptions {
    ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::Both {
        start_to_close: DEFAULT_ACTIVITY_START_TO_CLOSE_TIMEOUT,
        schedule_to_close: DEFAULT_ACTIVITY_START_TO_CLOSE_TIMEOUT + Duration::from_secs(60),
    })
    .retry_policy(tool_retry_policy(1))
    .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    use temporalio_sdk::ActivityCloseTimeouts;

    #[test]
    fn activity_options_use_extended_start_to_close_timeout() {
        assert_eq!(
            activity_options().close_timeouts,
            ActivityCloseTimeouts::StartToClose(DEFAULT_ACTIVITY_START_TO_CLOSE_TIMEOUT)
        );
    }

    fn all_execution_specs() -> impl Iterator<Item = ToolExecutionSpec> {
        [
            ToolExecutionClass::Interactive,
            ToolExecutionClass::RemoteInteractive,
            ToolExecutionClass::Process,
        ]
        .into_iter()
        .flat_map(|class| {
            [false, true]
                .into_iter()
                .map(move |retry_safe| ToolExecutionSpec::new(class, retry_safe))
        })
    }

    #[test]
    fn tool_call_options_keep_the_deadline_invariant_for_every_class() {
        for execution in all_execution_specs() {
            let options = tool_call_activity_options(execution);
            let ActivityCloseTimeouts::Both {
                start_to_close,
                schedule_to_close,
            } = options.close_timeouts
            else {
                panic!("tool call options must bound both timeouts");
            };
            let operation = tool_call_operation_timeout(execution.class);
            assert!(
                operation < start_to_close,
                "{execution:?}: operation deadline must undercut start-to-close"
            );
            assert!(
                start_to_close <= schedule_to_close,
                "{execution:?}: start-to-close must not exceed schedule-to-close"
            );
        }
    }

    #[test]
    fn no_tool_activity_has_an_unlimited_retry_policy() {
        for execution in all_execution_specs() {
            let retry = tool_call_activity_options(execution)
                .retry_policy
                .expect("tool call options declare a retry policy");
            let expected = if execution.retry_safe && execution.class != ToolExecutionClass::Process
            {
                TOOL_RETRY_SAFE_MAX_ATTEMPTS
            } else {
                1
            };
            assert_eq!(retry.maximum_attempts, expected, "{execution:?}");
        }
        let batch_retry = tool_batch_activity_options()
            .retry_policy
            .expect("batch options declare a retry policy");
        assert_eq!(batch_retry.maximum_attempts, 1);
        let blob_retry = boundary_error_blob_activity_options()
            .retry_policy
            .expect("boundary blob options declare a retry policy");
        assert_eq!(blob_retry.maximum_attempts, TOOL_RETRY_SAFE_MAX_ATTEMPTS);
        assert!(matches!(
            boundary_error_blob_activity_options().close_timeouts,
            ActivityCloseTimeouts::Both { .. }
        ));
    }

    #[test]
    fn llm_options_keep_the_deadline_invariant_and_a_bounded_retry_policy() {
        let options = llm_activity_options();
        let ActivityCloseTimeouts::Both {
            start_to_close,
            schedule_to_close,
        } = options.close_timeouts
        else {
            panic!("llm options must bound both timeouts");
        };
        assert_eq!(start_to_close, LLM_START_TO_CLOSE);
        assert_eq!(schedule_to_close, LLM_SCHEDULE_TO_CLOSE);
        assert!(start_to_close <= schedule_to_close);
        // Schedule-to-close is the authoritative total budget; it must leave
        // room for more than one attempt or retries are silently disabled.
        assert!(schedule_to_close >= start_to_close * 2);

        let retry = options
            .retry_policy
            .expect("llm options declare a retry policy");
        assert_eq!(retry.maximum_attempts, LLM_RETRY_MAX_ATTEMPTS);
        assert!(retry.maximum_attempts > 1);
        assert_eq!(
            retry.maximum_interval,
            Some(
                LLM_RETRY_MAX_INTERVAL
                    .try_into()
                    .expect("static duration fits the proto range")
            )
        );
    }

    #[test]
    fn retry_safe_calls_get_headroom_for_their_attempts() {
        let execution = ToolExecutionSpec::new(ToolExecutionClass::Interactive, true);
        let ActivityCloseTimeouts::Both {
            start_to_close,
            schedule_to_close,
        } = tool_call_activity_options(execution).close_timeouts
        else {
            panic!("tool call options must bound both timeouts");
        };
        assert!(schedule_to_close >= start_to_close * TOOL_RETRY_SAFE_MAX_ATTEMPTS as u32);
    }
}

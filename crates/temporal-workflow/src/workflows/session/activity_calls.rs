use temporalio_common::{error::IncomingError, protos::temporal::api::enums::v1::TimeoutType};
use temporalio_sdk::ActivityExecutionError;

use super::*;

/// Longest boundary-failure message recorded for the transcript; put_blob
/// inputs stay bounded even for pathological provider error chains.
const MAX_LLM_BOUNDARY_ERROR_BYTES: usize = 16 * 1024;

/// LLM provider activities: terminal provider errors complete the activity
/// with a failed result; transient ones surface as the typed retryable
/// `llm_provider_transient` failure so Temporal owns durable backoff. When
/// the activity finally fails — the transient retry budget is exhausted, or
/// the activity timed out because the worker went away or the provider call
/// hung past its budget — the failure is converted here into the same
/// terminal result shape the drive consumes: the run fails, the session
/// workflow survives. Cancellation and unrecognized application errors
/// propagate unchanged so operational bugs stay visible.
///
/// Run the generation activity while client admissions keep landing.
/// A cancel that makes the turn obsolete preempts the call; the
/// engine has already cancelled the turn by then.
pub(super) async fn call_llm_generate(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    drive: &mut CoreAgentDrive,
    request: LlmGenerationRequest,
) -> anyhow::Result<control::Raced<engine::LlmGenerationResult>> {
    let run_id = request.run_id;
    let turn_id = request.turn_id;
    let activity_ctx = ctx.clone();
    let activity = activity_ctx.start_activity(
        WorkflowActivities::llm_generate,
        LlmGenerateActivityRequest { request },
        crate::llm_activity_options(),
    );
    let raced = control::race_activity_with_admissions(ctx, drive, activity, |state| {
        control::generation_still_wanted(state, run_id, turn_id)
    })
    .await?;
    let outcome = match raced {
        control::Raced::Preempted => return Ok(control::Raced::Preempted),
        control::Raced::Completed(outcome) => outcome,
    };
    match outcome {
        Ok(result) => Ok(control::Raced::Completed(result)),
        Err(error) => match llm_boundary_failure(&error) {
            Some(failure) => {
                let failure_ref =
                    put_llm_boundary_error_blob(ctx, "LLM generation", &failure).await;
                Ok(control::Raced::Completed(engine::LlmGenerationResult {
                    run_id,
                    turn_id,
                    status: engine::LlmGenerationStatus::Failed,
                    failure_ref: Some(failure_ref),
                    context_entries: Vec::new(),
                    facts: engine::LlmGenerationFacts {
                        duration_ms: None,
                        provider_response_id: None,
                        finish: engine::LlmFinish::Failed,
                        usage: None,
                        tool_calls: Vec::new(),
                        approval_requests: Vec::new(),
                        context_token_estimate: None,
                    },
                }))
            }
            None => Err(anyhow::anyhow!("{error}")),
        },
    }
}

pub(super) async fn call_context_compact(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: engine::ContextCompactionRequest,
) -> anyhow::Result<engine::ContextCompactionResult> {
    let session_id = request.session_id.clone();
    let context_revision = request.request.context.context_revision;
    match ctx
        .start_activity(
            WorkflowActivities::context_compact,
            crate::ContextCompactActivityRequest { request },
            crate::llm_activity_options(),
        )
        .await
    {
        Ok(result) => Ok(result),
        Err(error) => match llm_boundary_failure(&error) {
            Some(failure) => {
                let failure_ref =
                    put_llm_boundary_error_blob(ctx, "context compaction", &failure).await;
                Ok(engine::ContextCompactionResult {
                    session_id,
                    context_revision,
                    status: engine::ContextCompactionStatus::Failed,
                    failure_ref: Some(failure_ref),
                    context_entries: Vec::new(),
                })
            }
            None => Err(anyhow::anyhow!("{error}")),
        },
    }
}

/// A provider-activity failure the run absorbs as a failed generation or
/// compaction instead of failing the session workflow.
#[derive(Debug, PartialEq, Eq)]
enum LlmBoundaryFailure {
    /// The typed `llm_provider_transient` failure exhausted its retry budget.
    TransientExhausted(crate::LlmTransientFailureDetails),
    /// The activity timed out with no provider failure in the chain: the
    /// worker stopped heartbeating (outage) or one attempt ran past its
    /// start-to-close and the schedule-to-close budget ended the retries.
    TimedOut {
        timeout_type: TimeoutType,
        cause: Option<TimeoutType>,
    },
}

/// Recognizes a boundary failure anywhere in the activity failure's cause
/// chain. An exhausted transient provider failure wins when present: when
/// schedule-to-close expires during a backoff, the typed application failure
/// arrives as the cause of a timeout failure rather than at the top level.
/// A chain made only of timeouts (heartbeat → schedule-to-close) is a worker
/// outage or hung attempt. Cancellation is never converted, and a chain that
/// carries any other application failure — even under a timeout — is an
/// operational bug that propagates.
fn llm_boundary_failure(error: &ActivityExecutionError) -> Option<LlmBoundaryFailure> {
    if matches!(error, ActivityExecutionError::Cancelled(_)) {
        return None;
    }
    let mut timeouts = Vec::new();
    let mut cause = error.cause();
    while let Some(incoming) = cause {
        match incoming {
            IncomingError::Application(failure) => {
                if failure.type_name() != Some(crate::LLM_PROVIDER_TRANSIENT_ERROR_TYPE) {
                    return None;
                }
                let details = failure
                    .details::<crate::LlmTransientFailureDetails>()
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| crate::LlmTransientFailureDetails {
                        version: crate::LLM_TRANSIENT_FAILURE_DETAILS_VERSION,
                        message: failure.to_string(),
                        attempt: 0,
                        retry_after_ms: None,
                    });
                return Some(LlmBoundaryFailure::TransientExhausted(details));
            }
            IncomingError::Timeout(timeout) => timeouts.push(timeout.timeout_type()),
            IncomingError::Cancelled(_) => return None,
            _ => {}
        }
        cause = incoming.cause();
    }
    let mut timeouts = timeouts.into_iter();
    let timeout_type = timeouts.next()?;
    Some(LlmBoundaryFailure::TimedOut {
        timeout_type,
        cause: timeouts.last(),
    })
}

fn timeout_type_label(timeout_type: TimeoutType) -> &'static str {
    match timeout_type {
        TimeoutType::StartToClose => "start-to-close",
        TimeoutType::ScheduleToStart => "schedule-to-start",
        TimeoutType::ScheduleToClose => "schedule-to-close",
        TimeoutType::Heartbeat => "heartbeat",
        TimeoutType::Unspecified => "unspecified",
    }
}

fn llm_boundary_error_message(operation: &str, failure: &LlmBoundaryFailure) -> String {
    let mut message = match failure {
        LlmBoundaryFailure::TransientExhausted(details) => format!(
            "{operation} failed: transient provider retries exhausted after {} attempts\nlast provider error: {}\n",
            details.attempt, details.message
        ),
        LlmBoundaryFailure::TimedOut {
            timeout_type,
            cause,
        } => {
            let cause = cause
                .map(|cause| {
                    format!(
                        ", last attempt hit its {} timeout",
                        timeout_type_label(cause)
                    )
                })
                .unwrap_or_default();
            format!(
                "{operation} failed: provider activity timed out ({} timeout{cause}); the worker was unavailable or the provider call hung past its budget\n",
                timeout_type_label(*timeout_type)
            )
        }
    };
    if message.len() > MAX_LLM_BOUNDARY_ERROR_BYTES {
        let mut end = MAX_LLM_BOUNDARY_ERROR_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    message
}

/// Materialize the boundary failure text with bounded attempts; fall back
/// to the well-known engine blob so the failure path itself can never retry
/// unbounded.
async fn put_llm_boundary_error_blob(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    operation: &str,
    failure: &LlmBoundaryFailure,
) -> BlobRef {
    let message = llm_boundary_error_message(operation, failure);
    ctx.start_activity(
        WorkflowActivities::put_blob,
        PutBlobRequest {
            bytes: message.into_bytes(),
        },
        crate::boundary_error_blob_activity_options(),
    )
    .await
    .unwrap_or_else(|_| engine::llm_runtime_boundary_failure_ref())
}

pub(super) async fn call_tool_prepare_promise_controls(
    ctx: &mut WorkflowContext<AgentSessionWorkflow>,
    request: engine::PromiseControlArgumentRequest,
) -> anyhow::Result<engine::PromiseControlArgumentFacts> {
    ctx.start_activity(
        WorkflowActivities::tool_prepare_promise_controls,
        ToolPreparePromiseControlsActivityRequest { request },
        activity_options(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))
}

#[cfg(test)]
mod tests {
    use temporalio_common::{
        data_converters::{
            ActivityExecutionDecodeHint, DefaultFailureConverter, FailureConverter,
            FailureDecodeHint, PayloadConverter, SerializationContextData,
        },
        protos::temporal::api::{
            enums::v1::RetryState,
            failure::v1::{
                ActivityFailureInfo, ApplicationFailureInfo, CanceledFailureInfo, Failure,
                TimeoutFailureInfo, failure::FailureInfo,
            },
        },
    };

    use super::*;

    /// Decode a proto failure exactly the way the SDK does for an activity
    /// resolution, so the recognizer is exercised against the real shape.
    fn activity_error(failure: Failure) -> ActivityExecutionError {
        let incoming = DefaultFailureConverter
            .to_error(
                failure,
                &PayloadConverter::default(),
                &SerializationContextData::None,
            )
            .expect("failure decodes");
        ActivityExecutionDecodeHint { cancelled: false }.adapt(incoming)
    }

    fn activity_failure(retry_state: RetryState, cause: Failure) -> Failure {
        Failure {
            message: "Activity task timed out".to_owned(),
            cause: Some(Box::new(cause)),
            failure_info: Some(FailureInfo::ActivityFailureInfo(ActivityFailureInfo {
                activity_type: Some("llm_generate".into()),
                retry_state: retry_state.into(),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    fn timeout_failure(timeout_type: TimeoutType, cause: Option<Failure>) -> Failure {
        Failure {
            message: format!("activity {} timeout", timeout_type_label(timeout_type)),
            cause: cause.map(Box::new),
            failure_info: Some(FailureInfo::TimeoutFailureInfo(TimeoutFailureInfo {
                timeout_type: timeout_type.into(),
                last_heartbeat_details: None,
            })),
            ..Default::default()
        }
    }

    fn application_failure(type_name: &str) -> Failure {
        Failure {
            message: format!("{type_name} failure"),
            failure_info: Some(FailureInfo::ApplicationFailureInfo(
                ApplicationFailureInfo {
                    r#type: type_name.to_owned(),
                    ..Default::default()
                },
            )),
            ..Default::default()
        }
    }

    #[test]
    fn transient_exhaustion_is_recognized_at_the_top_of_the_chain() {
        let error = activity_error(activity_failure(
            RetryState::MaximumAttemptsReached,
            application_failure(crate::LLM_PROVIDER_TRANSIENT_ERROR_TYPE),
        ));
        assert!(matches!(
            llm_boundary_failure(&error),
            Some(LlmBoundaryFailure::TransientExhausted(_))
        ));
    }

    #[test]
    fn transient_exhaustion_wins_over_a_wrapping_timeout() {
        // Schedule-to-close expired during a backoff: the typed failure is
        // the cause of the timeout, and it is the more specific story.
        let error = activity_error(activity_failure(
            RetryState::Timeout,
            timeout_failure(
                TimeoutType::ScheduleToClose,
                Some(application_failure(
                    crate::LLM_PROVIDER_TRANSIENT_ERROR_TYPE,
                )),
            ),
        ));
        assert!(matches!(
            llm_boundary_failure(&error),
            Some(LlmBoundaryFailure::TransientExhausted(_))
        ));
    }

    #[test]
    fn pure_timeout_chain_fails_the_run_instead_of_the_workflow() {
        // A worker outage: no heartbeat, then the schedule-to-close budget
        // ends the retries. Nothing in the chain is an application failure.
        let error = activity_error(activity_failure(
            RetryState::Timeout,
            timeout_failure(
                TimeoutType::ScheduleToClose,
                Some(timeout_failure(TimeoutType::Heartbeat, None)),
            ),
        ));
        assert_eq!(
            llm_boundary_failure(&error),
            Some(LlmBoundaryFailure::TimedOut {
                timeout_type: TimeoutType::ScheduleToClose,
                cause: Some(TimeoutType::Heartbeat),
            })
        );
        let message = llm_boundary_error_message(
            "LLM generation",
            &llm_boundary_failure(&error).expect("recognized"),
        );
        assert!(message.contains("schedule-to-close timeout"), "{message}");
        assert!(message.contains("heartbeat timeout"), "{message}");
    }

    #[test]
    fn single_timeout_has_no_cause() {
        let error = activity_error(activity_failure(
            RetryState::Timeout,
            timeout_failure(TimeoutType::StartToClose, None),
        ));
        assert_eq!(
            llm_boundary_failure(&error),
            Some(LlmBoundaryFailure::TimedOut {
                timeout_type: TimeoutType::StartToClose,
                cause: None,
            })
        );
    }

    #[test]
    fn cancellation_and_unknown_application_failures_propagate() {
        let cancelled = ActivityExecutionDecodeHint { cancelled: true }.adapt(
            DefaultFailureConverter
                .to_error(
                    Failure {
                        message: "cancelled".to_owned(),
                        failure_info: Some(FailureInfo::CanceledFailureInfo(
                            CanceledFailureInfo::default(),
                        )),
                        ..Default::default()
                    },
                    &PayloadConverter::default(),
                    &SerializationContextData::None,
                )
                .expect("failure decodes"),
        );
        assert!(matches!(cancelled, ActivityExecutionError::Cancelled(_)));
        assert_eq!(llm_boundary_failure(&cancelled), None);

        let unknown = activity_error(activity_failure(
            RetryState::NonRetryableFailure,
            application_failure("some_operational_bug"),
        ));
        assert_eq!(llm_boundary_failure(&unknown), None);

        // A timeout whose cause is an unknown application failure is still
        // that bug, not an outage.
        let wrapped_bug = activity_error(activity_failure(
            RetryState::Timeout,
            timeout_failure(
                TimeoutType::ScheduleToClose,
                Some(application_failure("some_operational_bug")),
            ),
        ));
        assert_eq!(llm_boundary_failure(&wrapped_bug), None);
    }
}

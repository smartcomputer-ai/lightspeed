//! Session-independent transcription. Temporal owns request identity and state;
//! activities store audio and transcript content in CAS.
use api::{
    Attribution, ModelConfig, TranscriptionFailure, TranscriptionFailureKind,
    TranscriptionStartParams, TranscriptionStatus, TranscriptionView,
};
use futures::{FutureExt, select_biased};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use temporalio_common::protos::{
    coresdk::workflow_commands::ActivityCancellationType, temporal::api::common::v1::RetryPolicy,
};
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ActivityCloseTimeouts, ActivityOptions, CancellableFuture, SyncWorkflowContext,
    WorkflowContext, WorkflowContextView, WorkflowResult,
};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionWorkflowArgs {
    pub universe_id: uuid::Uuid,
    pub transcription_id: String,
    pub created_by: Attribution,
    pub request: TranscriptionStartParams,
    pub model: ModelConfig,
    pub created_at_ms: u64,
}

/// Explicit caller keys, rather than audio hashes, define request identity.
pub fn transcription_id(owner: &Attribution, key: &str) -> String {
    let bytes = serde_json::to_vec(&(owner, key)).expect("serializable transcription identity");
    format!("transcription_{}", hex::encode(Sha256::digest(bytes)))
}

pub fn transcription_workflow_id(args: &TranscriptionWorkflowArgs) -> String {
    format!("{}/{}", args.universe_id, args.transcription_id)
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionSnapshot {
    pub request: TranscriptionStartParams,
    pub view: TranscriptionView,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TranscriptionActivityResult {
    Succeeded { transcript_ref: String },
    Failed { failure: TranscriptionFailure },
}

impl TranscriptionWorkflowArgs {
    pub fn pending(&self) -> TranscriptionView {
        TranscriptionView {
            transcription_id: self.transcription_id.clone(),
            created_by: self.created_by.clone(),
            audio: self.request.audio.clone(),
            model: self.model.clone(),
            status: TranscriptionStatus::Pending,
            created_at_ms: self.created_at_ms,
            transcript_ref: None,
            text: None,
            failure: None,
        }
    }
}

#[workflow(name = "TranscriptionWorkflow")]
#[derive(Default)]
pub struct TranscriptionWorkflow {
    snapshot: Option<TranscriptionSnapshot>,
    cancel_requested: bool,
}

#[workflow_methods]
impl TranscriptionWorkflow {
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        args: TranscriptionWorkflowArgs,
    ) -> WorkflowResult<()> {
        if ctx.workflow_id() != transcription_workflow_id(&args) {
            return Err(anyhow::anyhow!("transcription workflow identity mismatch").into());
        }
        let mut view = args.pending();
        view.status = TranscriptionStatus::Running;
        ctx.state_mut(|state| {
            state.snapshot = Some(TranscriptionSnapshot {
                request: args.request.clone(),
                view,
            })
        });
        let options = ActivityOptions::with_close_timeouts(ActivityCloseTimeouts::Both {
            start_to_close: Duration::from_secs(360),
            schedule_to_close: Duration::from_secs(900),
        })
        .heartbeat_timeout(Duration::from_secs(15))
        .cancellation_type(ActivityCancellationType::WaitCancellationCompleted)
        .retry_policy(RetryPolicy {
            initial_interval: Some(Duration::from_secs(2).try_into().unwrap()),
            maximum_interval: Some(Duration::from_secs(15).try_into().unwrap()),
            backoff_coefficient: 2.0,
            maximum_attempts: 3,
            non_retryable_error_types: vec![],
        })
        .build();
        let mut activity = ctx.start_activity(
            crate::WorkflowActivities::execute_transcription,
            args,
            options,
        );
        let result = {
            let cancellation = ctx.wait_condition(|state| state.cancel_requested).fuse();
            let external_cancel = ctx.cancelled().fuse();
            let mut work = (&mut activity).fuse();
            futures::pin_mut!(cancellation, external_cancel);
            select_biased! {
                _ = cancellation => None,
                _ = external_cancel => None,
                result = work => Some(result),
            }
        };
        if result.is_none() {
            activity.cancel();
            let _ = activity.await;
        }
        ctx.state_mut(|state| {
            let view = &mut state.snapshot.as_mut().expect("initialized").view;
            match result {
                None => view.status = TranscriptionStatus::Cancelled,
                Some(Ok(TranscriptionActivityResult::Succeeded { transcript_ref })) => {
                    view.status = TranscriptionStatus::Succeeded;
                    view.transcript_ref = Some(transcript_ref);
                }
                Some(Ok(TranscriptionActivityResult::Failed { failure })) => {
                    view.status = TranscriptionStatus::Failed;
                    view.failure = Some(failure);
                }
                Some(Err(error)) => {
                    view.status = TranscriptionStatus::Failed;
                    view.failure = Some(TranscriptionFailure {
                        kind: if error.as_timeout().is_some() {
                            TranscriptionFailureKind::Timeout
                        } else {
                            TranscriptionFailureKind::Provider
                        },
                        message: "Transcription exhausted its execution budget.".into(),
                    });
                }
            }
        });
        Ok(())
    }

    #[signal(name = "cancel")]
    pub fn cancel(&mut self, _ctx: &mut SyncWorkflowContext<Self>) {
        self.cancel_requested = true;
    }

    #[query(name = "snapshot")]
    pub fn snapshot(&self, _ctx: &WorkflowContextView) -> Option<TranscriptionSnapshot> {
        self.snapshot.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_is_scoped_to_owner_and_explicit_key() {
        let a = Attribution::Actor { id: "a".into() };
        let b = Attribution::Actor { id: "b".into() };
        assert_eq!(transcription_id(&a, "one"), transcription_id(&a, "one"));
        assert_ne!(transcription_id(&a, "one"), transcription_id(&b, "one"));
        assert_ne!(transcription_id(&a, "one"), transcription_id(&a, "two"));
    }
}

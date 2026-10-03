use super::*;
use temporal_workflow::{TranscriptionSnapshot, TranscriptionWorkflow, TranscriptionWorkflowArgs};
use temporalio_common::protos::temporal::api::enums::v1::WorkflowIdReusePolicy;

fn validate_id(id: &str) -> Result<(), AgentApiError> {
    if id
        .strip_prefix("transcription_")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        Ok(())
    } else {
        Err(AgentApiError::invalid_request("invalid transcription id"))
    }
}

impl GatewayAgentApi {
    pub(crate) async fn transcription_snapshot(
        &self,
        id: &str,
    ) -> Result<Option<TranscriptionSnapshot>, AgentApiError> {
        validate_id(id)?;
        let handle = self
            .client
            .get_workflow_handle::<TranscriptionWorkflow>(format!("{}/{id}", self.universe_id()));
        match handle
            .query(
                TranscriptionWorkflow::snapshot,
                (),
                WorkflowQueryOptions::default(),
            )
            .await
        {
            Ok(Some(mut snapshot)) => {
                if !snapshot.view.status.is_terminal() {
                    let description = handle
                        .describe(WorkflowDescribeOptions::default())
                        .await
                        .map_err(map_workflow_interaction_error)?;
                    match description.status() {
                        WorkflowExecutionStatus::TimedOut => {
                            snapshot.view.status = TranscriptionStatus::Failed;
                            snapshot.view.failure = Some(TranscriptionFailure {
                                kind: TranscriptionFailureKind::Timeout,
                                message: "Transcription exceeded its workflow deadline.".into(),
                            });
                        }
                        WorkflowExecutionStatus::Canceled | WorkflowExecutionStatus::Terminated => {
                            snapshot.view.status = TranscriptionStatus::Cancelled
                        }
                        WorkflowExecutionStatus::Failed => {
                            snapshot.view.status = TranscriptionStatus::Failed;
                            snapshot.view.failure = Some(TranscriptionFailure {
                                kind: TranscriptionFailureKind::Internal,
                                message: "Transcription workflow failed.".into(),
                            });
                        }
                        _ => {}
                    }
                }
                Ok(Some(snapshot))
            }
            Ok(None) => Err(AgentApiError::conflict(
                "transcription is starting; retry shortly",
            )),
            Err(WorkflowQueryError::NotFound(_)) => Ok(None),
            Err(error) => Err(map_workflow_query_error(error)),
        }
    }

    /// Trusted internal callers supply their controller attribution explicitly.
    pub(crate) async fn admit_transcription(
        &self,
        request: TranscriptionStartParams,
        owner: Attribution,
    ) -> Result<TranscriptionView, AgentApiError> {
        request.validate()?;
        let source = parse_blob_ref(&request.audio.blob_ref)?;
        let id = temporal_workflow::transcription_id(&owner, &request.idempotency_key);
        if let Some(snapshot) = self.transcription_snapshot(&id).await? {
            return matching_request(snapshot, &request);
        }
        let model = match &request.model {
            Some(model) => model.clone(),
            None => self
                .store
                .read_model_defaults()
                .await
                .map_err(model_defaults::map_store_error)?
                .speech_to_text
                .ok_or_else(|| {
                    AgentApiError::model_default_unset(ModelDefaultSlot::SpeechToText)
                })?,
        };
        ModelDefaultSlot::SpeechToText.validate_model(&model)?;
        let info = self
            .store
            .stat_blob(&source)
            .await
            .map_err(map_input_blob_store_error)?;
        if info.byte_len > 25 * 1024 * 1024 {
            return Err(AgentApiError::audio_blob_too_large(
                "audio exceeds the 25 MiB limit",
            ));
        }
        self.store
            .touch_blob_refs(&[source])
            .await
            .map_err(map_input_blob_store_error)?;
        let args = TranscriptionWorkflowArgs {
            universe_id: self.universe_id(),
            transcription_id: id.clone(),
            created_by: owner,
            request: request.clone(),
            model,
            created_at_ms: now_ms()? as u64,
        };
        let pending = args.pending();
        match self
            .client
            .start_workflow(
                TranscriptionWorkflow::run,
                args,
                WorkflowStartOptions::new(
                    self.task_queue.clone(),
                    format!("{}/{id}", self.universe_id()),
                )
                .id_reuse_policy(WorkflowIdReusePolicy::RejectDuplicate)
                .execution_timeout(Duration::from_secs(960))
                .build(),
            )
            .await
        {
            Ok(_) => Ok(pending),
            Err(WorkflowStartError::AlreadyStarted { .. }) => {
                // Another admission won the race. Its pinned model is authoritative.
                let snapshot = self.transcription_snapshot(&id).await?.ok_or_else(|| {
                    AgentApiError::conflict("transcription is starting; retry the same request")
                })?;
                matching_request(snapshot, &request)
            }
            Err(error) => Err(map_workflow_start_error(error)),
        }
    }

    pub(crate) async fn transcription_content(
        &self,
        mut view: TranscriptionView,
    ) -> Result<TranscriptionView, AgentApiError> {
        if let Some(reference) = &view.transcript_ref {
            let reference = parse_blob_ref(reference)?;
            let content = async {
                // Check metadata before the byte cache: a swept result is no
                // longer admissible even while this process has cached bytes.
                self.store.stat_blob(&reference).await?;
                self.store.read_bytes(&reference).await
            }
            .await;
            match content {
                Ok(bytes) => {
                    view.text = Some(String::from_utf8(bytes).map_err(|_| {
                        AgentApiError::internal("transcription result is not UTF-8")
                    })?);
                }
                Err(BlobStoreError::NotFound { .. }) => {
                    view.status = TranscriptionStatus::Expired;
                }
                Err(error) => return Err(map_blob_store_error(error)),
            }
        }
        Ok(view)
    }

    fn authorize_transcription_owner(&self, view: &TranscriptionView) -> Result<(), AgentApiError> {
        // Person callers can only access their own drafts. Direct universe keys
        // retain their method-group authority; core does not resolve person roles.
        if let Some(actor) = self.caller()?.actor
            && view.created_by != (Attribution::Actor { id: actor })
        {
            return Err(AgentApiError::forbidden());
        }
        Ok(())
    }

    pub(super) async fn start_transcription_impl(
        &self,
        params: TranscriptionStartParams,
    ) -> Result<AgentApiOutcome<TranscriptionResponse>, AgentApiError> {
        self.authorize_method(METHOD_TRANSCRIPTIONS_START, None)
            .await?;
        let view = self
            .admit_transcription(params, self.attribution()?)
            .await?;
        Ok(AgentApiOutcome::new(TranscriptionResponse {
            transcription: self.transcription_content(view).await?,
        }))
    }

    pub(super) async fn read_transcription_impl(
        &self,
        params: TranscriptionReadParams,
    ) -> Result<AgentApiOutcome<TranscriptionResponse>, AgentApiError> {
        self.authorize_method(METHOD_TRANSCRIPTIONS_READ, None)
            .await?;
        let view = self
            .transcription_snapshot(&params.transcription_id)
            .await?
            .ok_or_else(|| AgentApiError::not_found("transcription not found"))?
            .view;
        self.authorize_transcription_owner(&view)?;
        Ok(AgentApiOutcome::new(TranscriptionResponse {
            transcription: self.transcription_content(view).await?,
        }))
    }

    pub(super) async fn cancel_transcription_impl(
        &self,
        params: TranscriptionCancelParams,
    ) -> Result<AgentApiOutcome<TranscriptionResponse>, AgentApiError> {
        self.authorize_method(METHOD_TRANSCRIPTIONS_CANCEL, None)
            .await?;
        let view = self
            .transcription_snapshot(&params.transcription_id)
            .await?
            .ok_or_else(|| AgentApiError::not_found("transcription not found"))?
            .view;
        self.authorize_transcription_owner(&view)?;
        if !view.status.is_terminal() {
            let handle = self
                .client
                .get_workflow_handle::<TranscriptionWorkflow>(format!(
                    "{}/{}",
                    self.universe_id(),
                    params.transcription_id
                ));
            match handle
                .signal(
                    TranscriptionWorkflow::cancel,
                    (),
                    WorkflowSignalOptions::default(),
                )
                .await
            {
                Ok(()) | Err(WorkflowInteractionError::NotFound(_)) => {}
                Err(error) => return Err(map_workflow_interaction_error(error)),
            }
        }
        let view = self
            .transcription_snapshot(&params.transcription_id)
            .await?
            .map(|s| s.view)
            .unwrap_or(view);
        Ok(AgentApiOutcome::new(TranscriptionResponse {
            transcription: self.transcription_content(view).await?,
        }))
    }
}

fn matching_request(
    snapshot: TranscriptionSnapshot,
    request: &TranscriptionStartParams,
) -> Result<TranscriptionView, AgentApiError> {
    if snapshot.request != *request {
        return Err(AgentApiError::conflict(
            "transcription idempotency key was used with different input or options",
        ));
    }
    Ok(snapshot.view)
}

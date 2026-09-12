use engine::{
    BlobRef, RunId, SessionId, SessionPosition, WorkflowEndpointRef, WorkflowToolInvocation,
    storage::{
        AppendSessionEvents, AppendSessionEventsResult, BlobEdge, CreateSession, ReadSessionEvents,
        SessionStore, SessionStoreError, StoredSessionEntry, UncommittedStoredEvent,
    },
};
use temporal_workflow::{DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES, SessionBootstrapPayloadTooLarge};
use temporalio_sdk::activities::ActivityError;

use crate::worker::{
    AppendEventsRequest, CreateOrLoadSessionRequest, CreateOrLoadSessionResult, PutBlobRequest,
    ReadBlobRequest, ReadBlobResult,
};

use super::{common::activity_error, state::StorageActivityDeps};

pub(super) async fn create_or_load_session(
    deps: &StorageActivityDeps,
    request: CreateOrLoadSessionRequest,
) -> Result<CreateOrLoadSessionResult, ActivityError> {
    engine::storage::ensure_engine_blobs(deps.blobs.as_ref())
        .await
        .map_err(activity_error)?;
    let record = match deps
        .sessions
        .create_session(CreateSession {
            session_id: request.session_id.clone(),
            display_name: request.display_name.clone(),
            metadata: request.metadata.clone(),
            origin: None,
            delete_after_close_ms: request.delete_after_close_ms,
            created_at_ms: request.observed_at_ms,
        })
        .await
    {
        Ok(record) => record,
        Err(SessionStoreError::SessionAlreadyExists { .. }) => deps
            .sessions
            .load_session(&request.session_id)
            .await
            .map_err(activity_error)?
            .ok_or_else(|| {
                activity_error(anyhow::anyhow!(
                    "session disappeared after create conflict: {}",
                    request.session_id
                ))
            })?,
        Err(error) => return Err(activity_error(error)),
    };

    // Reduce checkpoint + authoritative tail *inside the activity* and return
    // only compact state, so neither the checkpoint bytes nor event history
    // crosses the Temporal activity boundary.
    let loaded =
        crate::checkpoint::load_reduction(deps.sessions.as_ref(), deps.blobs.as_ref(), &record)
            .await
            .map_err(activity_error)?;
    let checkpoint_due = crate::checkpoint::checkpoint_due(&loaded);
    let reduced = loaded.reduced;
    let head = record.head.clone();
    let fresh_session = loaded.fresh_session;
    if checkpoint_due
        && let Err(error) = crate::checkpoint::write_checkpoint(
            deps.sessions.as_ref(),
            deps.blobs.as_ref(),
            &record,
            &reduced,
            request.observed_at_ms,
        )
        .await
    {
        tracing::warn!(
            session_id = %request.session_id,
            error = %error,
            "session checkpoint write failed after bootstrap"
        );
    }
    let (core_state, replayed_event_count) = if fresh_session {
        (None, 0)
    } else {
        (Some(reduced.core_state), reduced.replayed_event_count)
    };

    let result = CreateOrLoadSessionResult {
        record,
        core_state,
        run_submissions: reduced.run_submissions,
        head,
        fresh_session,
        replayed_event_count,
    };

    guard_bootstrap_payload_size(&request.session_id, &result)?;
    Ok(result)
}

/// Fail with a typed, diagnosable error if the compact bootstrap result would
/// still exceed the Temporal payload budget — instead of letting Temporal
/// reject the activity completion with an opaque `Complete result exceeds size
/// limit`.
fn guard_bootstrap_payload_size(
    session_id: &SessionId,
    result: &CreateOrLoadSessionResult,
) -> Result<(), ActivityError> {
    guard_bootstrap_payload_size_with_budget(
        session_id,
        result,
        DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES,
    )
}

fn guard_bootstrap_payload_size_with_budget(
    session_id: &SessionId,
    result: &CreateOrLoadSessionResult,
    budget_bytes: u64,
) -> Result<(), ActivityError> {
    let serialized = serde_json::to_vec(result).map_err(activity_error)?;
    let reduced_state_bytes = serialized.len() as u64;
    if reduced_state_bytes > budget_bytes {
        return Err(activity_error(SessionBootstrapPayloadTooLarge {
            session_id: session_id.clone(),
            reduced_state_bytes,
            budget_bytes,
            replayed_event_count: result.replayed_event_count,
        }));
    }
    Ok(())
}

pub(super) async fn put_blob(
    deps: &StorageActivityDeps,
    request: PutBlobRequest,
) -> Result<BlobRef, ActivityError> {
    deps.blobs
        .put_bytes(request.bytes)
        .await
        .map_err(activity_error)
}

pub(super) async fn materialize_await_result(
    deps: &StorageActivityDeps,
    request: temporal_workflow::AwaitMaterializationRequest,
) -> Result<temporal_workflow::AwaitMaterializationResult, ActivityError> {
    if request.results.len() > 32 {
        return Err(activity_error(anyhow::anyhow!(
            "await materialization exceeds the 32-Promise limit"
        )));
    }

    // An `await` is one result, so the awaited payloads share one media
    // budget, in await order.
    let mut additional_context = Vec::new();
    let mut budget = engine::media::MAX_TOOL_MEDIA_ITEMS;
    let mut omitted = 0usize;
    for result in &request.results {
        if result.status != "resolved" {
            continue;
        }
        let Some(payload_ref) = &result.payload_ref else {
            continue;
        };
        let (entries, left_out) = prepare_payload_context(deps, payload_ref, &mut budget).await?;
        additional_context.extend(entries);
        omitted += left_out;
    }
    if omitted > 0 {
        additional_context.push(omission_note(deps, omitted, "await result").await?);
    }

    let mut results = Vec::with_capacity(request.results.len());
    let mut opaque_children = Vec::new();
    for result in request.results {
        let root = match result.status.as_str() {
            "resolved" => result.payload_ref.map(|blob_ref| ("output", blob_ref)),
            "failed" => result.error_ref.map(|blob_ref| ("error", blob_ref)),
            _ => None,
        };
        let materialized = if let Some((field, blob_ref)) = root {
            let bytes = deps
                .blobs
                .read_bytes(&blob_ref)
                .await
                .map_err(activity_error)?;
            let value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(json) => json,
                Err(_) => match String::from_utf8(bytes) {
                    Ok(text) => serde_json::Value::String(text),
                    Err(error) => {
                        let byte_len = error.as_bytes().len();
                        opaque_children.push(blob_ref.clone());
                        serde_json::json!({
                            "blob_ref": blob_ref,
                            "media_type": "application/octet-stream",
                            "byte_len": byte_len,
                        })
                    }
                },
            };
            Some((field, value))
        } else {
            None
        };
        let (output, error) = match materialized {
            Some(("output", value)) => (Some(value), None),
            Some(("error", value)) => (None, Some(value)),
            _ => (None, None),
        };
        results.push(temporal_workflow::MaterializedAwaitPromiseResult {
            promise_id: result.promise_id,
            status: result.status,
            output,
            error,
        });
    }

    let aggregate = temporal_workflow::MaterializedAwaitResult {
        outcome: request.outcome,
        results,
    };
    let aggregate_ref = deps
        .blobs
        .put_bytes(serde_json::to_vec(&aggregate).map_err(activity_error)?)
        .await
        .map_err(activity_error)?;
    if !opaque_children.is_empty()
        && let Some(blob_graph) = &deps.blob_graph
    {
        blob_graph
            .record_blob_edges(
                opaque_children
                    .into_iter()
                    .map(|child| BlobEdge::contains(aggregate_ref.clone(), child))
                    .collect(),
            )
            .await
            .map_err(activity_error)?;
    }
    Ok(temporal_workflow::AwaitMaterializationResult {
        result_ref: aggregate_ref,
        additional_context,
    })
}

/// Context a resolved payload supplies beside its result, prepared for the
/// model. A payload names media in a top-level `media` list of descriptors;
/// each admitted one (a supported type, a blob that is in CAS, within the
/// byte limit) becomes a media entry, in list order, until `budget` runs
/// out. Anything malformed, missing, or unsupported contributes nothing, and
/// nothing here fails the resume. Returns the entries and how many admitted
/// items the budget left out.
async fn prepare_payload_context(
    deps: &StorageActivityDeps,
    payload_ref: &BlobRef,
    budget: &mut usize,
) -> Result<(Vec<engine::ContextEntryInput>, usize), ActivityError> {
    let bytes = deps
        .blobs
        .read_bytes(payload_ref)
        .await
        .map_err(activity_error)?;
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok((Vec::new(), 0));
    };
    let Some(listed) = payload.get("media").and_then(serde_json::Value::as_array) else {
        return Ok((Vec::new(), 0));
    };
    let mut entries = Vec::new();
    let mut omitted = 0usize;
    for item in listed {
        let Ok(descriptor) = serde_json::from_value::<engine::media::MediaDescriptor>(item.clone())
        else {
            continue;
        };
        let Some(admitted) = engine::media::MediaDescriptor::new(
            descriptor.content_ref.clone(),
            &descriptor.media_type,
            descriptor.name.as_deref(),
        ) else {
            continue;
        };
        let Ok(info) = deps.blobs.stat_blob(&admitted.content_ref).await else {
            continue;
        };
        if engine::media::admit_tool_media(Some(&admitted.media_type), info.byte_len).is_err() {
            continue;
        }
        if *budget == 0 {
            omitted += 1;
            continue;
        }
        *budget -= 1;
        entries.push(admitted.context_entry());
    }
    Ok((entries, omitted))
}

/// A user-role text entry telling the model that media beyond the cap was
/// left out of one result.
async fn omission_note(
    deps: &StorageActivityDeps,
    omitted: usize,
    what: &str,
) -> Result<engine::ContextEntryInput, ActivityError> {
    let text = format!(
        "[{omitted} media item{} omitted: at most {} per {what}]",
        if omitted == 1 { "" } else { "s" },
        engine::media::MAX_TOOL_MEDIA_ITEMS
    );
    let content_ref = deps
        .blobs
        .put_bytes(text.clone().into_bytes())
        .await
        .map_err(activity_error)?;
    Ok(engine::ContextEntryInput {
        kind: engine::ContextEntryKind::Message {
            role: engine::ContextMessageRole::User,
        },
        content: engine::ContentRef::text(content_ref),
        preview: Some(text),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    })
}

/// Joined calls complete separately, so each promise's payload supplies its
/// own bounded supplements beside its own call result.
pub(super) async fn prepare_joined_context(
    deps: &StorageActivityDeps,
    request: temporal_workflow::JoinedContextPreparationRequest,
) -> Result<Vec<engine::PromiseContextEntries>, ActivityError> {
    let mut prepared = Vec::new();
    for result in request.results {
        if result.status != "resolved" {
            continue;
        }
        let Some(payload_ref) = result.payload_ref else {
            continue;
        };
        let Ok(promise_id) = engine::PromiseId::try_new(result.promise_id.clone()) else {
            continue;
        };
        let mut budget = engine::media::MAX_TOOL_MEDIA_ITEMS;
        let (mut entries, omitted) =
            prepare_payload_context(deps, &payload_ref, &mut budget).await?;
        if omitted > 0 {
            entries.push(omission_note(deps, omitted, "result").await?);
        }
        if !entries.is_empty() {
            prepared.push(engine::PromiseContextEntries {
                promise_id,
                entries,
            });
        }
    }
    Ok(prepared)
}

/// Bounded CAS load + JSON Schema check of one keyed reply payload against
/// the binding's immutable reply schema. Invalid payloads become a stable
/// CAS-backed error the workflow uses to fail the promise; only I/O and
/// schema-compilation problems are activity errors.
pub(super) async fn validate_workflow_tool_reply(
    deps: &StorageActivityDeps,
    request: temporal_workflow::WorkflowToolReplyValidationRequest,
) -> Result<temporal_workflow::WorkflowToolReplyValidationResult, ActivityError> {
    use temporal_workflow::WorkflowToolReplyValidationResult as ValidationResult;

    let schema_bytes = deps
        .blobs
        .read_bytes(&request.reply_schema_ref)
        .await
        .map_err(activity_error)?;
    let schema: serde_json::Value = serde_json::from_slice(&schema_bytes).map_err(|error| {
        activity_error(anyhow::anyhow!("reply schema is not valid JSON: {error}"))
    })?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|error| activity_error(anyhow::anyhow!("reply schema is unsupported: {error}")))?;

    let invalid = |message: String| async {
        let error_ref = deps
            .blobs
            .put_bytes(message.into_bytes())
            .await
            .map_err(activity_error)?;
        Ok(ValidationResult::Invalid { error_ref })
    };

    let Some(payload_ref) = request.payload_ref else {
        return invalid("reply payload is required by the binding's reply schema".to_owned()).await;
    };
    let payload_bytes = deps
        .blobs
        .read_bytes(&payload_ref)
        .await
        .map_err(activity_error)?;
    let payload: serde_json::Value = match serde_json::from_slice(&payload_bytes) {
        Ok(payload) => payload,
        Err(error) => {
            return invalid(format!("reply payload is not valid JSON: {error}")).await;
        }
    };
    match validator.validate(&payload) {
        Ok(()) => Ok(ValidationResult::Valid),
        Err(error) => {
            invalid(format!(
                "reply payload does not match the binding's reply schema: {error}"
            ))
            .await
        }
    }
}

pub(super) async fn read_blob(
    deps: &StorageActivityDeps,
    request: ReadBlobRequest,
) -> Result<ReadBlobResult, ActivityError> {
    let bytes = deps
        .blobs
        .read_bytes(&request.blob_ref)
        .await
        .map_err(activity_error)?;
    Ok(ReadBlobResult { bytes })
}

/// Read one run's workflow-tool emissions from this worker state's
/// universe-scoped session store. Exact receiver authorization is evaluated
/// against the durable binding facts by the engine projection.
// Work-cycle reconciliation is the first production caller.
#[allow(dead_code)]
pub(super) async fn read_tool_emissions(
    deps: &StorageActivityDeps,
    receiver_endpoint: &WorkflowEndpointRef,
    session_id: &SessionId,
    run_id: RunId,
) -> Result<Vec<WorkflowToolInvocation>, ActivityError> {
    read_tool_emissions_with_page_limit(deps, receiver_endpoint, session_id, run_id, 512).await
}

async fn read_tool_emissions_with_page_limit(
    deps: &StorageActivityDeps,
    receiver_endpoint: &WorkflowEndpointRef,
    session_id: &SessionId,
    run_id: RunId,
    page_limit: usize,
) -> Result<Vec<WorkflowToolInvocation>, ActivityError> {
    let entries =
        read_all_session_events_with_page_limit(deps.sessions.as_ref(), session_id, page_limit)
            .await?;
    engine::read_tool_emissions(&entries, receiver_endpoint, session_id, run_id)
        .map_err(activity_error)
}

pub(super) async fn append_events(
    deps: &StorageActivityDeps,
    request: AppendEventsRequest,
) -> Result<AppendSessionEventsResult, ActivityError> {
    let append = AppendSessionEvents {
        session_id: request.session_id.clone(),
        expected_head: request.expected_head.clone(),
        events: request.events.clone(),
    };
    match deps.sessions.append(append).await {
        Ok(result) => {
            maybe_refresh_checkpoint_after_append(deps, &request.session_id, &result).await;
            Ok(result)
        }
        Err(error @ SessionStoreError::ExpectedHeadMismatch { .. })
            if !request.events.is_empty() =>
        {
            confirm_existing_append(deps.sessions.as_ref(), &request, error)
                .await
                .map_err(activity_error)
        }
        Err(error) => Err(activity_error(error)),
    }
}

async fn maybe_refresh_checkpoint_after_append(
    deps: &StorageActivityDeps,
    session_id: &SessionId,
    result: &AppendSessionEventsResult,
) {
    let appended_bytes = result
        .entries
        .iter()
        .map(|entry| serde_json::to_vec(entry).map_or(0, |bytes| bytes.len() as u64))
        .sum::<u64>();
    let terminal = result
        .entries
        .iter()
        .any(engine::storage::is_terminal_run_entry);
    let Some(record) = deps.sessions.load_session(session_id).await.ok().flatten() else {
        return;
    };
    let checkpoint_seq = deps
        .sessions
        .load_checkpoint(session_id)
        .await
        .ok()
        .flatten()
        .map(|checkpoint| checkpoint.through_seq);
    let tail_event_count = record.head.as_ref().map_or(0, |head| {
        head.seq
            .as_u64()
            .saturating_sub(checkpoint_seq.map_or(0, engine::EventSeq::as_u64))
    });
    if !terminal
        && tail_event_count < crate::checkpoint::CHECKPOINT_TAIL_EVENT_THRESHOLD
        && appended_bytes < crate::checkpoint::CHECKPOINT_APPEND_BATCH_BYTE_THRESHOLD
    {
        return;
    }
    let loaded = match crate::checkpoint::load_reduction(
        deps.sessions.as_ref(),
        deps.blobs.as_ref(),
        &record,
    )
    .await
    {
        Ok(loaded) => loaded,
        Err(error) => {
            tracing::warn!(session_id = %session_id, error = %error, "session checkpoint refresh load failed");
            return;
        }
    };
    let newly_terminal_seqs = result
        .entries
        .iter()
        .filter(|entry| engine::storage::is_terminal_run_entry(entry))
        .map(|entry| entry.position.seq)
        .collect::<Vec<_>>();
    let terminal_runs_due = terminal
        && crate::checkpoint::terminal_runs_checkpoint_due(
            &loaded.reduced.core_state,
            checkpoint_seq,
            &newly_terminal_seqs,
        );
    let tail_due = loaded.tail_event_count >= crate::checkpoint::CHECKPOINT_TAIL_EVENT_THRESHOLD
        || loaded.tail_encoded_bytes >= crate::checkpoint::CHECKPOINT_TAIL_BYTE_THRESHOLD
        || appended_bytes >= crate::checkpoint::CHECKPOINT_APPEND_BATCH_BYTE_THRESHOLD;
    if !terminal_runs_due && !tail_due {
        return;
    }
    let created_at_ms = result
        .entries
        .last()
        .map_or(record.updated_at_ms, |entry| entry.observed_at_ms);
    if let Err(error) = crate::checkpoint::write_checkpoint(
        deps.sessions.as_ref(),
        deps.blobs.as_ref(),
        &record,
        &loaded.reduced,
        created_at_ms,
    )
    .await
    {
        tracing::warn!(session_id = %session_id, error = %error, "session checkpoint refresh failed");
    }
}

async fn confirm_existing_append(
    store: &dyn SessionStore,
    request: &AppendEventsRequest,
    original_error: SessionStoreError,
) -> Result<AppendSessionEventsResult, SessionStoreError> {
    let page = store
        .read_after(ReadSessionEvents {
            session_id: request.session_id.clone(),
            after: request.expected_head.as_ref().map(|position| position.seq),
            limit: request.events.len(),
        })
        .await?;
    if !committed_entries_match_request(&request.expected_head, &page.entries, &request.events) {
        return Err(original_error);
    }

    Ok(AppendSessionEventsResult {
        head: page.entries.last().map(|entry| entry.position.clone()),
        entries: page.entries,
    })
}

fn committed_entries_match_request(
    expected_head: &Option<SessionPosition>,
    entries: &[StoredSessionEntry],
    events: &[UncommittedStoredEvent],
) -> bool {
    if entries.len() != events.len() {
        return false;
    }

    let mut previous_seq = expected_head
        .as_ref()
        .map(|position| position.seq.as_u64())
        .unwrap_or(0);
    entries.iter().zip(events).all(|(entry, event)| {
        let expected_seq = previous_seq.saturating_add(1);
        let matches = entry.position.seq.as_u64() == expected_seq
            && entry.observed_at_ms == event.observed_at_ms
            && entry.joins == event.joins
            && entry.event == event.event;
        previous_seq = expected_seq;
        matches
    })
}

#[cfg(test)]
async fn read_all_session_events(
    store: &dyn SessionStore,
    session_id: &SessionId,
) -> Result<Vec<StoredSessionEntry>, ActivityError> {
    read_all_session_events_with_page_limit(store, session_id, 512).await
}

async fn read_all_session_events_with_page_limit(
    store: &dyn SessionStore,
    session_id: &SessionId,
    page_limit: usize,
) -> Result<Vec<StoredSessionEntry>, ActivityError> {
    let mut after = None;
    let mut entries = Vec::new();
    loop {
        let page = store
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
                after,
                limit: page_limit,
            })
            .await
            .map_err(activity_error)?;
        after = page.next_after;
        entries.extend(page.entries);
        if page.complete {
            return Ok(entries);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use engine::{
        CoreAgentIoError, CoreAgentLlm, CoreAgentTools, LlmFinish, LlmGenerationFacts,
        LlmGenerationRequest, LlmGenerationResult, LlmGenerationStatus, ObservedToolCall,
        StoredEvent, ToolBatchOutcome, ToolCallStatus, ToolInvocationBatchRequest,
        ToolInvocationBatchResult, ToolInvocationResult, ToolName, WorkflowToolBinding,
        WorkflowToolInvocation, WorkflowToolInvocationId,
        storage::{BlobStore, InMemoryBlobStore, InMemorySessionStore, SessionPage, SessionRecord},
    };
    use serde_json::json;

    use super::*;

    fn test_event(
        observed_at_ms: u64,
        joins: impl IntoIterator<Item = (&'static str, &'static str)>,
        payload: serde_json::Value,
    ) -> UncommittedStoredEvent {
        UncommittedStoredEvent {
            observed_at_ms,
            joins: joins
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect::<BTreeMap<_, _>>(),
            event: StoredEvent::new("lightspeed.test.event", 1, payload),
        }
    }

    async fn create_test_session(store: &InMemorySessionStore) -> SessionId {
        let session_id = SessionId::new("session-a");
        store
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        session_id
    }

    async fn read_all(store: &InMemorySessionStore, session_id: &SessionId) -> SessionPage {
        store
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
                after: None,
                limit: 16,
            })
            .await
            .expect("read session events")
    }

    /// Minimal LLM stub: the bootstrap-volume test never requests a run, so
    /// generation must never be reached.
    struct UnreachableLlm;

    #[async_trait::async_trait]
    impl engine::CoreAgentLlm for UnreachableLlm {
        async fn generate(
            &self,
            _request: engine::LlmGenerationRequest,
        ) -> Result<engine::LlmGenerationResult, engine::CoreAgentIoError> {
            panic!("bootstrap-volume test must not generate")
        }
    }

    struct WorkflowToolLlm {
        generations: AtomicUsize,
        tool_calls: Vec<ObservedToolCall>,
    }

    #[async_trait::async_trait]
    impl CoreAgentLlm for WorkflowToolLlm {
        async fn generate(
            &self,
            request: LlmGenerationRequest,
        ) -> Result<LlmGenerationResult, CoreAgentIoError> {
            let first = self.generations.fetch_add(1, Ordering::SeqCst) == 0;
            Ok(LlmGenerationResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                status: LlmGenerationStatus::Succeeded,
                failure_ref: None,
                context_entries: Vec::new(),
                facts: LlmGenerationFacts {
                    duration_ms: None,
                    provider_response_id: Some(format!("tool-response-{}", request.turn_id)),
                    finish: if first {
                        LlmFinish::ToolCalls
                    } else {
                        LlmFinish::Stop
                    },
                    usage: None,
                    tool_calls: if first {
                        self.tool_calls.clone()
                    } else {
                        Default::default()
                    },
                    approval_requests: Vec::new(),
                    context_token_estimate: None,
                },
            })
        }
    }

    struct WorkflowToolTools {
        universe_id: uuid::Uuid,
        bindings: BTreeMap<ToolName, WorkflowToolBinding>,
    }

    #[async_trait::async_trait]
    impl CoreAgentTools for WorkflowToolTools {
        async fn invoke_batch(
            &self,
            request: ToolInvocationBatchRequest,
        ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
            let results = request
                .calls
                .iter()
                .map(|call| {
                    let binding = self
                        .bindings
                        .get(&call.tool_name)
                        .expect("test tool call must have a durable workflow-tool binding");
                    let invocation = WorkflowToolInvocation {
                        invocation_id: WorkflowToolInvocationId::for_call(
                            self.universe_id,
                            &request.session_id,
                            request.run_id,
                            request.turn_id,
                            request.batch_id,
                            &call.call_id,
                            &binding.binding_fingerprint,
                        ),
                        tool_id: binding.definition.tool_id.clone(),
                        semantic_type: binding.definition.semantic_type.clone(),
                        schema_revision: binding.definition.revision,
                        binding_fingerprint: binding.binding_fingerprint.clone(),
                        session_universe_id: self.universe_id,
                        session_id: request.session_id.clone(),
                        run_id: request.run_id,
                        turn_id: request.turn_id,
                        tool_batch_id: request.batch_id,
                        tool_call_id: call.call_id.clone(),
                        arguments_ref: call.arguments_ref.clone(),
                        execution_context_ref: None,
                        completion_promises: None,
                    };
                    ToolInvocationResult {
                        duration_ms: None,
                        output_bytes: None,
                        truncated: false,
                        call_id: call.call_id.clone(),
                        status: ToolCallStatus::Succeeded,
                        output_ref: Some(BlobRef::from_bytes(b"accepted")),
                        model_visible_context_entries: vec![
                            ToolInvocationResult::tool_result_context_entry(
                                &call.call_id,
                                ToolCallStatus::Succeeded,
                                BlobRef::from_bytes(b"accepted"),
                            ),
                        ],
                        error_ref: None,
                        effects: vec![engine::workflow_tool_emit_effect(&invocation)],
                    }
                })
                .collect();
            Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                run_id: request.run_id,
                turn_id: request.turn_id,
                batch_id: request.batch_id,
                results,
            }))
        }
    }

    fn volume_session_config() -> engine::SessionConfig {
        temporal_workflow::default_session_config(engine::ModelSelection {
            api_kind: engine::ProviderApiKind::OpenAiResponses,
            provider_id: "openai".to_owned(),
            model: "gpt-test".to_owned(),
        })
    }

    /// Regression: a session whose durable log is far larger than the compact
    /// bootstrap budget rehydrates successfully through the compact path, and
    /// the full event log never appears in the activity result.
    #[tokio::test(flavor = "current_thread")]
    async fn bootstrap_returns_compact_state_for_large_log() {
        use test_support::{DriveCommand, RunnerStores, SessionRunner};

        let store = Arc::new(InMemorySessionStore::new());
        let blobs: Arc<dyn engine::storage::BlobStore> = Arc::new(InMemoryBlobStore::new());
        let session_id = SessionId::new("bridge_large_session");
        store
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create session");

        let runner = SessionRunner::new(
            RunnerStores::new(store.clone(), blobs.clone()),
            Arc::new(UnreachableLlm),
        );
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: engine::CoreAgentCommand::OpenSession {
                    config: volume_session_config(),
                },
                max_steps: None,
            })
            .await
            .expect("open session");

        // Repeatedly upsert the SAME keyed entry with sizeable previews. Each
        // upsert appends a context-applied event to the durable log but replaces
        // the prior active entry for that key, so the log grows without bound
        // while active context stays at a single entry: a long-lived session
        // whose durable log dwarfs its reduced state.
        let big_preview = "x".repeat(2_048);
        let upsert_count = 600u64;
        for index in 0..upsert_count {
            runner
                .drive_command(DriveCommand {
                    session_id: session_id.clone(),
                    observed_at_ms: 100 + index,
                    command: engine::CoreAgentCommand::UpsertContext {
                        expected_revision: None,
                        key: engine::ContextEntryKey::new("note.live"),
                        entry: engine::ContextEntryInput {
                            kind: engine::ContextEntryKind::ProviderOpaque,
                            content: engine::ContentRef {
                                content_ref: engine::BlobRef::from_bytes(
                                    format!("note-content-{index}").as_bytes(),
                                ),
                                media_type: Some("application/json".to_owned()),
                                provider_kind: None,
                            },
                            preview: Some(big_preview.clone()),
                            origin: None,
                            provenance_ref: None,
                            token_estimate: None,
                        },
                    },
                    max_steps: None,
                })
                .await
                .expect("upsert context");
        }

        let raw_log = read_all_session_events(store.as_ref(), &session_id)
            .await
            .expect("read raw log");
        let raw_log_bytes = serde_json::to_vec(&raw_log)
            .expect("serialize raw log")
            .len();

        let deps = storage_deps(store.clone());
        let result = create_or_load_session(
            &deps,
            CreateOrLoadSessionRequest {
                metadata: Default::default(),
                display_name: None,
                session_id: session_id.clone(),
                delete_after_close_ms: None,
                observed_at_ms: 2,
            },
        )
        .await
        .expect("cold bootstrap succeeds via compact path");

        // The activity result must be far smaller than the raw event log it was
        // reduced from.
        let result_bytes = serde_json::to_vec(&result).expect("serialize result").len();

        // The compact result carries reduced state, not the raw log.
        let core_state = result.core_state.expect("reduced state present");
        assert!(result.replayed_event_count >= upsert_count);
        // Active context stays tiny (the single replaced keyed entry) even
        // though the log accumulated hundreds of applied events.
        assert!(
            core_state.context.entries.len() < 8,
            "active context should stay small, got {}",
            core_state.context.entries.len()
        );
        assert!(
            result_bytes * 4 < raw_log_bytes,
            "compact result ({result_bytes} bytes) should be far smaller than raw log \
             ({raw_log_bytes} bytes)"
        );
        // And it stays under the bootstrap budget.
        assert!((result_bytes as u64) < temporal_workflow::DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES,);
    }

    /// The size guard fires with a typed error when the reduced result would
    /// exceed the budget, instead of letting Temporal reject it opaquely.
    #[test]
    fn bootstrap_size_guard_rejects_oversized_result() {
        let session_id = SessionId::new("oversized");
        let result = CreateOrLoadSessionResult {
            record: SessionRecord {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                lifecycle_status: engine::storage::SessionLifecycleStatus::New,
                closed_at_seq: None,
                closed_at_ms: None,
                retention_root_session_id: session_id.clone(),
                delete_after_close_ms: None,
                delete_at_ms: None,
                managed: false,
                head: None,
                source_session_id: None,
                source_seq: None,
                origin: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            core_state: Some(engine::CoreAgentState::new()),
            run_submissions: Default::default(),
            head: None,
            fresh_session: true,
            replayed_event_count: 0,
        };

        let serialized = serde_json::to_vec(&result).unwrap().len() as u64;
        assert!(serialized > 1, "fixture should be non-trivially sized");

        // Within budget: passes.
        guard_bootstrap_payload_size_with_budget(&session_id, &result, serialized + 1)
            .expect("within-budget result should pass");

        // Budget below serialized size: typed rejection.
        let err = guard_bootstrap_payload_size_with_budget(&session_id, &result, serialized - 1)
            .expect_err("oversized result should be rejected");
        match err {
            ActivityError::Application(failure) => {
                let typed = failure
                    .source_error()
                    .downcast_ref::<SessionBootstrapPayloadTooLarge>()
                    .expect("expected typed SessionBootstrapPayloadTooLarge");
                assert_eq!(typed.session_id, session_id);
                assert_eq!(typed.budget_bytes, serialized - 1);
            }
            other => panic!("expected application failure, got {other:?}"),
        }
    }

    fn storage_deps(store: Arc<InMemorySessionStore>) -> StorageActivityDeps {
        let sessions: Arc<dyn SessionStore> = store;
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        StorageActivityDeps {
            sessions,
            blobs,
            blob_graph: None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn joined_context_keeps_admitted_listed_blobs_in_order() {
        let deps = storage_deps(Arc::new(InMemorySessionStore::new()));
        let png = deps
            .blobs
            .put_bytes(b"\x89PNG\r\n\x1a\n....".to_vec())
            .await
            .expect("png");
        let pdf = deps
            .blobs
            .put_bytes(b"%PDF-1.7 report".to_vec())
            .await
            .expect("pdf");
        let missing = engine::BlobRef::from_bytes(b"never stored");
        let describe = |blob_ref: &engine::BlobRef, media_type: &str, name: &str| {
            json!({
                "handle": engine::media::media_handle(blob_ref),
                "content_ref": blob_ref,
                "media_type": media_type,
                "kind": if media_type == "application/pdf" { "document" } else { "image" },
                "name": name,
            })
        };
        let payload = deps
            .blobs
            .put_bytes(
                serde_json::to_vec(&json!({
                    "status": "completed",
                    "output": "see the render",
                    "media": [
                        describe(&pdf, "application/pdf", "report.pdf"),
                        describe(&missing, "image/png", "gone.png"),
                        describe(&png, "image/svg+xml", "vector.svg"),
                        {"not": "a descriptor"},
                        describe(&png, "image/png", "render.png"),
                    ]
                }))
                .expect("payload"),
            )
            .await
            .expect("payload blob");
        let text_payload = deps
            .blobs
            .put_bytes(b"plain text payload".to_vec())
            .await
            .expect("text payload");

        let prepared = prepare_joined_context(
            &deps,
            temporal_workflow::JoinedContextPreparationRequest {
                results: vec![
                    temporal_workflow::AwaitPromiseResult {
                        promise_id: "promise_1".to_owned(),
                        status: "resolved".to_owned(),
                        payload_ref: Some(payload),
                        error_ref: None,
                    },
                    temporal_workflow::AwaitPromiseResult {
                        promise_id: "promise_2".to_owned(),
                        status: "resolved".to_owned(),
                        payload_ref: Some(text_payload),
                        error_ref: None,
                    },
                    temporal_workflow::AwaitPromiseResult {
                        promise_id: "promise_3".to_owned(),
                        status: "failed".to_owned(),
                        payload_ref: None,
                        error_ref: None,
                    },
                ],
            },
        )
        .await
        .expect("prepare");

        assert_eq!(prepared.len(), 1, "{prepared:?}");
        assert_eq!(prepared[0].promise_id.as_str(), "promise_1");
        let previews = prepared[0]
            .entries
            .iter()
            .map(|entry| entry.preview.clone().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            previews,
            vec!["[document: report.pdf]", "[image: render.png]"]
        );
        assert_eq!(prepared[0].entries[1].content.content_ref, png);
        assert_eq!(
            prepared[0].entries[1].content.media_type.as_deref(),
            Some("image/png")
        );
    }

    /// An `await` is one result: its awaited payloads share one media
    /// budget, in await order, and the model is told what was left out.
    #[tokio::test(flavor = "current_thread")]
    async fn await_materialization_caps_media_across_promises_with_a_note() {
        let deps = storage_deps(Arc::new(InMemorySessionStore::new()));
        let mut payloads = Vec::new();
        for promise in 0..2u8 {
            let mut media = Vec::new();
            for index in 0..5u8 {
                let blob = deps
                    .blobs
                    .put_bytes(vec![0x89, b'P', b'N', b'G', promise, index])
                    .await
                    .expect("png");
                media.push(json!({
                    "handle": engine::media::media_handle(&blob),
                    "content_ref": blob,
                    "media_type": "image/png",
                    "kind": "image",
                    "name": format!("p{promise}-{index}.png"),
                }));
            }
            let payload = deps
                .blobs
                .put_bytes(
                    serde_json::to_vec(&json!({"status": "completed", "media": media})).unwrap(),
                )
                .await
                .expect("payload");
            payloads.push(payload);
        }
        let materialized = materialize_await_result(
            &deps,
            temporal_workflow::AwaitMaterializationRequest {
                outcome: temporal_workflow::AwaitOutcome::Terminal,
                results: payloads
                    .iter()
                    .enumerate()
                    .map(|(index, payload)| temporal_workflow::AwaitPromiseResult {
                        promise_id: format!("promise_{index}"),
                        status: "resolved".to_owned(),
                        payload_ref: Some(payload.clone()),
                        error_ref: None,
                    })
                    .collect(),
            },
        )
        .await
        .expect("materialize");
        let previews = materialized
            .additional_context
            .iter()
            .map(|entry| entry.preview.clone().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(previews.len(), engine::media::MAX_TOOL_MEDIA_ITEMS + 1);
        assert_eq!(previews[0], "[image: p0-0.png]");
        assert_eq!(previews[7], "[image: p1-2.png]");
        assert_eq!(
            previews[8],
            "[2 media items omitted: at most 8 per await result]"
        );
        let note = &materialized.additional_context[8];
        assert_eq!(note.content.media_type.as_deref(), Some("text/plain"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn await_materialization_embeds_json_text_and_opaque_roots_in_order() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let json_ref = blobs
            .put_bytes(br#"{"answer":42}"#.to_vec())
            .await
            .expect("json root");
        let text_ref = blobs
            .put_bytes(b"delegated answer".to_vec())
            .await
            .expect("text root");
        let opaque_ref = blobs
            .put_bytes(vec![0xff, 0x00])
            .await
            .expect("opaque root");
        let deps = StorageActivityDeps {
            sessions,
            blobs: blobs.clone(),
            blob_graph: None,
        };

        let materialized = materialize_await_result(
            &deps,
            temporal_workflow::AwaitMaterializationRequest {
                outcome: temporal_workflow::AwaitOutcome::Timeout,
                results: vec![
                    temporal_workflow::AwaitPromiseResult {
                        promise_id: "json".to_owned(),
                        status: "resolved".to_owned(),
                        payload_ref: Some(json_ref),
                        error_ref: None,
                    },
                    temporal_workflow::AwaitPromiseResult {
                        promise_id: "text".to_owned(),
                        status: "failed".to_owned(),
                        payload_ref: None,
                        error_ref: Some(text_ref),
                    },
                    temporal_workflow::AwaitPromiseResult {
                        promise_id: "opaque".to_owned(),
                        status: "resolved".to_owned(),
                        payload_ref: Some(opaque_ref.clone()),
                        error_ref: None,
                    },
                    temporal_workflow::AwaitPromiseResult {
                        promise_id: "pending".to_owned(),
                        status: "pending".to_owned(),
                        payload_ref: None,
                        error_ref: None,
                    },
                ],
            },
        )
        .await
        .expect("materialize await");
        let value: serde_json::Value = serde_json::from_slice(
            &blobs
                .read_bytes(&materialized.result_ref)
                .await
                .expect("aggregate bytes"),
        )
        .expect("aggregate JSON");

        assert_eq!(value["outcome"], "timeout");
        assert_eq!(value["results"][0]["output"], json!({"answer": 42}));
        assert_eq!(value["results"][1]["error"], "delegated answer");
        assert_eq!(
            value["results"][2]["output"]["blob_ref"],
            opaque_ref.as_str()
        );
        assert_eq!(value["results"][2]["output"]["byte_len"], 2);
        assert!(value["results"][3].get("output").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_events_returns_existing_entries_after_retry() {
        let store = Arc::new(InMemorySessionStore::new());
        let deps = storage_deps(store.clone());
        let session_id = create_test_session(store.as_ref()).await;
        let request = AppendEventsRequest {
            session_id: session_id.clone(),
            expected_head: None,
            events: vec![
                test_event(10, [("turn", "1")], json!({"value": "first"})),
                test_event(11, [("turn", "1")], json!({"value": "second"})),
            ],
        };

        let first = append_events(&deps, request.clone())
            .await
            .expect("append first batch");
        let retried = append_events(&deps, request)
            .await
            .expect("confirm retried batch");

        assert_eq!(retried, first);
        let page = read_all(store.as_ref(), &session_id).await;
        assert_eq!(page.entries, first.entries);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preparation_batch_retry_after_lost_commit_response_preserves_all_changes_once() {
        use engine::{
            ContextEntryInput, ContextEntryKey, ContextEntryKind, CoreAgentAction,
            CoreAgentCommand, CoreAgentDrive, CoreAgentState, EnvironmentsFeature, EventSeq,
        };
        let store = Arc::new(InMemorySessionStore::new());
        let deps = storage_deps(store.clone());
        let session_id = create_test_session(store.as_ref()).await;
        let mut config = temporal_workflow::default_session_config(engine::ModelSelection {
            api_kind: engine::ProviderApiKind::OpenAiResponses,
            provider_id: "openai".into(),
            model: "test-model".into(),
        });
        let open = CoreAgentCommand::OpenSession {
            config: config.clone(),
        };
        config.features.environments = Some(EnvironmentsFeature::default());
        let mut staged =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        let mut events = Vec::new();
        for command in [
            open,
            CoreAgentCommand::ReplaceSessionConfig {
                expected_revision: Some(0),
                config,
            },
            CoreAgentCommand::SetActiveEnvironment {
                environment_id: engine::EnvironmentId::new("existing"),
            },
            CoreAgentCommand::ReplaceContextPrefix {
                expected_revision: None,
                key_prefix: ContextEntryKey::new("instructions"),
                entries: BTreeMap::from([(
                    ContextEntryKey::new("instructions.050.profile"),
                    ContextEntryInput {
                        kind: ContextEntryKind::Instructions,
                        content: engine::ContentRef::text(BlobRef::from_bytes(b"prepared profile")),
                        preview: None,
                        origin: None,
                        provenance_ref: None,
                        token_estimate: None,
                    },
                )]),
            },
        ] {
            let CoreAgentAction::AppendEvents { events: next, .. } =
                staged.admit_command(command, 10).unwrap()
            else {
                panic!("fixture command must produce events");
            };
            let start = events.len() as u64;
            let entries = next
                .iter()
                .enumerate()
                .map(|(index, event)| StoredSessionEntry {
                    position: SessionPosition {
                        seq: EventSeq::new(start + index as u64 + 1),
                    },
                    observed_at_ms: event.observed_at_ms,
                    joins: event.joins.clone(),
                    event: event.event.clone(),
                })
                .collect();
            staged.resume_appended(entries).unwrap();
            events.extend(next);
        }
        let request = AppendEventsRequest {
            session_id: session_id.clone(),
            expected_head: None,
            events,
        };
        // The database committed, but the activity completion was lost.
        let committed = store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events: request.events.clone(),
            })
            .await
            .unwrap();
        let retry = append_events(&deps, request.clone()).await.unwrap();
        assert_eq!(retry, committed);
        assert_eq!(append_events(&deps, request).await.unwrap(), committed);
        let durable = read_all(store.as_ref(), &session_id).await;
        assert_eq!(durable.entries, committed.entries);
        let mut replay = CoreAgentDrive::from_replayed(session_id, CoreAgentState::new(), None);
        replay.resume_appended(durable.entries).unwrap();
        assert_eq!(replay.state(), staged.state());
        assert_eq!(replay.state().lifecycle.config_revision, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_emission_retry_and_restarted_reads_are_complete_across_pages() {
        use engine::{
            ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentCommand,
            FunctionToolSpec, ManagedSessionWorkflowTools, RunConfig, RunRequestCommand,
            RunRequestSource, ToolKind, ToolParallelism, ToolSpec, WorkflowEndpointRef,
            WorkflowToolDeclaration, WorkflowToolDefinition, WorkflowToolId,
        };
        use test_support::{DriveCommand, RunnerStores, SessionRunner};
        use uuid::Uuid;

        let store = Arc::new(InMemorySessionStore::new());
        let session_id = create_test_session(store.as_ref()).await;
        let universe_id = Uuid::from_u128(1);
        let controller_receiver = WorkflowEndpointRef {
            workflow_id: "work-controller".to_owned(),
            workflow_kind: "agent_work".to_owned(),
        };
        let approval_receiver = WorkflowEndpointRef {
            workflow_id: "approval-plugin".to_owned(),
            workflow_kind: "approval".to_owned(),
        };
        let declaration = ManagedSessionWorkflowTools::v1(
            Some(controller_receiver.clone()),
            vec![
                WorkflowToolDeclaration::bound_notify(
                    WorkflowToolDefinition {
                        tool_id: WorkflowToolId::new("work-report"),
                        revision: 1,
                        semantic_type: "lightspeed.work.report.v1".to_owned(),
                        tool: ToolSpec {
                            name: ToolName::new("work_report"),
                            execution: Default::default(),
                            kind: ToolKind::Function(FunctionToolSpec {
                                description_ref: None,
                                input_schema_ref: BlobRef::from_bytes(b"work-report-schema"),
                                output_schema_ref: None,
                                strict: Some(true),
                                provider_options_ref: None,
                            }),
                            parallelism: ToolParallelism::ParallelSafe,
                        },
                    },
                    controller_receiver.clone(),
                ),
                WorkflowToolDeclaration::bound_notify(
                    WorkflowToolDefinition {
                        tool_id: WorkflowToolId::new("approval-request"),
                        revision: 1,
                        semantic_type: "lightspeed.approval.request.v1".to_owned(),
                        tool: ToolSpec {
                            name: ToolName::new("request_approval"),
                            execution: Default::default(),
                            kind: ToolKind::Function(FunctionToolSpec {
                                description_ref: None,
                                input_schema_ref: BlobRef::from_bytes(b"approval-schema"),
                                output_schema_ref: None,
                                strict: Some(true),
                                provider_options_ref: None,
                            }),
                            parallelism: ToolParallelism::ParallelSafe,
                        },
                    },
                    approval_receiver.clone(),
                ),
            ],
        );
        let admitted = declaration
            .admit(universe_id)
            .expect("admit independently addressed workflow tools");
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let sessions: Arc<dyn SessionStore> = store.clone();
        let runner = SessionRunner::new(
            RunnerStores::new(sessions, blobs),
            Arc::new(WorkflowToolLlm {
                generations: AtomicUsize::new(0),
                tool_calls: vec![
                    ObservedToolCall {
                        call_id: engine::ToolCallId::new("report-call"),
                        tool_id: Some(ToolName::new("work_report")),
                        tool_name: ToolName::new("work_report"),
                        provider_kind: None,
                        arguments_ref: BlobRef::from_bytes(b"{\"outcome\":\"complete\"}"),
                        native_call_ref: None,
                    },
                    ObservedToolCall {
                        call_id: engine::ToolCallId::new("approval-call"),
                        tool_id: Some(ToolName::new("request_approval")),
                        tool_name: ToolName::new("request_approval"),
                        provider_kind: None,
                        arguments_ref: BlobRef::from_bytes(b"{\"reason\":\"ship it\"}"),
                        native_call_ref: None,
                    },
                ],
            }),
        )
        .with_tools(Arc::new(WorkflowToolTools {
            universe_id,
            bindings: admitted
                .bindings
                .iter()
                .map(|binding| (binding.definition.tool.name.clone(), binding.clone()))
                .collect(),
        }));

        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: CoreAgentCommand::OpenManagedSession {
                    config: volume_session_config(),
                    session_universe_id: universe_id,
                    workflow_tools: declaration,
                },
                max_steps: None,
            })
            .await
            .expect("open managed session");
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 11,
                command: CoreAgentCommand::ReplaceTools {
                    expected_revision: Some(0),
                    tools: admitted
                        .bindings
                        .iter()
                        .map(|binding| {
                            (
                                binding.definition.tool.name.clone(),
                                binding.definition.tool.clone(),
                            )
                        })
                        .collect(),
                },
                max_steps: None,
            })
            .await
            .expect("install workflow-tool tool");
        let completed = runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 12,
                command: CoreAgentCommand::RequestRun(RunRequestCommand {
                    notify_on_terminal: Vec::new(),
                    submission_id: None,
                    source: RunRequestSource::Input {
                        input: vec![ContextEntryInput {
                            kind: ContextEntryKind::Message {
                                role: ContextMessageRole::User,
                            },
                            content: engine::ContentRef {
                                content_ref: BlobRef::from_bytes(b"complete the work"),
                                media_type: None,
                                provider_kind: None,
                            },
                            preview: None,
                            origin: None,
                            provenance_ref: None,
                            token_estimate: None,
                        }],
                    },
                    run_config: RunConfig::default(),
                }),
                max_steps: None,
            })
            .await
            .expect("complete run with workflow-tool emission");
        let run = completed.state.runs.completed.last().expect("terminal run");
        let run_id = run.run_id;
        let expected = completed
            .state
            .workflow_tools
            .emissions
            .values()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(expected.len(), 2);
        let expected_report = expected
            .iter()
            .find(|invocation| invocation.tool_id.as_str() == "work-report")
            .cloned()
            .expect("durable work report");
        let expected_approval = expected
            .iter()
            .find(|invocation| invocation.tool_id.as_str() == "approval-request")
            .cloned()
            .expect("durable approval request");

        // Re-submit the exact committed tool-result append as an activity
        // retry. The activity must return the existing entries rather than
        // append a second copy of either emission.
        let durable_entries = read_all_session_events(store.as_ref(), &session_id)
            .await
            .expect("read durable run log");
        let tool_result_indices = durable_entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                matches!(
                    entry.event.kind.as_str(),
                    "lightspeed.core.tool.call_completed" | "lightspeed.core.workflow_tool.emitted"
                )
                .then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(tool_result_indices.len(), 4);
        let first_tool_result = tool_result_indices[0];
        let last_tool_result = *tool_result_indices.last().expect("tool result events");
        assert_eq!(
            tool_result_indices,
            (first_tool_result..=last_tool_result).collect::<Vec<_>>(),
            "the two successful calls and emissions must be one atomic append"
        );
        let expected_head = first_tool_result
            .checked_sub(1)
            .map(|index| durable_entries[index].position.clone());
        let retried_events = durable_entries[first_tool_result..=last_tool_result]
            .iter()
            .map(|entry| UncommittedStoredEvent {
                observed_at_ms: entry.observed_at_ms,
                joins: entry.joins.clone(),
                event: entry.event.clone(),
            })
            .collect::<Vec<_>>();

        // A fresh dependency bundle represents a restarted worker process:
        // the pull result is reconstructed only from the durable, paginated
        // session log and carries no workflow-local cursor or cache.
        let deps = storage_deps(store.clone());
        let retried = append_events(
            &deps,
            AppendEventsRequest {
                session_id: session_id.clone(),
                expected_head,
                events: retried_events,
            },
        )
        .await
        .expect("confirm duplicate tool-result append");
        assert_eq!(
            retried.entries,
            durable_entries[first_tool_result..=last_tool_result]
        );

        let reports = read_tool_emissions_with_page_limit(
            &deps,
            &controller_receiver,
            &session_id,
            run_id,
            2,
        )
        .await
        .expect("read controller emissions after terminal boundary");
        let approvals =
            read_tool_emissions_with_page_limit(&deps, &approval_receiver, &session_id, run_id, 2)
                .await
                .expect("read approval emissions after terminal boundary");

        assert_eq!(reports, vec![expected_report]);
        assert_eq!(approvals, vec![expected_approval]);
        let after_retry = read_all_session_events(store.as_ref(), &session_id)
            .await
            .expect("read log after retry");
        assert_eq!(
            after_retry
                .iter()
                .filter(|entry| { entry.event.kind == "lightspeed.core.workflow_tool.emitted" })
                .count(),
            2,
            "duplicate tool-result admission must not duplicate emissions"
        );
        read_tool_emissions_with_page_limit(
            &deps,
            &WorkflowEndpointRef {
                workflow_id: "unrelated-plugin".to_owned(),
                workflow_kind: "unrelated".to_owned(),
            },
            &session_id,
            run_id,
            2,
        )
        .await
        .expect_err("unbound receiver must not read session emissions");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_events_preserves_payload_conflict() {
        let store = Arc::new(InMemorySessionStore::new());
        let deps = storage_deps(store.clone());
        let session_id = create_test_session(store.as_ref()).await;
        let first = AppendEventsRequest {
            session_id: session_id.clone(),
            expected_head: None,
            events: vec![test_event(10, [("turn", "1")], json!({"value": "first"}))],
        };
        append_events(&deps, first)
            .await
            .expect("append first batch");

        let error = append_events(
            &deps,
            AppendEventsRequest {
                session_id,
                expected_head: None,
                events: vec![test_event(
                    10,
                    [("turn", "1")],
                    json!({"value": "different"}),
                )],
            },
        )
        .await
        .expect_err("different payload remains a conflict");

        assert_expected_head_mismatch(error);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_events_preserves_observed_time_and_join_conflict() {
        let store = Arc::new(InMemorySessionStore::new());
        let deps = storage_deps(store.clone());
        let session_id = create_test_session(store.as_ref()).await;
        let first = AppendEventsRequest {
            session_id: session_id.clone(),
            expected_head: None,
            events: vec![test_event(10, [("turn", "1")], json!({"value": "same"}))],
        };
        append_events(&deps, first)
            .await
            .expect("append first batch");

        let error = append_events(
            &deps,
            AppendEventsRequest {
                session_id,
                expected_head: None,
                events: vec![test_event(11, [("turn", "2")], json!({"value": "same"}))],
            },
        )
        .await
        .expect_err("different observed time and joins remain a conflict");

        assert_expected_head_mismatch(error);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_events_keeps_empty_append_as_noop() {
        let store = Arc::new(InMemorySessionStore::new());
        let deps = storage_deps(store.clone());
        let session_id = create_test_session(store.as_ref()).await;

        let result = append_events(
            &deps,
            AppendEventsRequest {
                session_id: session_id.clone(),
                expected_head: None,
                events: Vec::new(),
            },
        )
        .await
        .expect("empty append");

        assert!(result.entries.is_empty());
        assert_eq!(result.head, None);
        assert!(
            read_all(store.as_ref(), &session_id)
                .await
                .entries
                .is_empty()
        );
    }

    fn assert_expected_head_mismatch(error: ActivityError) {
        let ActivityError::Application(failure) = error else {
            panic!("expected application failure");
        };
        assert!(matches!(
            failure.source_error().downcast_ref::<SessionStoreError>(),
            Some(SessionStoreError::ExpectedHeadMismatch { .. })
        ));
    }
}

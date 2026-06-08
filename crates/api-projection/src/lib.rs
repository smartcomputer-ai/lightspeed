//! Projection helpers from CoreAgent's committed log to `api` views.
//!
//! This crate is the explicit bridge between reducer internals and the stable
//! client-facing API. It does not admit commands or execute side effects beyond
//! reading blob-backed text needed to materialize views.

use std::collections::BTreeMap;

use api::{
    AgentApiError, CompactionPolicyInput, ContextConfigInput, ContextEntryInputView,
    ContextEntryKindView, ContextMessageRoleView, ContextView, EventCursor, EventJoinsView,
    GenerationConfig, HostToolMode as ApiHostToolMode, InputItem, ModelConfig, ReasoningEffort,
    RunDefaultsConfig, RunStatus as ApiRunStatus, RunView, SessionConfigView, SessionEventKindView,
    SessionEventView, SessionItemView, SessionStatus as ApiSessionStatus, SessionView,
    TokenEstimateQualityView, TokenEstimateView, ToolBatchView, ToolCallDisplayGroup,
    ToolCallDisplayView, ToolCallEventView, ToolCallView, ToolConfigView, ToolEffectView,
    ToolExecutionTargetView, ToolItemStatus,
};
use engine::{ApplyEvent, ToolExecutionTarget};
use engine::{
    CompactionPolicy, ContextCompactionStatus, ContextCompactionTrigger, ContextEntry,
    ContextEntryId, ContextEntryInput, ContextEntryKind, ContextEntrySource, ContextEvent,
    ContextMessageRole, ContextRemovalReason, ContextRewriteReason, CoreAgentCodec, CoreAgentEntry,
    CoreAgentEventKind, CoreAgentJoins, CoreAgentLifecycleEvent, CoreAgentState, CoreAgentStatus,
    CoreApplyEvent, EventSeq, LlmGenerationStatus, ModelProviderOptions, ModelSelection,
    ObservedToolCall, ProviderApiKind, ProviderRequestDefaults, RunEvent, RunFailure, RunId,
    RunStatus, SessionConfig, SessionId, SteeringId, ToolBatchId, ToolCallStatus, ToolConfigEvent,
    ToolEvent, TurnEvent, TurnId,
    storage::{
        BlobStore, BlobStoreError, DynamicSessionEntry, ReadSessionEvents, SessionRecord,
        SessionStore, SessionStoreError,
    },
};
use serde_json::Value;

pub const DEFAULT_EVENT_PAGE_LIMIT: u32 = 128;
pub const MAX_EVENT_PAGE_LIMIT: u32 = 512;

pub struct ProjectSession<'a> {
    pub session_id: &'a SessionId,
    pub state: &'a CoreAgentState,
    pub record: &'a SessionRecord,
    pub entries: &'a [CoreAgentEntry],
    pub cwd: Option<String>,
}

pub struct CoreAgentProjector<'a> {
    blobs: &'a dyn BlobStore,
}

impl<'a> CoreAgentProjector<'a> {
    pub fn new(blobs: &'a dyn BlobStore) -> Self {
        Self { blobs }
    }

    pub async fn project_session(
        &self,
        params: ProjectSession<'_>,
    ) -> Result<SessionView, AgentApiError> {
        let mut runs = Vec::new();

        for record in &params.state.runs.completed {
            runs.push(
                self.project_run(params.entries, record.run_id, record.status)
                    .await?,
            );
        }
        if let Some(active_run) = params.state.runs.active.as_ref() {
            runs.push(
                self.project_run(params.entries, active_run.run_id, active_run.status)
                    .await?,
            );
        }

        let config = match params.state.lifecycle.config.as_ref() {
            Some(config) => Some(self.project_session_config(config).await?),
            None => None,
        };

        Ok(SessionView {
            id: params.session_id.as_str().to_owned(),
            status: session_status(params.state),
            cwd: params.cwd,
            config_revision: params.state.lifecycle.config_revision,
            config,
            created_at_ms: params.record.created_at_ms,
            updated_at_ms: params.record.updated_at_ms,
            runs,
            active_context: self
                .project_context_state(params.state.context.revision, &params.state.context.entries)
                .await?,
            vfs_mounts: Vec::new(),
        })
    }

    pub async fn project_run(
        &self,
        entries: &[CoreAgentEntry],
        run_id: RunId,
        status: RunStatus,
    ) -> Result<RunView, AgentApiError> {
        let projection = CoreAgentProjection::new(entries);
        let context_entries = projection.context_entries_for_run(run_id);
        let mut items = Vec::new();

        for item in &context_entries {
            let projected = self.project_item(item).await?;
            items.push(projected);
        }

        Ok(RunView {
            id: api_run_id(run_id),
            status: core_run_status_to_api_status(status),
            input: self
                .project_input_entries(&projection.accepted_input_for_run(run_id))
                .await?,
            items,
            tool_batches: self
                .project_tool_batches_for_run(&projection, &context_entries, run_id)
                .await?,
        })
    }

    pub async fn project_context_state(
        &self,
        revision: u64,
        entries: &[ContextEntry],
    ) -> Result<ContextView, AgentApiError> {
        let mut items = Vec::with_capacity(entries.len());
        for entry in entries {
            items.push(self.project_item(entry).await?);
        }
        Ok(ContextView { revision, items })
    }

    pub async fn project_item(
        &self,
        item: &ContextEntry,
    ) -> Result<SessionItemView, AgentApiError> {
        let id = api_item_id(item.entry_id);
        match &item.kind {
            ContextEntryKind::Message { role } => {
                let text = self.read_blob_text(&item.content_ref).await?;
                match role {
                    ContextMessageRole::User => Ok(SessionItemView::UserMessage { id, text }),
                    ContextMessageRole::Assistant => {
                        Ok(SessionItemView::AssistantMessage { id, text })
                    }
                }
            }
            ContextEntryKind::ToolCall { call_id, name } => Ok(SessionItemView::ToolCall {
                id,
                call_id: call_id.as_str().to_owned(),
                tool_name: name.as_str().to_owned(),
                arguments: Some(self.read_blob_text(&item.content_ref).await?),
                status: ToolItemStatus::Requested,
            }),
            ContextEntryKind::ToolResult { call_id, is_error } => Ok(SessionItemView::ToolResult {
                id,
                call_id: call_id.as_str().to_owned(),
                output: Some(self.read_blob_text(&item.content_ref).await?),
                is_error: *is_error,
                status: if *is_error {
                    ToolItemStatus::Failed
                } else {
                    ToolItemStatus::Succeeded
                },
            }),
            ContextEntryKind::Instructions => Ok(SessionItemView::SystemEvent {
                id,
                text: item
                    .preview
                    .clone()
                    .unwrap_or_else(|| "instructions".to_owned()),
            }),
            ContextEntryKind::SkillCatalog => Ok(SessionItemView::SystemEvent {
                id,
                text: item
                    .preview
                    .clone()
                    .unwrap_or_else(|| "skills catalog".to_owned()),
            }),
            ContextEntryKind::SkillActivation { skill_id } => Ok(SessionItemView::SystemEvent {
                id,
                text: item
                    .preview
                    .clone()
                    .unwrap_or_else(|| format!("skill activated: {skill_id}")),
            }),
            ContextEntryKind::ReasoningState => Ok(SessionItemView::SystemEvent {
                id,
                text: item
                    .preview
                    .clone()
                    .unwrap_or_else(|| "context item".to_owned()),
            }),
            ContextEntryKind::ProviderOpaque => Ok(SessionItemView::ProviderContext {
                id,
                content_ref: item.content_ref.as_str().to_owned(),
                media_type: item.media_type.clone(),
                preview: item.preview.clone(),
                provider_kind: item.provider_kind.clone(),
                provider_item_id: item.provider_item_id.clone(),
                token_estimate: item.token_estimate.as_ref().map(token_estimate_to_api),
            }),
        }
    }

    pub async fn project_input_entries(
        &self,
        input: &[ContextEntryInput],
    ) -> Result<Vec<InputItem>, AgentApiError> {
        let mut projected = Vec::with_capacity(input.len());
        for entry in input {
            match entry.kind {
                ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                } => projected.push(InputItem::Text {
                    text: self.read_blob_text(&entry.content_ref).await?,
                }),
                _ => projected.push(InputItem::TextRef {
                    blob_ref: entry.content_ref.as_str().to_owned(),
                }),
            }
        }
        Ok(projected)
    }

    pub async fn project_session_config(
        &self,
        config: &SessionConfig,
    ) -> Result<SessionConfigView, AgentApiError> {
        Ok(SessionConfigView {
            model: model_to_api(&config.model),
            generation: GenerationConfig {
                max_output_tokens: config.turn.max_output_tokens,
                reasoning_effort: reasoning_effort_to_api(&config.turn.provider_request_defaults),
            },
            context: ContextConfigInput {
                compaction: config
                    .context
                    .compaction
                    .as_ref()
                    .map(compaction_policy_to_api),
            },
            run_defaults: RunDefaultsConfig {
                max_turns: config.run.max_turns,
                max_tool_rounds: config.run.max_tool_rounds,
            },
            tools: ToolConfigView {
                web_search: effective_web_search_enabled(config),
                web_fetch: effective_web_fetch_enabled(config),
                host: host_tool_mode_to_api(effective_host_tool_mode(config)),
            },
        })
    }

    pub async fn project_entry(
        &self,
        session_id: &SessionId,
        entry: &CoreAgentEntry,
    ) -> Result<SessionEventView, AgentApiError> {
        Ok(SessionEventView {
            cursor: event_cursor(entry.position.seq),
            session_id: session_id.as_str().to_owned(),
            observed_at_ms: entry.observed_at_ms,
            joins: event_joins_to_api(&entry.joins),
            kind: self.project_event_kind(&entry.event.kind).await?,
        })
    }

    pub async fn project_event_kind(
        &self,
        kind: &CoreAgentEventKind,
    ) -> Result<SessionEventKindView, AgentApiError> {
        match kind {
            CoreAgentEventKind::Lifecycle(event) => match event {
                CoreAgentLifecycleEvent::Opened { config } => {
                    Ok(SessionEventKindView::SessionOpened {
                        model: Some(model_to_api(&config.model)),
                    })
                }
                CoreAgentLifecycleEvent::ConfigChanged { config, revision } => {
                    Ok(SessionEventKindView::SessionConfigChanged {
                        model: Some(model_to_api(&config.model)),
                        revision: *revision,
                    })
                }
                CoreAgentLifecycleEvent::Closed => Ok(SessionEventKindView::SessionClosed),
            },
            CoreAgentEventKind::Run(event) => match event {
                RunEvent::Accepted {
                    run_id,
                    submission_id,
                    input,
                    ..
                } => Ok(SessionEventKindView::RunAccepted {
                    run_id: api_run_id(*run_id),
                    submission_id: submission_id.as_ref().map(|id| id.as_str().to_owned()),
                    input: project_context_entry_inputs(input),
                }),
                RunEvent::Started { run_id } => Ok(SessionEventKindView::RunStarted {
                    run_id: api_run_id(*run_id),
                }),
                RunEvent::SteeringAccepted {
                    run_id,
                    steering_id,
                    input,
                } => Ok(SessionEventKindView::RunSteeringAccepted {
                    run_id: api_run_id(*run_id),
                    steering_id: api_steering_id(*steering_id),
                    input: project_context_entry_inputs(input),
                }),
                RunEvent::CancellationRequested { run_id } => {
                    Ok(SessionEventKindView::RunCancellationRequested {
                        run_id: api_run_id(*run_id),
                    })
                }
                RunEvent::Completed { run_id, output_ref } => {
                    Ok(SessionEventKindView::RunCompleted {
                        run_id: api_run_id(*run_id),
                        output_ref: output_ref.as_ref().map(|ref_| ref_.as_str().to_owned()),
                    })
                }
                RunEvent::Failed { run_id, failure } => Ok(SessionEventKindView::RunFailed {
                    run_id: api_run_id(*run_id),
                    message: self.run_failure_message(failure).await,
                }),
                RunEvent::Cancelled { run_id } => Ok(SessionEventKindView::RunCancelled {
                    run_id: api_run_id(*run_id),
                }),
            },
            CoreAgentEventKind::Turn(event) => match event {
                TurnEvent::Started { turn_id, run_id } => Ok(SessionEventKindView::TurnStarted {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                }),
                TurnEvent::Planned {
                    turn_id, run_id, ..
                } => Ok(SessionEventKindView::TurnPlanned {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                }),
                TurnEvent::GenerationRequested { turn_id, run_id } => {
                    Ok(SessionEventKindView::TurnGenerationRequested {
                        run_id: api_run_id(*run_id),
                        turn_id: api_turn_id(*turn_id),
                    })
                }
                TurnEvent::GenerationCompleted {
                    turn_id,
                    run_id,
                    status,
                    ..
                } => Ok(SessionEventKindView::TurnGenerationCompleted {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                    status: llm_generation_status_to_api(status).to_owned(),
                }),
                TurnEvent::Completed { turn_id, .. } => Ok(SessionEventKindView::TurnCompleted {
                    turn_id: api_turn_id(*turn_id),
                }),
            },
            CoreAgentEventKind::Context(event) => match event {
                ContextEvent::EntriesApplied {
                    base_revision,
                    entries,
                } => {
                    let mut projected = Vec::with_capacity(entries.len());
                    for entry in entries {
                        projected.push(self.project_item(entry).await?);
                    }
                    Ok(SessionEventKindView::ContextEntriesApplied {
                        base_revision: *base_revision,
                        revision: context_event_revision(*base_revision)?,
                        items: projected,
                    })
                }
                ContextEvent::EntriesRemoved {
                    base_revision,
                    entry_ids,
                    reason,
                } => Ok(SessionEventKindView::ContextEntriesRemoved {
                    base_revision: *base_revision,
                    revision: context_event_revision(*base_revision)?,
                    item_ids: entry_ids
                        .iter()
                        .map(|entry_id| api_item_id(*entry_id))
                        .collect(),
                    reason: context_removal_reason_to_api(reason).to_owned(),
                }),
                ContextEvent::KeysRemoved {
                    base_revision,
                    keys,
                } => Ok(SessionEventKindView::ContextKeysRemoved {
                    base_revision: *base_revision,
                    revision: context_event_revision(*base_revision)?,
                    keys: keys.iter().map(|key| key.as_str().to_owned()).collect(),
                }),
                ContextEvent::KeyPrefixReplaced {
                    base_revision,
                    key_prefix,
                    entries,
                } => {
                    let mut projected = Vec::with_capacity(entries.len());
                    for entry in entries {
                        projected.push(self.project_item(entry).await?);
                    }
                    Ok(SessionEventKindView::ContextKeyPrefixReplaced {
                        base_revision: *base_revision,
                        revision: context_event_revision(*base_revision)?,
                        key_prefix: key_prefix.as_str().to_owned(),
                        items: projected,
                    })
                }
                ContextEvent::StateReplaced {
                    base_revision,
                    entries,
                    reason,
                } => {
                    let mut projected = Vec::with_capacity(entries.len());
                    for entry in entries {
                        projected.push(self.project_item(entry).await?);
                    }
                    Ok(SessionEventKindView::ContextStateReplaced {
                        base_revision: *base_revision,
                        revision: context_event_revision(*base_revision)?,
                        items: projected,
                        reason: context_rewrite_reason_to_api(reason).to_owned(),
                    })
                }
                ContextEvent::CompactionRequested {
                    base_revision,
                    trigger,
                } => Ok(SessionEventKindView::ContextCompactionRequested {
                    base_revision: *base_revision,
                    revision: context_event_revision(*base_revision)?,
                    trigger: context_compaction_trigger_to_api(*trigger).to_owned(),
                }),
                ContextEvent::CompactionFinished {
                    base_revision,
                    status,
                    failure_ref,
                } => Ok(SessionEventKindView::ContextCompactionFinished {
                    base_revision: *base_revision,
                    revision: context_event_revision(*base_revision)?,
                    status: context_compaction_status_to_api(*status).to_owned(),
                    failure_ref: failure_ref
                        .as_ref()
                        .map(|blob_ref| blob_ref.as_str().to_owned()),
                }),
            },
            CoreAgentEventKind::ToolConfig(event) => match event {
                ToolConfigEvent::RegistryChanged { .. } => {
                    Ok(SessionEventKindView::ToolRegistryChanged)
                }
                ToolConfigEvent::ProfileSelected { profile_id } => {
                    Ok(SessionEventKindView::ToolProfileSelected {
                        profile_id: profile_id.as_str().to_owned(),
                    })
                }
                ToolConfigEvent::DefaultTargetSet { target } => {
                    Ok(SessionEventKindView::ToolDefaultTargetChanged {
                        namespace: target.namespace.clone(),
                        target: Some(tool_execution_target_to_api(target)),
                    })
                }
                ToolConfigEvent::DefaultTargetCleared { namespace } => {
                    Ok(SessionEventKindView::ToolDefaultTargetChanged {
                        namespace: namespace.clone(),
                        target: None,
                    })
                }
            },
            CoreAgentEventKind::Tool(event) => match event {
                ToolEvent::BatchStarted {
                    run_id,
                    turn_id,
                    batch_id,
                    calls,
                } => Ok(SessionEventKindView::ToolBatchStarted {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                    batch_id: api_tool_batch_id(*batch_id),
                    calls: self.project_tool_call_events(calls).await?,
                }),
                ToolEvent::CallStarted {
                    run_id,
                    turn_id,
                    batch_id,
                    call_id,
                    ..
                } => Ok(SessionEventKindView::ToolCallStarted {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                    batch_id: api_tool_batch_id(*batch_id),
                    call_id: call_id.as_str().to_owned(),
                }),
                ToolEvent::CallCompleted {
                    run_id,
                    turn_id,
                    batch_id,
                    result,
                } => Ok(SessionEventKindView::ToolCallCompleted {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                    batch_id: api_tool_batch_id(*batch_id),
                    call_id: result.call_id.as_str().to_owned(),
                    status: core_tool_status_to_api_status(result.status),
                    effects: tool_effects_to_api(&result.effects),
                }),
                ToolEvent::BatchCompleted {
                    run_id,
                    turn_id,
                    batch_id,
                } => Ok(SessionEventKindView::ToolBatchCompleted {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                    batch_id: api_tool_batch_id(*batch_id),
                }),
            },
        }
    }

    async fn project_tool_call_events(
        &self,
        calls: &[ObservedToolCall],
    ) -> Result<Vec<ToolCallEventView>, AgentApiError> {
        let mut projected = Vec::with_capacity(calls.len());
        for call in calls {
            let arguments = self.read_blob_text(&call.arguments_ref).await?;
            projected.push(ToolCallEventView {
                call_id: call.call_id.as_str().to_owned(),
                tool_name: call.tool_name.as_str().to_owned(),
                arguments_ref: call.arguments_ref.as_str().to_owned(),
                arguments: Some(arguments.clone()),
                display: tool_call_display(call.tool_name.as_str(), &arguments),
            });
        }
        Ok(projected)
    }

    async fn project_tool_batches_for_run(
        &self,
        projection: &CoreAgentProjection<'_>,
        context_entries: &[&ContextEntry],
        run_id: RunId,
    ) -> Result<Vec<ToolBatchView>, AgentApiError> {
        let result_by_call = self.project_tool_results_for_run(context_entries).await?;
        let effect_by_call = tool_effects_for_run(projection, run_id);
        let mut batches = Vec::new();
        let mut completed_batches = BTreeMap::new();

        for entry in projection.entries() {
            let CoreAgentEventKind::Tool(event) = &entry.event.kind else {
                continue;
            };
            match event {
                ToolEvent::BatchStarted {
                    run_id: event_run_id,
                    turn_id,
                    batch_id,
                    calls,
                } if *event_run_id == run_id => {
                    let mut projected_calls = Vec::with_capacity(calls.len());
                    for call in calls {
                        let result = result_by_call.get(call.call_id.as_str());
                        let arguments = self.read_blob_text(&call.arguments_ref).await?;
                        projected_calls.push(ToolCallView {
                            call_id: call.call_id.as_str().to_owned(),
                            tool_name: call.tool_name.as_str().to_owned(),
                            arguments_ref: call.arguments_ref.as_str().to_owned(),
                            arguments: Some(arguments.clone()),
                            output: result.and_then(|result| result.output.clone()),
                            is_error: result.is_some_and(|result| result.is_error),
                            status: result
                                .map(|result| result.status)
                                .unwrap_or(ToolItemStatus::Running),
                            effects: effect_by_call
                                .get(call.call_id.as_str())
                                .cloned()
                                .unwrap_or_default(),
                            display: tool_call_display(call.tool_name.as_str(), &arguments),
                        });
                    }
                    batches.push(ToolBatchView {
                        id: api_tool_batch_id(*batch_id),
                        turn_id: api_turn_id(*turn_id),
                        status: ToolItemStatus::Running,
                        calls: projected_calls,
                    });
                }
                ToolEvent::BatchCompleted {
                    run_id: event_run_id,
                    batch_id,
                    ..
                } if *event_run_id == run_id => {
                    completed_batches.insert(api_tool_batch_id(*batch_id), true);
                }
                _ => {}
            }
        }

        for batch in &mut batches {
            if completed_batches.contains_key(&batch.id) {
                for call in &mut batch.calls {
                    if matches!(
                        call.status,
                        ToolItemStatus::Running | ToolItemStatus::Requested
                    ) {
                        call.status = ToolItemStatus::Unavailable;
                    }
                }
            }
            batch.status = aggregate_api_tool_status(&batch.calls);
        }

        Ok(batches)
    }

    async fn project_tool_results_for_run(
        &self,
        context_entries: &[&ContextEntry],
    ) -> Result<BTreeMap<String, ProjectedToolResult>, AgentApiError> {
        let mut result_by_call = BTreeMap::new();
        for item in context_entries {
            let ContextEntryKind::ToolResult { call_id, is_error } = &item.kind else {
                continue;
            };
            result_by_call.insert(
                call_id.as_str().to_owned(),
                ProjectedToolResult {
                    output: Some(self.read_blob_text(&item.content_ref).await?),
                    is_error: *is_error,
                    status: if *is_error {
                        ToolItemStatus::Failed
                    } else {
                        ToolItemStatus::Succeeded
                    },
                },
            );
        }
        Ok(result_by_call)
    }

    async fn read_blob_text(&self, blob_ref: &engine::BlobRef) -> Result<String, AgentApiError> {
        self.blobs
            .read_text(blob_ref)
            .await
            .map_err(map_blob_store_error)
    }

    async fn run_failure_message(&self, failure: &RunFailure) -> String {
        if let Some(message_ref) = &failure.message_ref
            && let Ok(message) = self.read_blob_text(message_ref).await
        {
            return message;
        }
        format!("{:?}", failure.kind)
    }
}

#[derive(Clone, Debug)]
struct ProjectedToolResult {
    output: Option<String>,
    is_error: bool,
    status: ToolItemStatus,
}

pub struct CoreAgentProjection<'a> {
    entries: &'a [CoreAgentEntry],
}

impl<'a> CoreAgentProjection<'a> {
    pub fn new(entries: &'a [CoreAgentEntry]) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &'a [CoreAgentEntry] {
        self.entries
    }

    pub fn accepted_input_for_run(&self, run_id: RunId) -> Vec<ContextEntryInput> {
        self.entries
            .iter()
            .find_map(|entry| {
                let CoreAgentEventKind::Run(RunEvent::Accepted {
                    run_id: event_run_id,
                    input,
                    ..
                }) = &entry.event.kind
                else {
                    return None;
                };
                (*event_run_id == run_id).then(|| input.clone())
            })
            .unwrap_or_default()
    }

    pub fn context_entries_for_run(&self, run_id: RunId) -> Vec<&'a ContextEntry> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let CoreAgentEventKind::Context(ContextEvent::EntriesApplied { entries, .. }) =
                    &entry.event.kind
                else {
                    return None;
                };
                Some(
                    entries
                        .iter()
                        .filter(move |entry| context_entry_run_id(entry) == Some(run_id)),
                )
            })
            .flatten()
            .collect()
    }
}

pub fn context_entry_run_id(entry: &ContextEntry) -> Option<RunId> {
    match &entry.source {
        ContextEntrySource::RunInput { run_id, .. }
        | ContextEntrySource::Steering { run_id, .. }
        | ContextEntrySource::AssistantOutput { run_id, .. }
        | ContextEntrySource::Tool { run_id, .. }
        | ContextEntrySource::Reasoning { run_id, .. } => Some(*run_id),
        ContextEntrySource::ContextEdit | ContextEntrySource::Runtime { .. } => None,
    }
}

pub async fn read_all_session_entries(
    sessions: &dyn SessionStore,
    session_id: &SessionId,
    page_limit: usize,
) -> Result<Vec<CoreAgentEntry>, AgentApiError> {
    let mut after = None;
    let mut entries = Vec::new();
    let codec = CoreAgentCodec;
    loop {
        let page = sessions
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
                after,
                limit: page_limit,
            })
            .await
            .map_err(map_session_store_error)?;
        after = page.next_after;
        for entry in &page.entries {
            entries.push(decode_dynamic_entry(&codec, entry)?);
        }
        if page.complete {
            return Ok(entries);
        }
    }
}

pub fn decode_dynamic_entry(
    codec: &CoreAgentCodec,
    entry: &DynamicSessionEntry,
) -> Result<CoreAgentEntry, AgentApiError> {
    codec
        .decode_entry(entry)
        .map_err(|error| AgentApiError::internal(error.to_string()))
}

pub fn replay_core_agent_state(
    entries: &[CoreAgentEntry],
) -> Result<CoreAgentState, AgentApiError> {
    let mut state = CoreAgentState::new();
    let apply = CoreApplyEvent;
    for entry in entries {
        apply
            .apply(&mut state, entry)
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
    }
    Ok(state)
}

pub fn input_text(input: &[InputItem]) -> Result<String, AgentApiError> {
    let mut parts = Vec::new();
    for item in input {
        match item {
            InputItem::Text { text } => {
                let text = text.trim();
                if !text.is_empty() {
                    parts.push(text);
                }
            }
            InputItem::TextRef { .. } => {
                return Err(AgentApiError::invalid_request(
                    "run/start textRef input requires blob store resolution",
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(AgentApiError::invalid_request(
            "run/start input must contain at least one non-empty text item",
        ));
    }
    Ok(parts.join("\n\n"))
}

pub fn event_page_limit(limit: Option<u32>) -> Result<usize, AgentApiError> {
    let limit = limit.unwrap_or(DEFAULT_EVENT_PAGE_LIMIT);
    if limit == 0 || limit > MAX_EVENT_PAGE_LIMIT {
        return Err(AgentApiError::invalid_request(format!(
            "session/events/read limit must be between 1 and {MAX_EVENT_PAGE_LIMIT}"
        )));
    }
    usize::try_from(limit)
        .map_err(|_| AgentApiError::invalid_request("session/events/read limit is too large"))
}

pub fn event_cursor(seq: EventSeq) -> EventCursor {
    EventCursor { seq: seq.as_u64() }
}

pub fn started_run_id(entries: &[CoreAgentEntry]) -> Option<RunId> {
    entries.iter().find_map(|entry| match &entry.event.kind {
        CoreAgentEventKind::Run(RunEvent::Started { run_id, .. }) => Some(*run_id),
        _ => None,
    })
}

pub fn api_run_id(run_id: RunId) -> String {
    format!("run_{}", run_id.as_u64())
}

pub fn api_steering_id(steering_id: SteeringId) -> String {
    format!("steering_{}", steering_id.as_u64())
}

pub fn api_item_id(entry_id: ContextEntryId) -> String {
    format!("item_{}", entry_id.as_u64())
}

pub fn parse_api_run_id(value: &str) -> Result<RunId, AgentApiError> {
    let raw = value.strip_prefix("run_").ok_or_else(|| {
        AgentApiError::invalid_request(format!("run id must use run_<number> form: {value}"))
    })?;
    raw.parse::<u64>()
        .map(RunId::new)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid run id {value}: {error}")))
}

pub fn api_turn_id(turn_id: TurnId) -> String {
    format!("turn_{}", turn_id.as_u64())
}

pub fn api_tool_batch_id(batch_id: ToolBatchId) -> String {
    format!("tool_batch_{}", batch_id.as_u64())
}

fn context_event_revision(base_revision: u64) -> Result<u64, AgentApiError> {
    base_revision
        .checked_add(1)
        .ok_or_else(|| AgentApiError::internal("context event revision overflow"))
}

fn context_removal_reason_to_api(reason: &ContextRemovalReason) -> &'static str {
    match reason {
        ContextRemovalReason::Pruned => "pruned",
        ContextRemovalReason::ProviderCompacted => "providerCompacted",
    }
}

fn context_rewrite_reason_to_api(reason: &ContextRewriteReason) -> &'static str {
    match reason {
        ContextRewriteReason::Pruned => "pruned",
        ContextRewriteReason::PolicyChanged => "policyChanged",
        ContextRewriteReason::ProviderCompacted => "providerCompacted",
    }
}

fn context_compaction_trigger_to_api(trigger: ContextCompactionTrigger) -> &'static str {
    match trigger {
        ContextCompactionTrigger::Manual => "manual",
        ContextCompactionTrigger::HighWatermark => "highWatermark",
    }
}

fn context_compaction_status_to_api(status: ContextCompactionStatus) -> &'static str {
    match status {
        ContextCompactionStatus::Succeeded => "succeeded",
        ContextCompactionStatus::Failed => "failed",
    }
}

fn compaction_policy_to_api(policy: &CompactionPolicy) -> CompactionPolicyInput {
    match policy {
        CompactionPolicy::Disabled => CompactionPolicyInput::Disabled,
        CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens,
        } => CompactionPolicyInput::ProviderTriggered {
            compact_threshold_tokens: *compact_threshold_tokens,
        },
        CompactionPolicy::ProviderStandalone {
            compact_threshold_tokens,
            target_tokens,
        } => CompactionPolicyInput::ProviderStandalone {
            compact_threshold_tokens: *compact_threshold_tokens,
            target_tokens: *target_tokens,
        },
    }
}

pub fn event_joins_to_api(joins: &CoreAgentJoins) -> EventJoinsView {
    EventJoinsView {
        run_id: joins.run_id.map(api_run_id),
        turn_id: joins.turn_id.map(api_turn_id),
        tool_batch_id: joins.tool_batch_id.map(api_tool_batch_id),
        tool_call_id: joins
            .tool_call_id
            .as_ref()
            .map(|call_id| call_id.as_str().to_owned()),
        submission_id: joins
            .submission_id
            .as_ref()
            .map(|submission_id| submission_id.as_str().to_owned()),
        correlation_id: joins
            .correlation_id
            .as_ref()
            .map(|correlation_id| correlation_id.as_str().to_owned()),
    }
}

pub fn session_status(state: &CoreAgentState) -> ApiSessionStatus {
    match state.lifecycle.status {
        CoreAgentStatus::New => ApiSessionStatus::NotLoaded,
        CoreAgentStatus::Closed => ApiSessionStatus::Closed,
        CoreAgentStatus::Open if state.runs.active.is_some() => ApiSessionStatus::Active,
        CoreAgentStatus::Open => ApiSessionStatus::Idle,
    }
}

pub fn core_run_status_to_api_status(status: RunStatus) -> ApiRunStatus {
    match status {
        RunStatus::Active => ApiRunStatus::Running,
        RunStatus::Cancelling => ApiRunStatus::Cancelling,
        RunStatus::Completed => ApiRunStatus::Completed,
        RunStatus::Failed => ApiRunStatus::Failed,
        RunStatus::Cancelled => ApiRunStatus::Cancelled,
    }
}

pub fn core_tool_status_to_api_status(status: ToolCallStatus) -> ToolItemStatus {
    match status {
        ToolCallStatus::Observed | ToolCallStatus::Accepted => ToolItemStatus::Requested,
        ToolCallStatus::Pending => ToolItemStatus::Running,
        ToolCallStatus::Succeeded => ToolItemStatus::Succeeded,
        ToolCallStatus::Failed | ToolCallStatus::Cancelled => ToolItemStatus::Failed,
        ToolCallStatus::Unavailable => ToolItemStatus::Unavailable,
    }
}

pub fn model_to_api(model: &ModelSelection) -> ModelConfig {
    ModelConfig {
        provider_id: model.provider_id.clone(),
        api_kind: api_kind_to_str(&model.api_kind).to_owned(),
        model: model.model.clone(),
    }
}

fn reasoning_effort_to_api(defaults: &ProviderRequestDefaults) -> Option<ReasoningEffort> {
    match defaults {
        ProviderRequestDefaults::OpenAiResponses(defaults) => {
            match defaults
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.effort.as_deref().map(str::to_ascii_lowercase))
            {
                Some(value) if value == "low" => Some(ReasoningEffort::Low),
                Some(value) if value == "medium" => Some(ReasoningEffort::Medium),
                Some(value) if value == "high" => Some(ReasoningEffort::High),
                Some(_) | None => None,
            }
        }
        _ => None,
    }
}

fn effective_web_search_enabled(config: &SessionConfig) -> bool {
    config.model.api_kind == ProviderApiKind::OpenAiResponses
        && config.tools.web_search.unwrap_or(true)
}

fn effective_web_fetch_enabled(config: &SessionConfig) -> bool {
    config.tools.web_fetch.unwrap_or(true)
}

fn effective_host_tool_mode(config: &SessionConfig) -> engine::HostToolMode {
    config.tools.host.unwrap_or(engine::HostToolMode::Edit)
}

fn host_tool_mode_to_api(mode: engine::HostToolMode) -> ApiHostToolMode {
    match mode {
        engine::HostToolMode::None => ApiHostToolMode::None,
        engine::HostToolMode::ReadOnly => ApiHostToolMode::ReadOnly,
        engine::HostToolMode::Edit => ApiHostToolMode::Edit,
    }
}

pub fn session_config_for_api_model(
    default_config: &SessionConfig,
    model: Option<ModelConfig>,
) -> Result<SessionConfig, AgentApiError> {
    let Some(model) = model else {
        return Ok(default_config.clone());
    };
    let mut config = default_config.clone();
    config.model = ModelSelection {
        api_kind: api_kind_from_str(&model.api_kind)?,
        provider_id: model.provider_id,
        model: model.model,
        options: ModelProviderOptions::None,
    };
    config
        .validate_provider_compatibility()
        .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
    Ok(config)
}

pub fn api_kind_to_str(api_kind: &ProviderApiKind) -> &'static str {
    match api_kind {
        ProviderApiKind::OpenAiResponses => "openai:responses",
        ProviderApiKind::AnthropicMessages => "anthropic:messages",
        ProviderApiKind::OpenAiCompletions => "openai:completions",
    }
}

pub fn api_kind_from_str(value: &str) -> Result<ProviderApiKind, AgentApiError> {
    match value {
        "openai:responses" | "openai_responses" | "openAiResponses" => {
            Ok(ProviderApiKind::OpenAiResponses)
        }
        "anthropic:messages" | "anthropic_messages" | "anthropicMessages" => {
            Ok(ProviderApiKind::AnthropicMessages)
        }
        "openai:completions" | "openai_completions" | "openAiCompletions" => {
            Ok(ProviderApiKind::OpenAiCompletions)
        }
        _ => Err(AgentApiError::invalid_request(format!(
            "unsupported provider api kind: {value}"
        ))),
    }
}

pub fn project_context_entry_inputs(input: &[ContextEntryInput]) -> Vec<ContextEntryInputView> {
    input
        .iter()
        .map(|entry| ContextEntryInputView {
            kind: context_entry_kind_to_api(&entry.kind),
            content_ref: entry.content_ref.as_str().to_owned(),
            media_type: entry.media_type.clone(),
            preview: entry.preview.clone(),
            provider_kind: entry.provider_kind.clone(),
            provider_item_id: entry.provider_item_id.clone(),
            token_estimate: entry.token_estimate.as_ref().map(token_estimate_to_api),
        })
        .collect()
}

fn context_entry_kind_to_api(kind: &ContextEntryKind) -> ContextEntryKindView {
    match kind {
        ContextEntryKind::Message { role } => ContextEntryKindView::Message {
            role: context_message_role_to_api(role),
        },
        ContextEntryKind::Instructions => ContextEntryKindView::Instructions,
        ContextEntryKind::SkillCatalog => ContextEntryKindView::SkillCatalog,
        ContextEntryKind::SkillActivation { skill_id } => ContextEntryKindView::SkillActivation {
            skill_id: skill_id.as_str().to_owned(),
        },
        ContextEntryKind::ToolCall { call_id, name } => ContextEntryKindView::ToolCall {
            call_id: call_id.as_str().to_owned(),
            name: name.as_str().to_owned(),
        },
        ContextEntryKind::ToolResult { call_id, is_error } => ContextEntryKindView::ToolResult {
            call_id: call_id.as_str().to_owned(),
            is_error: *is_error,
        },
        ContextEntryKind::ReasoningState => ContextEntryKindView::ReasoningState,
        ContextEntryKind::ProviderOpaque => ContextEntryKindView::ProviderOpaque,
    }
}

fn context_message_role_to_api(role: &ContextMessageRole) -> ContextMessageRoleView {
    match role {
        ContextMessageRole::User => ContextMessageRoleView::User,
        ContextMessageRole::Assistant => ContextMessageRoleView::Assistant,
    }
}

fn token_estimate_to_api(estimate: &engine::TokenEstimate) -> TokenEstimateView {
    TokenEstimateView {
        tokens: estimate.tokens,
        quality: token_estimate_quality_to_api(estimate.quality),
    }
}

fn token_estimate_quality_to_api(
    quality: engine::TokenEstimateQuality,
) -> TokenEstimateQualityView {
    match quality {
        engine::TokenEstimateQuality::Exact => TokenEstimateQualityView::Exact,
        engine::TokenEstimateQuality::ProviderCounted => TokenEstimateQualityView::ProviderCounted,
        engine::TokenEstimateQuality::Estimated => TokenEstimateQualityView::Estimated,
    }
}

pub fn map_session_store_error(error: SessionStoreError) -> AgentApiError {
    match error {
        SessionStoreError::SessionAlreadyExists { session_id } => {
            AgentApiError::conflict(format!("session already exists: {session_id}"))
        }
        SessionStoreError::SessionNotFound { session_id } => {
            AgentApiError::not_found(format!("session not found: {session_id}"))
        }
        SessionStoreError::InvalidLimit { limit } => {
            AgentApiError::invalid_request(format!("invalid page limit: {limit}"))
        }
        SessionStoreError::ExpectedHeadMismatch { .. } => {
            AgentApiError::conflict(error.to_string())
        }
        SessionStoreError::Store { message } => AgentApiError::internal(message),
    }
}

pub fn map_blob_store_error(error: BlobStoreError) -> AgentApiError {
    match error {
        BlobStoreError::NotFound { blob_ref } => AgentApiError::internal(format!(
            "blob not found while projecting API view: {blob_ref}"
        )),
        BlobStoreError::Store { message } => AgentApiError::internal(message),
    }
}

fn aggregate_api_tool_status(calls: &[ToolCallView]) -> ToolItemStatus {
    if calls.is_empty() {
        return ToolItemStatus::Unavailable;
    }
    if calls.iter().any(|call| {
        matches!(
            call.status,
            ToolItemStatus::Failed | ToolItemStatus::Unavailable
        )
    }) {
        return ToolItemStatus::Failed;
    }
    if calls.iter().any(|call| {
        matches!(
            call.status,
            ToolItemStatus::Requested | ToolItemStatus::Running
        )
    }) {
        return ToolItemStatus::Running;
    }
    if calls
        .iter()
        .all(|call| matches!(call.status, ToolItemStatus::Succeeded))
    {
        return ToolItemStatus::Succeeded;
    }
    ToolItemStatus::Unavailable
}

fn llm_generation_status_to_api(status: &LlmGenerationStatus) -> &'static str {
    match status {
        LlmGenerationStatus::Succeeded => "succeeded",
        LlmGenerationStatus::Failed => "failed",
        LlmGenerationStatus::Cancelled => "cancelled",
    }
}

fn tool_execution_target_to_api(target: &ToolExecutionTarget) -> ToolExecutionTargetView {
    ToolExecutionTargetView {
        namespace: target.namespace.clone(),
        id: target.id.clone(),
    }
}

fn tool_effects_for_run(
    projection: &CoreAgentProjection<'_>,
    run_id: RunId,
) -> BTreeMap<String, Vec<ToolEffectView>> {
    let mut effects = BTreeMap::new();
    for entry in projection.entries() {
        let CoreAgentEventKind::Tool(ToolEvent::CallCompleted {
            run_id: event_run_id,
            result,
            ..
        }) = &entry.event.kind
        else {
            continue;
        };
        if *event_run_id == run_id && !result.effects.is_empty() {
            effects.insert(
                result.call_id.as_str().to_owned(),
                tool_effects_to_api(&result.effects),
            );
        }
    }
    effects
}

fn tool_effects_to_api(effects: &[engine::ToolEffect]) -> Vec<ToolEffectView> {
    effects
        .iter()
        .map(|effect| ToolEffectView {
            kind: effect.kind.clone(),
            data: effect.data.clone(),
        })
        .collect()
}

fn tool_call_display(tool_name: &str, arguments: &str) -> Option<ToolCallDisplayView> {
    let json = serde_json::from_str::<Value>(arguments).ok();
    let normalized = tool_name.to_ascii_lowercase();
    let view = match normalized.as_str() {
        "read_file" | "read" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Explore,
            verb: "Read".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["path", "file_path"])),
            detail: None,
        },
        "list_dir" | "ls" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Explore,
            verb: "List".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["path"]))
                .or_else(|| Some("/".to_owned())),
            detail: None,
        },
        "grep" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Explore,
            verb: "Search".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["pattern"])),
            detail: json
                .as_ref()
                .and_then(|json| first_string(json, &["path", "include"]))
                .map(|target| format!("in {target}")),
        },
        "glob" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Explore,
            verb: "Find".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["pattern"])),
            detail: json
                .as_ref()
                .and_then(|json| first_string(json, &["path"]))
                .map(|target| format!("in {target}")),
        },
        "web_fetch" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Explore,
            verb: "Fetch".to_owned(),
            target: json.as_ref().and_then(|json| first_string(json, &["url"])),
            detail: None,
        },
        "write_file" | "write" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Edit,
            verb: "Write".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["path", "file_path"])),
            detail: None,
        },
        "edit_file" | "edit" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Edit,
            verb: "Edit".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["path", "file_path"])),
            detail: None,
        },
        "apply_patch" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Edit,
            verb: "Patch".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["patch"]))
                .and_then(|patch| patch_target(&patch)),
            detail: None,
        },
        "exec_command" | "bash" | "run_process" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Execute,
            verb: "Run".to_owned(),
            target: json.as_ref().and_then(command_display),
            detail: json
                .as_ref()
                .and_then(|json| first_string(json, &["cwd"]))
                .map(|cwd| format!("in {cwd}")),
        },
        "write_stdin" | "write_process_stdin" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Execute,
            verb: "Send input".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["process_id", "handle", "id"])),
            detail: None,
        },
        _ => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Other,
            verb: tool_name.to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["path", "file", "command", "cmd"])),
            detail: None,
        },
    };
    Some(view)
}

fn first_string(json: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| json_text(json.get(*key)?))
}

fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn command_display(json: &Value) -> Option<String> {
    if let Some(command) = first_string(json, &["command", "cmd"]) {
        return Some(command);
    }
    let argv = json.get("argv")?.as_array()?;
    let parts = argv.iter().filter_map(json_text).collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn patch_target(patch: &str) -> Option<String> {
    patch.lines().find_map(|line| {
        line.strip_prefix("*** Add File: ")
            .or_else(|| line.strip_prefix("*** Update File: "))
            .or_else(|| line.strip_prefix("*** Delete File: "))
            .or_else(|| line.strip_prefix("*** Move to: "))
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use engine::{
        BlobRef, ContextEntryId, CoreAgentJoins, EventSeq, SessionPosition, TokenEstimate,
        TokenEstimateQuality, storage::InMemoryBlobStore,
    };

    use super::*;

    #[test]
    fn context_entries_for_run_reads_committed_entry_events() {
        let first = context_entry(
            1,
            ContextEntrySource::RunInput {
                run_id: RunId::new(1),
                input_index: 0,
            },
        );
        let second = context_entry(
            2,
            ContextEntrySource::RunInput {
                run_id: RunId::new(2),
                input_index: 0,
            },
        );
        let entries = vec![entry(1, vec![first]), entry(2, vec![second])];

        let projected = CoreAgentProjection::new(&entries).context_entries_for_run(RunId::new(1));

        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].entry_id, ContextEntryId::new(1));
    }

    #[test]
    fn context_entry_input_projection_is_ref_backed() {
        let blob_ref = BlobRef::from_bytes(b"hello");
        let projected = project_context_entry_inputs(&[ContextEntryInput {
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            content_ref: blob_ref.clone(),
            media_type: Some("text/plain".to_owned()),
            preview: Some("hello".to_owned()),
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }]);

        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].content_ref, blob_ref.as_str());
        assert_eq!(projected[0].media_type.as_deref(), Some("text/plain"));
        assert!(matches!(
            projected[0].kind,
            ContextEntryKindView::Message {
                role: ContextMessageRoleView::User
            }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_compaction_context_events_project_reason() {
        let blobs = InMemoryBlobStore::new();
        let projector = CoreAgentProjector::new(&blobs);

        let removed = projector
            .project_event_kind(&CoreAgentEventKind::Context(ContextEvent::EntriesRemoved {
                base_revision: 7,
                entry_ids: vec![ContextEntryId::new(11), ContextEntryId::new(12)],
                reason: ContextRemovalReason::ProviderCompacted,
            }))
            .await
            .expect("project provider-compacted removal");
        assert_eq!(
            removed,
            SessionEventKindView::ContextEntriesRemoved {
                base_revision: 7,
                revision: 8,
                item_ids: vec!["item_11".to_owned(), "item_12".to_owned()],
                reason: "providerCompacted".to_owned(),
            }
        );

        let replaced = projector
            .project_event_kind(&CoreAgentEventKind::Context(ContextEvent::StateReplaced {
                base_revision: 8,
                entries: Vec::new(),
                reason: ContextRewriteReason::ProviderCompacted,
            }))
            .await
            .expect("project provider-compacted rewrite");
        assert_eq!(
            replaced,
            SessionEventKindView::ContextStateReplaced {
                base_revision: 8,
                revision: 9,
                items: Vec::new(),
                reason: "providerCompacted".to_owned(),
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_context_item_exposes_debug_metadata() {
        let blobs = InMemoryBlobStore::new();
        let projector = CoreAgentProjector::new(&blobs);
        let item = ContextEntry {
            entry_id: ContextEntryId::new(42),
            key: None,
            kind: ContextEntryKind::ProviderOpaque,
            source: ContextEntrySource::AssistantOutput {
                run_id: RunId::new(7),
                turn_id: TurnId::new(8),
            },
            content_ref: BlobRef::from_bytes(br#"{"type":"compaction"}"#),
            media_type: Some("application/json".to_owned()),
            preview: Some("OpenAI Responses compaction item".to_owned()),
            provider_kind: Some("openai.responses.compaction".to_owned()),
            provider_item_id: Some("item_compaction_1".to_owned()),
            token_estimate: Some(TokenEstimate {
                tokens: 123,
                quality: TokenEstimateQuality::ProviderCounted,
            }),
        };

        let projected = projector
            .project_item(&item)
            .await
            .expect("project provider context item");

        assert_eq!(
            projected,
            SessionItemView::ProviderContext {
                id: "item_42".to_owned(),
                content_ref: BlobRef::from_bytes(br#"{"type":"compaction"}"#)
                    .as_str()
                    .to_owned(),
                media_type: Some("application/json".to_owned()),
                preview: Some("OpenAI Responses compaction item".to_owned()),
                provider_kind: Some("openai.responses.compaction".to_owned()),
                provider_item_id: Some("item_compaction_1".to_owned()),
                token_estimate: Some(TokenEstimateView {
                    tokens: 123,
                    quality: TokenEstimateQualityView::ProviderCounted,
                }),
            }
        );
    }

    #[test]
    fn input_text_joins_non_empty_text_items() {
        let text = input_text(&[
            InputItem::Text {
                text: " first ".to_owned(),
            },
            InputItem::Text {
                text: "".to_owned(),
            },
            InputItem::Text {
                text: "second".to_owned(),
            },
        ])
        .expect("valid input");

        assert_eq!(text, "first\n\nsecond");
    }

    #[test]
    fn input_text_rejects_unresolved_text_refs() {
        let error = input_text(&[InputItem::TextRef {
            blob_ref: BlobRef::from_bytes(b"hello").as_str().to_owned(),
        }])
        .expect_err("text refs require store resolution");

        assert_eq!(error.kind, api::AgentApiErrorKind::InvalidRequest);
    }

    fn entry(seq: u64, entries: Vec<ContextEntry>) -> CoreAgentEntry {
        CoreAgentEntry {
            position: SessionPosition {
                seq: EventSeq::new(seq),
            },
            observed_at_ms: seq,
            joins: CoreAgentJoins::default(),
            event: engine::CoreAgentEvent {
                kind: CoreAgentEventKind::Context(ContextEvent::EntriesApplied {
                    base_revision: seq - 1,
                    entries,
                }),
            },
        }
    }

    fn context_entry(id: u64, source: ContextEntrySource) -> ContextEntry {
        ContextEntry {
            entry_id: ContextEntryId::new(id),
            key: None,
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            source,
            content_ref: BlobRef::default(),
            media_type: None,
            preview: None,
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }
    }
}

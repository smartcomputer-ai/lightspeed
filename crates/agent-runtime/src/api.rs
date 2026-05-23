//! Local implementation of the client-facing Forge agent API.
//!
//! The API surface in `agent-api` is deliberately client-oriented
//! (`session`, `run`, `item`). This module is the translation layer from that
//! surface to the event-sourced `agent-core` runner.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use agent_api::{
    AgentApiError, AgentApiErrorKind, AgentApiOutcome, AgentApiService, AgentNotification,
    ClientCapabilities, EventCursor, EventJoinsView, InitializeParams, InitializeResponse,
    InputItem, ModelConfig, RunStartParams, RunStartResponse, RunStatus as ApiRunStatus,
    RunView as ApiRunView, ServerCapabilities, ServerInfo, SessionEventKindView, SessionEventView,
    SessionEventsReadParams, SessionEventsReadResponse, SessionItemView, SessionReadParams,
    SessionReadResponse, SessionStartParams, SessionStartResponse,
    SessionStatus as ApiSessionStatus, SessionView as ApiSessionView, ToolBatchView,
    ToolCallDisplayGroup, ToolCallDisplayView, ToolCallEventView, ToolCallView,
    ToolExecutionTargetView, ToolItemStatus,
};
use agent_core::{
    AgentHandle, BlobRef, ContextEvent, ContextItem, ContextItemKind, ContextMessageRole,
    CoreAgentCodec, CoreAgentCommand, CoreAgentEntry, CoreAgentLifecycleEvent, CoreAgentLlm,
    CoreAgentState, CoreAgentStatus, CoreAgentTools, DriveCommand, DriveSession, EventSeq,
    LlmGenerationStatus, ModelSelection, ProviderApiKind, RunEvent, RunId, RunStatus, RunnerError,
    RunnerQuiescence, RunnerStores, SessionConfig, SessionId, SessionRunner, ToolCallStatus,
    ToolConfigEvent, ToolEvent, ToolExecutionTarget, ToolProfileId, ToolRegistry, TurnEvent,
    storage::{BlobStoreError, BlobWrite, CreateSession, ReadSessionEvents, SessionStoreError},
};
use async_trait::async_trait;
use serde_json::Value;

const DEFAULT_AGENT_HANDLE: &str = "forge.local";
const DEFAULT_EVENT_PAGE_LIMIT: u32 = 128;
const MAX_EVENT_PAGE_LIMIT: u32 = 512;
const MAX_LOCAL_DRIVE_SLICES: usize = 10_000;

pub trait AgentApiClock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemAgentApiClock;

impl AgentApiClock for SystemAgentApiClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
            .unwrap_or(0)
    }
}

pub struct LocalAgentApiBuilder {
    stores: RunnerStores,
    llm: Arc<dyn CoreAgentLlm>,
    tools: Option<Arc<dyn CoreAgentTools>>,
    default_config: SessionConfig,
    agent_handle: AgentHandle,
    tool_registry: Option<ToolRegistry>,
    tool_profile_id: Option<ToolProfileId>,
    default_tool_targets: BTreeMap<String, ToolExecutionTarget>,
    clock: Arc<dyn AgentApiClock>,
}

impl LocalAgentApiBuilder {
    pub fn with_agent_handle(mut self, agent_handle: AgentHandle) -> Self {
        self.agent_handle = agent_handle;
        self
    }

    pub fn with_tool_registry(mut self, registry: ToolRegistry) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    pub fn with_tool_profile_id(mut self, profile_id: ToolProfileId) -> Self {
        self.tool_profile_id = Some(profile_id);
        self
    }

    pub fn with_default_tool_target(mut self, target: ToolExecutionTarget) -> Self {
        self.default_tool_targets
            .insert(target.namespace.clone(), target);
        self
    }

    pub fn without_default_tool_target(mut self, namespace: impl Into<String>) -> Self {
        self.default_tool_targets.remove(&namespace.into());
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn AgentApiClock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_tools(mut self, tools: Arc<dyn CoreAgentTools>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn build(self) -> LocalAgentApi {
        let mut runner = SessionRunner::new(self.stores.clone(), self.llm);
        if let Some(tools) = self.tools {
            runner = runner.with_tools(tools);
        }
        LocalAgentApi {
            stores: self.stores,
            runner,
            default_config: self.default_config,
            agent_handle: self.agent_handle,
            tool_registry: self.tool_registry,
            tool_profile_id: self.tool_profile_id,
            default_tool_targets: self.default_tool_targets,
            clock: self.clock,
            next_session_id: AtomicU64::new(1),
            next_submission_id: AtomicU64::new(1),
            metadata: RwLock::new(BTreeMap::new()),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct LocalSessionMetadata {
    cwd: Option<String>,
}

#[derive(Clone, Debug)]
struct ProjectedToolResult {
    output: Option<String>,
    is_error: bool,
    status: ToolItemStatus,
}

pub struct LocalAgentApi {
    stores: RunnerStores,
    runner: SessionRunner,
    default_config: SessionConfig,
    agent_handle: AgentHandle,
    tool_registry: Option<ToolRegistry>,
    tool_profile_id: Option<ToolProfileId>,
    default_tool_targets: BTreeMap<String, ToolExecutionTarget>,
    clock: Arc<dyn AgentApiClock>,
    next_session_id: AtomicU64,
    next_submission_id: AtomicU64,
    metadata: RwLock<BTreeMap<SessionId, LocalSessionMetadata>>,
}

impl LocalAgentApi {
    pub fn builder(
        stores: RunnerStores,
        llm: Arc<dyn CoreAgentLlm>,
        default_config: SessionConfig,
    ) -> LocalAgentApiBuilder {
        LocalAgentApiBuilder {
            stores,
            llm,
            tools: None,
            default_config,
            agent_handle: AgentHandle::new(DEFAULT_AGENT_HANDLE),
            tool_registry: None,
            tool_profile_id: None,
            default_tool_targets: BTreeMap::new(),
            clock: Arc::new(SystemAgentApiClock),
        }
    }

    pub fn new(
        stores: RunnerStores,
        llm: Arc<dyn CoreAgentLlm>,
        default_config: SessionConfig,
    ) -> Self {
        Self::builder(stores, llm, default_config).build()
    }

    pub async fn open_or_start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        match self.start_session(params.clone()).await {
            Ok(outcome) => Ok(outcome),
            Err(error)
                if matches!(error.kind, AgentApiErrorKind::Conflict)
                    && params.session_id.is_some() =>
            {
                let session_id =
                    SessionId::try_new(params.session_id.expect("checked session id present"))
                        .map_err(|error| {
                            AgentApiError::invalid_request(format!("invalid session id: {error}"))
                        })?;
                self.write_session_metadata(
                    session_id.clone(),
                    LocalSessionMetadata { cwd: params.cwd },
                )?;
                let state = self
                    .runner
                    .load_state(&session_id)
                    .await
                    .map_err(map_runner_error)?;
                let session = self.project_session(&session_id, &state).await?;
                Ok(AgentApiOutcome::new(SessionStartResponse { session }))
            }
            Err(error) => Err(error),
        }
    }

    fn allocate_session_id(&self) -> SessionId {
        let next = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        SessionId::new(format!("session_{next}"))
    }

    fn allocate_submission_id(&self) -> agent_core::SubmissionId {
        let next = self.next_submission_id.fetch_add(1, Ordering::Relaxed);
        agent_core::SubmissionId::new(format!("submit_{next}"))
    }

    fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    fn session_metadata(
        &self,
        session_id: &SessionId,
    ) -> Result<LocalSessionMetadata, AgentApiError> {
        let metadata = self
            .metadata
            .read()
            .map_err(|_| AgentApiError::internal("local API metadata lock poisoned"))?;
        Ok(metadata.get(session_id).cloned().unwrap_or_default())
    }

    fn write_session_metadata(
        &self,
        session_id: SessionId,
        metadata: LocalSessionMetadata,
    ) -> Result<(), AgentApiError> {
        self.metadata
            .write()
            .map_err(|_| AgentApiError::internal("local API metadata lock poisoned"))?
            .insert(session_id, metadata);
        Ok(())
    }

    async fn drive(
        &self,
        session_id: SessionId,
        command: CoreAgentCommand,
        max_steps: Option<u32>,
    ) -> Result<agent_core::DriveOutcome, AgentApiError> {
        let observed_at_ms = self.now_ms();
        let mut outcome = self
            .runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms,
                command,
                max_steps,
            })
            .await
            .map_err(map_runner_error)?;

        if let Some(rejection) = outcome.rejection.as_ref() {
            return Err(AgentApiError::rejected(rejection.to_string()));
        }

        let mut slices = 0usize;
        while matches!(outcome.quiescence, RunnerQuiescence::IterationLimitReached) {
            if slices >= MAX_LOCAL_DRIVE_SLICES {
                return Err(AgentApiError::internal(
                    "local runner did not quiesce after repeated drive slices",
                ));
            }
            slices = slices.saturating_add(1);
            let next = self
                .runner
                .drive_until_quiescent(DriveSession {
                    session_id: session_id.clone(),
                    observed_at_ms,
                    max_steps,
                })
                .await
                .map_err(map_runner_error)?;
            let made_progress = !next.emitted_entries.is_empty();
            outcome.emitted_entries.extend(next.emitted_entries);
            outcome.head = next.head;
            outcome.state = next.state;
            outcome.quiescence = next.quiescence;
            if !made_progress
                && matches!(outcome.quiescence, RunnerQuiescence::IterationLimitReached)
            {
                return Err(AgentApiError::internal(
                    "local runner reached the drive step limit without making progress",
                ));
            }
        }
        Ok(outcome)
    }

    async fn project_session(
        &self,
        session_id: &SessionId,
        state: &CoreAgentState,
    ) -> Result<ApiSessionView, AgentApiError> {
        let record = self
            .stores
            .sessions
            .load_session(session_id)
            .await
            .map_err(map_session_store_error)?
            .ok_or_else(|| AgentApiError::not_found(format!("session not found: {session_id}")))?;
        let metadata = self.session_metadata(session_id)?;
        let mut runs = Vec::new();

        for record in &state.runs.completed {
            runs.push(
                self.project_run_for_core_run(session_id, state, record.run_id, record.status)
                    .await?,
            );
        }
        if let Some(active_run) = state.runs.active.as_ref() {
            runs.push(
                self.project_run_for_core_run(
                    session_id,
                    state,
                    active_run.run_id,
                    active_run.status,
                )
                .await?,
            );
        }

        Ok(ApiSessionView {
            id: session_id.as_str().to_owned(),
            status: session_status(state),
            cwd: metadata.cwd,
            model: state
                .lifecycle
                .config
                .as_ref()
                .map(|config| model_to_api(&config.model)),
            created_at_ms: record.created_at_ms,
            updated_at_ms: record.updated_at_ms,
            runs,
        })
    }

    async fn project_run_for_core_run(
        &self,
        session_id: &SessionId,
        _state: &CoreAgentState,
        run_id: RunId,
        status: RunStatus,
    ) -> Result<ApiRunView, AgentApiError> {
        let entries = self.read_all_session_entries(session_id).await?;
        let projection = crate::projection::CoreAgentProjection::new(&entries);
        let context_items = projection.context_items_for_run(run_id);
        let mut input = Vec::new();
        let mut items = Vec::new();

        for item in &context_items {
            let projected = self.project_item(item).await?;
            if let SessionItemView::UserMessage { text, .. } = &projected {
                input.push(InputItem::Text { text: text.clone() });
            }
            items.push(projected);
        }

        Ok(ApiRunView {
            id: api_run_id(run_id),
            status: core_run_status_to_api_status(status),
            input,
            items,
            tool_batches: self
                .project_tool_batches_for_run(&projection, &context_items, run_id)
                .await?,
        })
    }

    async fn project_item(&self, item: &ContextItem) -> Result<SessionItemView, AgentApiError> {
        let id = format!("item_{}", item.item_id.as_u64());
        match &item.kind {
            ContextItemKind::Message { role } => {
                let text = self.read_blob_text(&item.native_item_ref).await?;
                match role {
                    ContextMessageRole::User => Ok(SessionItemView::UserMessage { id, text }),
                    ContextMessageRole::Assistant => {
                        Ok(SessionItemView::AssistantMessage { id, text })
                    }
                }
            }
            ContextItemKind::ToolCall { call_id, name } => Ok(SessionItemView::ToolCall {
                id,
                call_id: call_id.as_str().to_owned(),
                tool_name: name.as_str().to_owned(),
                arguments: Some(self.read_blob_text(&item.native_item_ref).await?),
                status: ToolItemStatus::Requested,
            }),
            ContextItemKind::ToolResult { call_id, is_error } => Ok(SessionItemView::ToolResult {
                id,
                call_id: call_id.as_str().to_owned(),
                output: Some(self.read_blob_text(&item.native_item_ref).await?),
                is_error: *is_error,
                status: if *is_error {
                    ToolItemStatus::Failed
                } else {
                    ToolItemStatus::Succeeded
                },
            }),
            ContextItemKind::ReasoningState
            | ContextItemKind::CompactionState
            | ContextItemKind::ProviderOpaque => Ok(SessionItemView::SystemEvent {
                id,
                text: item
                    .preview
                    .clone()
                    .unwrap_or_else(|| "context item".to_owned()),
            }),
        }
    }

    async fn project_tool_batches_for_run(
        &self,
        projection: &crate::projection::CoreAgentProjection<'_>,
        context_items: &[&ContextItem],
        run_id: RunId,
    ) -> Result<Vec<ToolBatchView>, AgentApiError> {
        let result_by_call = self.project_tool_results_for_run(context_items).await?;
        let mut batches = Vec::new();
        let mut completed_batches = BTreeMap::new();

        for entry in projection.entries() {
            let agent_core::CoreAgentEventKind::Tool(event) = &entry.event.kind else {
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
        context_items: &[&ContextItem],
    ) -> Result<BTreeMap<String, ProjectedToolResult>, AgentApiError> {
        let mut result_by_call = BTreeMap::new();
        for item in context_items {
            let ContextItemKind::ToolResult { call_id, is_error } = &item.kind else {
                continue;
            };
            result_by_call.insert(
                call_id.as_str().to_owned(),
                ProjectedToolResult {
                    output: Some(self.read_blob_text(&item.native_item_ref).await?),
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

    async fn read_all_session_entries(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<CoreAgentEntry>, AgentApiError> {
        let mut after = None;
        let mut entries = Vec::new();
        let codec = CoreAgentCodec;
        loop {
            let page = self
                .stores
                .sessions
                .read_after(ReadSessionEvents {
                    session_id: session_id.clone(),
                    after,
                    limit: MAX_EVENT_PAGE_LIMIT as usize,
                })
                .await
                .map_err(map_session_store_error)?;
            after = page.next_after;
            for entry in &page.entries {
                entries.push(
                    codec
                        .decode_entry(entry)
                        .map_err(|error| AgentApiError::internal(error.to_string()))?,
                );
            }
            if page.complete {
                return Ok(entries);
            }
        }
    }

    async fn project_entry(
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

    async fn project_event_kind(
        &self,
        kind: &agent_core::CoreAgentEventKind,
    ) -> Result<SessionEventKindView, AgentApiError> {
        match kind {
            agent_core::CoreAgentEventKind::Lifecycle(event) => match event {
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
            agent_core::CoreAgentEventKind::Run(event) => match event {
                RunEvent::Queued {
                    submission_id,
                    input_ref,
                    ..
                } => Ok(SessionEventKindView::RunQueued {
                    submission_id: submission_id.as_ref().map(|id| id.as_str().to_owned()),
                    input_ref: input_ref.as_str().to_owned(),
                }),
                RunEvent::Started {
                    run_id,
                    submission_id,
                    input_ref,
                    ..
                } => Ok(SessionEventKindView::RunStarted {
                    run_id: api_run_id(*run_id),
                    submission_id: submission_id.as_ref().map(|id| id.as_str().to_owned()),
                    input_ref: input_ref.as_str().to_owned(),
                }),
                RunEvent::SteeringAdded { run_id, input_ref } => {
                    Ok(SessionEventKindView::RunSteeringAdded {
                        run_id: api_run_id(*run_id),
                        input_ref: input_ref.as_str().to_owned(),
                    })
                }
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
                    message: format!("{:?}", failure.kind),
                }),
                RunEvent::Cancelled { run_id } => Ok(SessionEventKindView::RunCancelled {
                    run_id: api_run_id(*run_id),
                }),
            },
            agent_core::CoreAgentEventKind::Turn(event) => match event {
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
            agent_core::CoreAgentEventKind::Context(event) => match event {
                ContextEvent::ItemsRecorded { items } => {
                    let mut projected = Vec::with_capacity(items.len());
                    for item in items {
                        projected.push(self.project_item(item).await?);
                    }
                    Ok(SessionEventKindView::ItemsRecorded { items: projected })
                }
                ContextEvent::WindowPlanned {
                    run_id, turn_id, ..
                } => Ok(SessionEventKindView::ContextWindowPlanned {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                }),
                ContextEvent::CompactionRecorded {
                    run_id,
                    turn_id,
                    record,
                } => Ok(SessionEventKindView::CompactionRecorded {
                    run_id: Some(api_run_id(*run_id)),
                    turn_id: turn_id.map(api_turn_id),
                    summary_ref: record
                        .summary_ref
                        .as_ref()
                        .map(|ref_| ref_.as_str().to_owned()),
                }),
            },
            agent_core::CoreAgentEventKind::ToolConfig(event) => match event {
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
                        target: Some(ToolExecutionTargetView {
                            namespace: target.namespace.clone(),
                            id: target.id.clone(),
                        }),
                    })
                }
                ToolConfigEvent::DefaultTargetCleared { namespace } => {
                    Ok(SessionEventKindView::ToolDefaultTargetChanged {
                        namespace: namespace.clone(),
                        target: None,
                    })
                }
            },
            agent_core::CoreAgentEventKind::Tool(event) => match event {
                ToolEvent::BatchStarted {
                    run_id,
                    turn_id,
                    batch_id,
                    calls,
                } => Ok(SessionEventKindView::ToolBatchStarted {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                    batch_id: api_tool_batch_id(*batch_id),
                    calls: {
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
                        projected
                    },
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

    async fn read_blob_text(&self, blob_ref: &BlobRef) -> Result<String, AgentApiError> {
        self.stores
            .blobs
            .read_text(blob_ref)
            .await
            .map_err(map_blob_store_error)
    }

    async fn notifications_for_entries(
        &self,
        session_id: &SessionId,
        state: &CoreAgentState,
        entries: &[CoreAgentEntry],
    ) -> Result<Vec<AgentNotification>, AgentApiError> {
        let mut notifications = Vec::new();
        for entry in entries {
            match &entry.event.kind {
                agent_core::CoreAgentEventKind::Run(RunEvent::Started { run_id, .. }) => {
                    notifications.push(AgentNotification::RunStarted {
                        session_id: session_id.as_str().to_owned(),
                        run: self
                            .project_run_for_core_run(session_id, state, *run_id, RunStatus::Active)
                            .await?,
                    });
                }
                agent_core::CoreAgentEventKind::Run(RunEvent::Completed { run_id, .. }) => {
                    notifications.push(AgentNotification::RunCompleted {
                        session_id: session_id.as_str().to_owned(),
                        run: self
                            .project_run_for_core_run(
                                session_id,
                                state,
                                *run_id,
                                RunStatus::Completed,
                            )
                            .await?,
                    });
                }
                agent_core::CoreAgentEventKind::Run(RunEvent::Failed { run_id, .. }) => {
                    notifications.push(AgentNotification::RunCompleted {
                        session_id: session_id.as_str().to_owned(),
                        run: self
                            .project_run_for_core_run(session_id, state, *run_id, RunStatus::Failed)
                            .await?,
                    });
                }
                agent_core::CoreAgentEventKind::Run(RunEvent::Cancelled { run_id }) => {
                    notifications.push(AgentNotification::RunCompleted {
                        session_id: session_id.as_str().to_owned(),
                        run: self
                            .project_run_for_core_run(
                                session_id,
                                state,
                                *run_id,
                                RunStatus::Cancelled,
                            )
                            .await?,
                    });
                }
                agent_core::CoreAgentEventKind::Context(
                    agent_core::ContextEvent::ItemsRecorded { items },
                ) => {
                    for item in items {
                        if let Some(run_id) = crate::projection::context_item_run_id(item) {
                            notifications.push(AgentNotification::ItemCompleted {
                                session_id: session_id.as_str().to_owned(),
                                run_id: api_run_id(run_id),
                                item: self.project_item(item).await?,
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        notifications.push(AgentNotification::SessionStatusChanged {
            session_id: session_id.as_str().to_owned(),
            status: session_status(state),
        });
        Ok(notifications)
    }
}

#[async_trait]
impl AgentApiService for LocalAgentApi {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> Result<AgentApiOutcome<InitializeResponse>, AgentApiError> {
        let _capabilities = params.capabilities.unwrap_or(ClientCapabilities {
            experimental_api: false,
        });
        Ok(AgentApiOutcome::new(InitializeResponse {
            protocol_version: agent_api::PROTOCOL_VERSION.to_owned(),
            server_info: ServerInfo {
                name: "agent-runtime".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            capabilities: ServerCapabilities {
                notifications: true,
                history_read: true,
                event_log: true,
                local_execution: true,
            },
        }))
    }

    async fn start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        let session_id = match params.session_id {
            Some(session_id) => SessionId::try_new(session_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid session id: {error}"))
            })?,
            None => self.allocate_session_id(),
        };
        let config = session_config_for_session(&self.default_config, params.model)?;
        let now_ms = self.now_ms();

        self.stores
            .sessions
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: self.agent_handle.clone(),
                created_at_ms: now_ms,
            })
            .await
            .map_err(map_session_store_error)?;

        let mut last = self
            .drive(
                session_id.clone(),
                CoreAgentCommand::OpenSession { config },
                None,
            )
            .await?;
        if let Some(registry) = self.tool_registry.clone() {
            self.drive(
                session_id.clone(),
                CoreAgentCommand::SetToolRegistry { registry },
                None,
            )
            .await?;
        }
        for target in self.default_tool_targets.values().cloned() {
            last = self
                .drive(
                    session_id.clone(),
                    CoreAgentCommand::SetDefaultToolTarget { target },
                    None,
                )
                .await?;
        }
        if let Some(profile_id) = self.tool_profile_id.clone() {
            last = self
                .drive(
                    session_id.clone(),
                    CoreAgentCommand::SelectToolProfile { profile_id },
                    None,
                )
                .await?;
        }

        self.write_session_metadata(session_id.clone(), LocalSessionMetadata { cwd: params.cwd })?;
        let session = self.project_session(&session_id, &last.state).await?;
        Ok(AgentApiOutcome::with_notifications(
            SessionStartResponse {
                session: session.clone(),
            },
            vec![AgentNotification::SessionStarted { session }],
        ))
    }

    async fn read_session(
        &self,
        params: SessionReadParams,
    ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let state = self
            .runner
            .load_state(&session_id)
            .await
            .map_err(map_runner_error)?;
        let session = self.project_session(&session_id, &state).await?;
        Ok(AgentApiOutcome::new(SessionReadResponse { session }))
    }

    async fn read_session_events(
        &self,
        params: SessionEventsReadParams,
    ) -> Result<AgentApiOutcome<SessionEventsReadResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        self.stores
            .sessions
            .load_session(&session_id)
            .await
            .map_err(map_session_store_error)?
            .ok_or_else(|| AgentApiError::not_found(format!("session not found: {session_id}")))?;
        let limit = event_page_limit(params.limit)?;
        let page = self
            .stores
            .sessions
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
                after: params.after.map(|cursor| EventSeq::new(cursor.seq)),
                limit,
            })
            .await
            .map_err(map_session_store_error)?;
        let head_cursor = self
            .stores
            .sessions
            .head(&session_id)
            .await
            .map_err(map_session_store_error)?
            .map(|position| event_cursor(position.seq));
        let mut events = Vec::with_capacity(page.entries.len());
        let codec = CoreAgentCodec;
        for entry in &page.entries {
            let entry = codec
                .decode_entry(entry)
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
            events.push(self.project_entry(&session_id, &entry).await?);
        }

        Ok(AgentApiOutcome::new(SessionEventsReadResponse {
            events,
            next_cursor: page.next_after.map(event_cursor),
            head_cursor,
            complete: page.complete,
            gap: None,
        }))
    }

    async fn start_run(
        &self,
        params: RunStartParams,
    ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError> {
        let session_id = SessionId::try_new(params.session_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid session id: {error}"))
        })?;
        let input_text = input_text(&params.input)?;
        let before = self
            .runner
            .load_state(&session_id)
            .await
            .map_err(map_runner_error)?;
        let config = before.lifecycle.config.as_ref().ok_or_else(|| {
            AgentApiError::invalid_request(format!("session is not open: {session_id}"))
        })?;
        let input_ref = self
            .stores
            .blobs
            .put_bytes(BlobWrite {
                bytes: input_text.into_bytes(),
                child_refs: Vec::new(),
            })
            .await
            .map_err(map_blob_store_error)?;

        let outcome = self
            .drive(
                session_id.clone(),
                CoreAgentCommand::RequestRun {
                    submission_id: Some(self.allocate_submission_id()),
                    input_ref,
                    run_config: config.run.clone(),
                },
                None,
            )
            .await?;

        let run_id = started_run_id(&outcome.emitted_entries)
            .or_else(|| outcome.state.runs.completed.last().map(|run| run.run_id))
            .or_else(|| outcome.state.runs.active.as_ref().map(|run| run.run_id))
            .ok_or_else(|| AgentApiError::internal("request did not start a run"))?;
        let status = outcome
            .state
            .runs
            .completed
            .iter()
            .find(|run| run.run_id == run_id)
            .map(|run| run.status)
            .or_else(|| outcome.state.runs.active.as_ref().map(|run| run.status))
            .unwrap_or(RunStatus::Active);
        let run = self
            .project_run_for_core_run(&session_id, &outcome.state, run_id, status)
            .await?;
        let notifications = self
            .notifications_for_entries(&session_id, &outcome.state, &outcome.emitted_entries)
            .await?;

        Ok(AgentApiOutcome::with_notifications(
            RunStartResponse { run },
            notifications,
        ))
    }
}

fn input_text(input: &[InputItem]) -> Result<String, AgentApiError> {
    let parts = input
        .iter()
        .map(|item| match item {
            InputItem::Text { text } => text.trim(),
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return Err(AgentApiError::invalid_request(
            "run/start input must contain at least one non-empty text item",
        ));
    }
    Ok(parts.join("\n\n"))
}

fn event_page_limit(limit: Option<u32>) -> Result<usize, AgentApiError> {
    let limit = limit.unwrap_or(DEFAULT_EVENT_PAGE_LIMIT);
    if limit == 0 || limit > MAX_EVENT_PAGE_LIMIT {
        return Err(AgentApiError::invalid_request(format!(
            "session/events/read limit must be between 1 and {MAX_EVENT_PAGE_LIMIT}"
        )));
    }
    usize::try_from(limit)
        .map_err(|_| AgentApiError::invalid_request("session/events/read limit is too large"))
}

fn event_cursor(seq: EventSeq) -> EventCursor {
    EventCursor { seq: seq.as_u64() }
}

fn started_run_id(entries: &[CoreAgentEntry]) -> Option<RunId> {
    entries.iter().find_map(|entry| match &entry.event.kind {
        agent_core::CoreAgentEventKind::Run(RunEvent::Started { run_id, .. }) => Some(*run_id),
        _ => None,
    })
}

fn api_run_id(run_id: RunId) -> String {
    format!("run_{}", run_id.as_u64())
}

fn api_turn_id(turn_id: agent_core::TurnId) -> String {
    format!("turn_{}", turn_id.as_u64())
}

fn api_tool_batch_id(batch_id: agent_core::ToolBatchId) -> String {
    format!("tool_batch_{}", batch_id.as_u64())
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

fn core_tool_status_to_api_status(status: ToolCallStatus) -> ToolItemStatus {
    match status {
        ToolCallStatus::Observed | ToolCallStatus::Accepted => ToolItemStatus::Requested,
        ToolCallStatus::Pending => ToolItemStatus::Running,
        ToolCallStatus::Succeeded => ToolItemStatus::Succeeded,
        ToolCallStatus::Failed | ToolCallStatus::Cancelled => ToolItemStatus::Failed,
        ToolCallStatus::Unavailable => ToolItemStatus::Unavailable,
    }
}

fn llm_generation_status_to_api(status: &LlmGenerationStatus) -> &'static str {
    match status {
        LlmGenerationStatus::Succeeded => "succeeded",
        LlmGenerationStatus::Failed => "failed",
        LlmGenerationStatus::Cancelled => "cancelled",
    }
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

fn event_joins_to_api(joins: &agent_core::CoreAgentJoins) -> EventJoinsView {
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

fn session_status(state: &CoreAgentState) -> ApiSessionStatus {
    match state.lifecycle.status {
        CoreAgentStatus::New => ApiSessionStatus::NotLoaded,
        CoreAgentStatus::Closed => ApiSessionStatus::Closed,
        CoreAgentStatus::Open if state.runs.active.is_some() => ApiSessionStatus::Active,
        CoreAgentStatus::Open => ApiSessionStatus::Idle,
    }
}

fn core_run_status_to_api_status(status: RunStatus) -> ApiRunStatus {
    match status {
        RunStatus::Active => ApiRunStatus::Running,
        RunStatus::Cancelling => ApiRunStatus::Cancelling,
        RunStatus::Completed => ApiRunStatus::Completed,
        RunStatus::Failed => ApiRunStatus::Failed,
        RunStatus::Cancelled => ApiRunStatus::Cancelled,
    }
}

fn model_to_api(model: &ModelSelection) -> ModelConfig {
    ModelConfig {
        provider_id: model.provider_id.clone(),
        api_kind: api_kind_to_str(&model.api_kind).to_owned(),
        model: model.model.clone(),
    }
}

fn session_config_for_session(
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
        options: agent_core::ModelProviderOptions::None,
    };
    config
        .validate_provider_compatibility()
        .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
    Ok(config)
}

fn api_kind_to_str(api_kind: &ProviderApiKind) -> &'static str {
    match api_kind {
        ProviderApiKind::OpenAiResponses => "openai:responses",
        ProviderApiKind::AnthropicMessages => "anthropic:messages",
        ProviderApiKind::OpenAiCompletions => "openai:completions",
    }
}

fn api_kind_from_str(value: &str) -> Result<ProviderApiKind, AgentApiError> {
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

fn map_runner_error(error: RunnerError) -> AgentApiError {
    match error {
        RunnerError::SessionStore(error) => map_session_store_error(error),
        RunnerError::BlobStore(error) => map_blob_store_error(error),
        RunnerError::Codec(error) => AgentApiError::internal(error.to_string()),
        RunnerError::Command(rejection) => AgentApiError::rejected(rejection.to_string()),
        RunnerError::InvalidRequest { message } => AgentApiError::invalid_request(message),
        RunnerError::Domain(error) => AgentApiError::internal(error.to_string()),
        RunnerError::Planning(error) => AgentApiError::internal(error.to_string()),
    }
}

fn map_session_store_error(error: SessionStoreError) -> AgentApiError {
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

fn map_blob_store_error(error: BlobStoreError) -> AgentApiError {
    match error {
        BlobStoreError::NotFound { blob_ref } => AgentApiError::internal(format!(
            "blob not found while projecting API view: {blob_ref}"
        )),
        BlobStoreError::Store { message } => AgentApiError::internal(message),
    }
}

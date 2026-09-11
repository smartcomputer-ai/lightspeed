//! Projection helpers from CoreAgent's committed log to `api` views.
//!
//! This crate is the explicit bridge between reducer internals and the stable
//! client-facing API. It does not admit commands or execute side effects beyond
//! reading blob-backed text needed to materialize views.

use std::collections::{BTreeMap, BTreeSet};

use api::{
    ActiveToolsView, AgentApiError, ApprovalDecisionKind, ApprovalSubjectView,
    BoundWorkflowToolDispatchInput, ContextEntryInputView, ContextEntryKindView,
    ContextEntrySourceView, ContextEntryView, ContextMessageRoleView, ContextView, EventCursor,
    EventJoinsView, InputItem, LlmUsageView, ManagedSessionWorkflowToolsInput, MediaKind,
    ModelConfig, PendingApprovalView, PrincipalKind, PrincipalRefView, ProviderContextDisplayView,
    ProviderNativeToolExecutionView, RunAcceptedSourceView, RunFailureKindView,
    RunStatus as ApiRunStatus, RunSummarySourceView, RunSummaryView, RunView, RunViewSource,
    SessionEventKindView, SessionEventView, SessionManagementView, SessionRetentionView,
    SessionStatus as ApiSessionStatus, SessionView, TokenEstimateQualityView, TokenEstimateView,
    ToolBatchView, ToolCallDisplayGroup, ToolCallDisplayView, ToolCallEventView, ToolCallView,
    ToolEffectView, ToolItemStatus, ToolKindView, ToolParallelismView, ToolView,
    WorkflowEndpointInput, WorkflowStartRefInput, WorkflowToolCompletionInput,
    WorkflowToolCompletionKeySourceInput, WorkflowToolDeclarationInput,
    WorkflowToolDefinitionInput, WorkflowToolKindInput, WorkflowToolSpecInput,
    WorkflowToolTargetInput,
};
use engine::{
    ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND,
    ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND, ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND,
    CompactionPolicy, ContextCompactionStatus, ContextCompactionTrigger, ContextEntry,
    ContextEntryId, ContextEntryInput, ContextEntryKind, ContextEntrySource, ContextEvent,
    ContextMessageRole, ContextRemovalReason, ContextRewriteReason, CoreAgentCodec, CoreAgentEntry,
    CoreAgentEvent, CoreAgentJoins, CoreAgentLifecycleEvent, CoreAgentState, CoreAgentStatus,
    EventSeq, LlmGenerationStatus, ModelSelection, OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND,
    OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND, OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND,
    ObservedToolCall, ProviderApiKind, RunEvent, RunFailure, RunFailureKind, RunId, RunSource,
    RunStatus, SessionConfig, SessionId, SteeringId, ToolBatchId, ToolCallStatus, ToolChoice,
    ToolConfigEvent, ToolEvent, ToolKind, ToolParallelism, ToolSpec, TurnEvent, TurnId,
    storage::{
        BlobStore, BlobStoreError, ReadSessionEvents, SessionRecord, SessionStore,
        SessionStoreError, StoredSessionEntry,
    },
};
use futures_util::future::try_join_all;
use serde_json::Value;

mod content;
pub use content::{content_ref_to_api, project_content_text};

pub const DEFAULT_EVENT_PAGE_LIMIT: u32 = 128;
pub const MAX_EVENT_PAGE_LIMIT: u32 = 512;
const MAX_INLINE_TEXT_BYTES: usize = 4_096;

pub struct ProjectSession<'a> {
    pub session_id: &'a SessionId,
    pub state: &'a CoreAgentState,
    pub record: &'a SessionRecord,
    pub retention: &'a SessionRetentionView,
    pub run_limit: usize,
    pub run_cursor: Option<RunId>,
}

pub struct ProjectRun<'a> {
    pub entries: &'a [CoreAgentEntry],
    pub run_id: RunId,
    pub status: ApiRunStatus,
    pub output: Option<&'a engine::ContentRef>,
    pub source: &'a RunSource,
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
    pub usage: Option<&'a engine::LlmUsage>,
}

pub struct CoreAgentProjector<'a> {
    blobs: &'a dyn BlobStore,
}

/// One run in reducer state, whichever queue bucket holds it.
#[derive(Clone, Copy)]
pub enum RunStateRef<'a> {
    Completed(&'a engine::RunRecord),
    Active(&'a engine::ActiveRun),
    Queued(&'a engine::AcceptedRun),
}

impl RunStateRef<'_> {
    fn id(&self) -> RunId {
        match self {
            Self::Completed(run) => run.run_id,
            Self::Active(run) => run.run_id,
            Self::Queued(run) => run.run_id,
        }
    }
}

impl<'a> CoreAgentProjector<'a> {
    pub fn new(blobs: &'a dyn BlobStore) -> Self {
        Self { blobs }
    }

    pub async fn project_session(
        &self,
        params: ProjectSession<'_>,
    ) -> Result<(SessionView, Option<api::RunId>, bool), AgentApiError> {
        let (runs, next_run_cursor, has_older_runs) = self
            .project_run_summaries(params.state, params.run_cursor, params.run_limit)
            .await?;
        let active_run = self.project_active_run_summary(params.state).await?;

        let config = match params.state.lifecycle.config.as_ref() {
            Some(config) => Some(self.project_session_config(config).await?),
            None => None,
        };

        let session = SessionView {
            id: params.session_id.as_str().to_owned(),
            display_name: params.record.display_name.clone(),
            metadata: params.record.metadata.clone(),
            status: session_status(params.state),
            closed_at_ms: params.record.closed_at_ms,
            retention: params.retention.clone(),
            managed: params.record.managed,
            config_revision: params.state.lifecycle.config_revision,
            config,
            created_at_ms: params.record.created_at_ms,
            updated_at_ms: params.record.updated_at_ms,
            runs,
            active_run,
            active_context: self
                .project_context_state(params.state.context.revision, &params.state.context.entries)
                .await?,
            active_tools: active_tools_to_api(
                params.state.tooling.revision,
                &params.state.tooling.tools,
            ),
            active_environment_id: params
                .state
                .environment
                .active_environment_id
                .as_ref()
                .map(|id| id.as_str().to_owned()),
            management: session_management_to_api(params.state),
            origin: params
                .record
                .origin
                .as_ref()
                .map(session_origin_to_api)
                .transpose()?,
        };
        Ok((session, next_run_cursor, has_older_runs))
    }

    /// Project one newest-first keyset page directly from reducer state.
    pub async fn project_run_summaries(
        &self,
        state: &CoreAgentState,
        cursor: Option<RunId>,
        limit: usize,
    ) -> Result<(Vec<RunSummaryView>, Option<api::RunId>, bool), AgentApiError> {
        let mut candidates = state
            .runs
            .completed
            .iter()
            .map(RunStateRef::Completed)
            .chain(state.runs.active.iter().map(RunStateRef::Active))
            .chain(state.runs.queued.iter().map(RunStateRef::Queued))
            .filter(|run| cursor.is_none_or(|cursor| run.id() < cursor))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|run| std::cmp::Reverse(run.id()));
        let has_older_runs = candidates.len() > limit;
        candidates.truncate(limit);
        let runs_with_ids =
            try_join_all(candidates.into_iter().map(|run| async move {
                Ok((run.id(), self.run_state_summary(state, run).await?))
            }))
            .await?;
        let last_run_id = runs_with_ids.last().map(|(id, _)| *id);
        let runs = runs_with_ids
            .into_iter()
            .map(|(_, summary)| summary)
            .collect();
        let next_cursor = has_older_runs
            .then(|| api_run_id(last_run_id.expect("older runs imply a non-empty page")));
        Ok((runs, next_cursor, has_older_runs))
    }

    /// The currently executing run's summary, independent of any page window.
    pub async fn project_active_run_summary(
        &self,
        state: &CoreAgentState,
    ) -> Result<Option<RunSummaryView>, AgentApiError> {
        match state.runs.active.as_ref() {
            Some(active) => Ok(Some(
                self.run_state_summary(state, RunStateRef::Active(active))
                    .await?,
            )),
            None => Ok(None),
        }
    }

    async fn run_state_summary(
        &self,
        state: &CoreAgentState,
        run: RunStateRef<'_>,
    ) -> Result<RunSummaryView, AgentApiError> {
        let (id, status, accepted_at_ms, started_at_ms, completed_at_ms, usage, source) = match run
        {
            RunStateRef::Completed(run) => (
                run.run_id,
                core_run_status_to_api_status(run.status),
                run.accepted_at_ms,
                run.started_at_ms,
                Some(run.completed_at_ms),
                run.usage.as_ref(),
                Some(&run.source),
            ),
            RunStateRef::Active(run) => (
                run.run_id,
                core_run_status_to_api_status(run.status),
                run.accepted_at_ms,
                run.started_at_ms,
                None,
                run.usage.as_ref(),
                Some(&run.source),
            ),
            RunStateRef::Queued(run) => (
                run.run_id,
                ApiRunStatus::Queued,
                run.accepted_at_ms,
                None,
                None,
                None,
                Some(&run.source),
            ),
        };
        let pending_approvals = if let Some(active) = state
            .runs
            .active
            .as_ref()
            .filter(|active| active.run_id == id)
        {
            try_join_all(active.pending_approvals().map(|record| async move {
                Ok(PendingApprovalView {
                    approval_id: record.request.approval_id.as_str().to_owned(),
                    requested_at_ms: record.requested_at_ms,
                    subject: self
                        .approval_subject_to_api(&record.request.subject)
                        .await?,
                })
            }))
            .await?
        } else {
            Vec::new()
        };
        Ok(RunSummaryView {
            id: api_run_id(id),
            status,
            accepted_at_ms,
            started_at_ms,
            completed_at_ms,
            source: self.project_run_summary_source(source).await?,
            usage: usage.map(llm_usage_to_api),
            pending_approvals,
        })
    }

    async fn project_run_summary_source(
        &self,
        source: Option<&RunSource>,
    ) -> Result<RunSummarySourceView, AgentApiError> {
        const PREVIEW_BYTES: usize = 512;
        match source {
            Some(RunSource::Input { input }) => {
                let first = input.first();
                let content_ref = first.map(|entry| entry.content.content_ref.as_str().to_owned());
                let text = match first {
                    Some(entry)
                        if is_text_message_media_type(entry.content.media_type.as_deref()) =>
                    {
                        project_content_text(self.blobs, &entry.content).await?
                    }
                    Some(entry) => entry.preview.clone(),
                    None => None,
                };
                let preview_truncated =
                    text.as_ref().is_some_and(|text| text.len() > PREVIEW_BYTES);
                Ok(RunSummarySourceView::Input {
                    content_ref,
                    preview: text.map(|text| truncate_utf8(&text, PREVIEW_BYTES)),
                    preview_truncated,
                })
            }
            None => Ok(RunSummarySourceView::Input {
                content_ref: None,
                preview: None,
                preview_truncated: false,
            }),
        }
    }

    /// Project a sequence page using stable metadata from reducer state.
    /// Event pages never need to repeat lifecycle events or generation facts.
    pub async fn project_run_with_metadata(
        &self,
        params: ProjectRun<'_>,
    ) -> Result<RunView, AgentApiError> {
        let projection = CoreAgentProjection::new(params.entries);
        let context_entries = projection.context_entries_for_run(params.run_id);
        let projected_entries = self.project_context_entries(&context_entries).await?;

        Ok(RunView {
            id: api_run_id(params.run_id),
            status: params.status,
            output: params.output.map(content_ref_to_api),
            output_text: match params.output {
                Some(content) => project_content_text(self.blobs, content).await?,
                None => None,
            },
            started_at_ms: params.started_at_ms,
            completed_at_ms: params.completed_at_ms,
            source: match params.source {
                RunSource::Input { input } => RunViewSource::Input {
                    items: self.project_input_entries(input).await?,
                },
            },
            entries: projected_entries,
            tool_batches: self
                .project_tool_batches_for_run(&projection, &context_entries, params.run_id)
                .await?,
            usage: params.usage.map(llm_usage_to_api),
            pending_approvals: self
                .pending_approvals_for_run(&projection, params.run_id)
                .await?,
        })
    }

    async fn pending_approvals_for_run(
        &self,
        projection: &CoreAgentProjection<'_>,
        run_id: RunId,
    ) -> Result<Vec<PendingApprovalView>, AgentApiError> {
        let mut pending = BTreeMap::new();
        for entry in projection.entries() {
            let CoreAgentEvent::Approval(event) = &entry.event else {
                continue;
            };
            match event {
                engine::ApprovalEvent::Requested { approval } if approval.run_id == run_id => {
                    pending.insert(
                        approval.approval_id.clone(),
                        (entry.observed_at_ms, approval.subject.clone()),
                    );
                }
                engine::ApprovalEvent::Decided {
                    approval_id,
                    run_id: event_run_id,
                    ..
                }
                | engine::ApprovalEvent::Cancelled {
                    approval_id,
                    run_id: event_run_id,
                } if *event_run_id == run_id => {
                    pending.remove(approval_id);
                }
                _ => {}
            }
        }
        let mut views = Vec::with_capacity(pending.len());
        for (approval_id, (requested_at_ms, subject)) in pending {
            views.push(PendingApprovalView {
                approval_id: approval_id.as_str().to_owned(),
                requested_at_ms,
                subject: self.approval_subject_to_api(&subject).await?,
            });
        }
        Ok(views)
    }

    async fn approval_subject_to_api(
        &self,
        subject: &engine::ApprovalSubject,
    ) -> Result<ApprovalSubjectView, AgentApiError> {
        match subject {
            engine::ApprovalSubject::McpToolCall {
                server_id,
                server_label,
                tool_name,
                arguments_ref,
            } => {
                let arguments = self.read_blob_text(arguments_ref).await?;
                Ok(ApprovalSubjectView::McpToolCall {
                    server_id: server_id.clone(),
                    server_label: server_label.clone(),
                    tool_name: tool_name.clone(),
                    arguments_ref: arguments_ref.as_str().to_owned(),
                    arguments_preview: truncate_utf8(&arguments, 4_096),
                })
            }
        }
    }

    pub async fn project_context_state(
        &self,
        revision: u64,
        entries: &[ContextEntry],
    ) -> Result<ContextView, AgentApiError> {
        Ok(ContextView {
            revision,
            entries: self
                .project_context_entries(&entries.iter().collect::<Vec<_>>())
                .await?,
        })
    }

    /// Project one entry from its authoritative content. Group projection also
    /// resolves fetch citations against earlier tool results.
    /// `superseded_by` is the newer catalog version that updated this one,
    /// known only when the caller holds the whole active context (state
    /// views); event projections pass `None`.
    pub async fn project_context_entry(
        &self,
        entry: &ContextEntry,
        superseded_by: Option<ContextEntryId>,
    ) -> Result<ContextEntryView, AgentApiError> {
        let text = match &entry.kind {
            // Binary media entries render from their preview; decoding
            // the blob as UTF-8 text would fail.
            ContextEntryKind::Message { .. } | ContextEntryKind::ReasoningState => {
                project_content_text(self.blobs, &entry.content)
                    .await?
                    .map(|full| (full, false))
            }
            ContextEntryKind::ToolCall { .. }
            | ContextEntryKind::ToolResult { .. }
            | ContextEntryKind::Catalog { .. } => {
                Some(self.bounded_blob_text(&entry.content.content_ref).await?)
            }
            _ => None,
        };
        let (text, text_truncated) = match text {
            Some((text, truncated)) => (Some(text), truncated),
            None => (None, false),
        };
        let display = match &entry.kind {
            ContextEntryKind::ProviderOpaque => self.provider_context_display(entry).await,
            _ => None,
        };
        Ok(ContextEntryView {
            id: api_item_id(entry.entry_id),
            key: entry.key.as_ref().map(|key| key.as_str().to_owned()),
            kind: context_entry_kind_to_api(&entry.kind),
            content: content_ref_to_api(&entry.content),
            origin: entry.origin.clone(),
            provenance_ref: entry
                .provenance_ref
                .as_ref()
                .map(|reference| reference.as_str().to_owned()),
            preview: entry.preview.clone(),
            provider_item_id: self.native_item_id(&entry.content).await,
            token_estimate: entry.token_estimate.as_ref().map(token_estimate_to_api),
            text,
            text_truncated,
            display,
            citations: Vec::new(),
            source: Some(context_entry_source_to_api(&entry.source)),
            supersedes: entry.supersedes.map(api_item_id),
            superseded_by: superseded_by.map(api_item_id),
        })
    }

    /// Native identity is diagnostic; missing blobs or payload IDs do not invent one.
    async fn native_item_id(&self, content: &engine::ContentRef) -> Option<String> {
        if content.media_type.as_deref() != Some("application/json")
            || !content
                .provider_kind
                .as_deref()
                .is_some_and(|kind| kind.starts_with("openai.") || kind.starts_with("anthropic."))
        {
            return None;
        }
        let bytes = self.blobs.read_bytes(&content.content_ref).await.ok()?;
        let raw: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        raw.get("id")?.as_str().map(str::to_owned)
    }

    /// Read a blob's text bounded to the inline budget, reporting whether the
    /// inline copy is a prefix of a longer body.
    async fn bounded_blob_text(
        &self,
        content_ref: &engine::BlobRef,
    ) -> Result<(String, bool), AgentApiError> {
        let full = self.read_blob_text(content_ref).await?;
        let truncated = full.len() > MAX_INLINE_TEXT_BYTES;
        Ok((truncate_utf8(&full, MAX_INLINE_TEXT_BYTES), truncated))
    }

    /// Project active context entries, resolving `supersededBy` across them.
    async fn project_context_entries(
        &self,
        entries: &[&ContextEntry],
    ) -> Result<Vec<ContextEntryView>, AgentApiError> {
        let superseded_by = superseded_by_map(entries);
        self.project_context_entry_group(entries, &superseded_by)
            .await
    }

    async fn project_context_event_entries(
        &self,
        entries: &[ContextEntry],
    ) -> Result<Vec<ContextEntryView>, AgentApiError> {
        self.project_context_entry_group(&entries.iter().collect::<Vec<_>>(), &BTreeMap::new())
            .await
    }

    async fn project_context_entry_group(
        &self,
        entries: &[&ContextEntry],
        superseded_by: &BTreeMap<ContextEntryId, ContextEntryId>,
    ) -> Result<Vec<ContextEntryView>, AgentApiError> {
        let mut views = try_join_all(entries.iter().map(|entry| {
            self.project_context_entry(entry, superseded_by.get(&entry.entry_id).copied())
        }))
        .await?;
        self.attach_citations(entries, &mut views).await;
        Ok(views)
    }

    /// Project citations from each assistant message's own native payload.
    /// Fetch citations may refer to earlier tool results in the same turn.
    async fn attach_citations(&self, entries: &[&ContextEntry], views: &mut [ContextEntryView]) {
        for (index, entry) in entries.iter().enumerate() {
            if !matches!(
                entry.kind,
                ContextEntryKind::Message {
                    role: ContextMessageRole::Assistant
                }
            ) {
                continue;
            }
            let citations = match entry.content.provider_kind.as_deref() {
                Some(ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND) => {
                    let Some(blocks) = self.read_context_json(entry).await else {
                        continue;
                    };
                    let fetched_urls = if anthropic_has_document_citations(&blocks) {
                        self.fetched_urls_before(entries, index).await
                    } else {
                        Vec::new()
                    };
                    anthropic_citations(&blocks, &fetched_urls)
                }
                Some(
                    OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND
                    | llm_clients::content::OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND,
                ) => {
                    let Some(item) = self.read_context_json(entry).await else {
                        continue;
                    };
                    openai_citations(&item)
                }
                _ => continue,
            };
            views[index].citations = citations;
        }
    }

    /// URLs of the fetch results earlier in the same turn, in response order.
    /// Anthropic fetch citations locate a document by that index instead of
    /// naming its URL.
    async fn fetched_urls_before(&self, entries: &[&ContextEntry], index: usize) -> Vec<String> {
        let source = &entries[index].source;
        let mut urls = Vec::new();
        for entry in entries[..index].iter().filter(|entry| {
            entry.source == *source
                && entry.content.provider_kind.as_deref()
                    == Some(ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND)
        }) {
            let Some(block) = self.read_context_json(entry).await else {
                continue;
            };
            if block.get("type").and_then(Value::as_str) != Some("web_fetch_tool_result") {
                continue;
            }
            if let Some(url) = block
                .get("content")
                .and_then(|content| content.get("url"))
                .and_then(Value::as_str)
            {
                urls.push(url.to_owned());
            }
        }
        urls
    }

    async fn read_context_json(&self, entry: &ContextEntry) -> Option<Value> {
        let text = self.read_blob_text(&entry.content.content_ref).await.ok()?;
        serde_json::from_str(&text).ok()
    }

    pub async fn project_input_entries(
        &self,
        input: &[ContextEntryInput],
    ) -> Result<Vec<InputItem>, AgentApiError> {
        try_join_all(input.iter().map(|entry| async move {
            Ok(match entry.kind {
                ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                } => {
                    // Binary media entries project as media items; decoding
                    // the blob as UTF-8 text would fail.
                    if is_text_message_media_type(entry.content.media_type.as_deref()) {
                        InputItem::Text {
                            origin: entry.origin.clone(),
                            text: project_content_text(self.blobs, &entry.content)
                                .await?
                                .unwrap_or_default(),
                        }
                    } else {
                        let mime = entry.content.media_type.clone().unwrap_or_default();
                        InputItem::Media {
                            origin: entry.origin.clone(),
                            blob_ref: entry.content.content_ref.as_str().to_owned(),
                            kind: media_kind_for_mime(&mime),
                            mime,
                            name: None,
                        }
                    }
                }
                _ => InputItem::TextRef {
                    origin: entry.origin.clone(),
                    blob_ref: entry.content.content_ref.as_str().to_owned(),
                },
            })
        }))
        .await
    }

    pub async fn project_session_config(
        &self,
        config: &SessionConfig,
    ) -> Result<api::SessionConfig, AgentApiError> {
        session_config_to_api(config)
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
            kind: self.project_event_kind(&entry.event).await?,
        })
    }

    pub async fn project_event_kind(
        &self,
        kind: &CoreAgentEvent,
    ) -> Result<SessionEventKindView, AgentApiError> {
        match kind {
            CoreAgentEvent::Lifecycle(event) => match event {
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
            CoreAgentEvent::WorkflowToolConfig(event) => match event {
                engine::WorkflowToolConfigEvent::ManagedBindingsAdmitted {
                    lifecycle_controller,
                    creation_fingerprint,
                    bindings,
                    ..
                } => Ok(SessionEventKindView::WorkflowToolsConfigured {
                    lifecycle_controller_workflow_kind: lifecycle_controller
                        .as_ref()
                        .map(|controller| controller.workflow_kind.clone()),
                    creation_fingerprint: creation_fingerprint.clone(),
                    tool_ids: bindings
                        .iter()
                        .map(|binding| binding.definition.tool_id.as_str().to_owned())
                        .collect(),
                }),
                engine::WorkflowToolConfigEvent::SystemBindingAdmitted { binding } => {
                    Ok(SessionEventKindView::SystemWorkflowToolConfigured {
                        tool_id: binding.definition.tool_id.as_str().to_owned(),
                        binding_fingerprint: binding.binding_fingerprint.clone(),
                    })
                }
            },
            CoreAgentEvent::WorkflowTool(event) => match event {
                engine::WorkflowToolEvent::Emitted { invocation } => {
                    Ok(SessionEventKindView::WorkflowToolEmitted {
                        invocation_id: invocation.invocation_id.as_str().to_owned(),
                        tool_id: invocation.tool_id.as_str().to_owned(),
                        semantic_type: invocation.semantic_type.clone(),
                        schema_revision: invocation.schema_revision,
                        binding_fingerprint: invocation.binding_fingerprint.clone(),
                        run_id: api_run_id(invocation.run_id),
                        turn_id: api_turn_id(invocation.turn_id),
                        batch_id: api_tool_batch_id(invocation.tool_batch_id),
                        call_id: invocation.tool_call_id.as_str().to_owned(),
                        arguments_ref: invocation.arguments_ref.as_str().to_owned(),
                        completion_promises: invocation.completion_promises.as_ref().map(
                            |promises| {
                                promises
                                    .iter()
                                    .map(|(key, promise_id)| {
                                        (key.clone(), promise_id.as_str().to_owned())
                                    })
                                    .collect()
                            },
                        ),
                    })
                }
                engine::WorkflowToolEvent::StartRequested {
                    invocation,
                    execution_id,
                } => Ok(SessionEventKindView::WorkflowToolStartRequested {
                    invocation_id: invocation.invocation_id.as_str().to_owned(),
                    tool_id: invocation.tool_id.as_str().to_owned(),
                    semantic_type: invocation.semantic_type.clone(),
                    schema_revision: invocation.schema_revision,
                    binding_fingerprint: invocation.binding_fingerprint.clone(),
                    run_id: api_run_id(invocation.run_id),
                    turn_id: api_turn_id(invocation.turn_id),
                    batch_id: api_tool_batch_id(invocation.tool_batch_id),
                    call_id: invocation.tool_call_id.as_str().to_owned(),
                    arguments_ref: invocation.arguments_ref.as_str().to_owned(),
                    execution_id: execution_id.clone(),
                    completion_promises: invocation
                        .completion_promises
                        .as_ref()
                        .map(|promises| {
                            promises
                                .iter()
                                .map(|(key, promise_id)| {
                                    (key.clone(), promise_id.as_str().to_owned())
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                }),
                engine::WorkflowToolEvent::StartFailed {
                    invocation_id,
                    error_ref,
                } => Ok(SessionEventKindView::WorkflowToolStartFailed {
                    invocation_id: invocation_id.as_str().to_owned(),
                    error_ref: error_ref.as_str().to_owned(),
                }),
                engine::WorkflowToolEvent::DeliveryFailed {
                    invocation_id,
                    error_ref,
                } => Ok(SessionEventKindView::WorkflowToolDeliveryFailed {
                    invocation_id: invocation_id.as_str().to_owned(),
                    error_ref: error_ref.as_str().to_owned(),
                }),
            },
            CoreAgentEvent::Run(event) => match event {
                RunEvent::Accepted(accepted) => Ok(SessionEventKindView::RunAccepted {
                    run_id: api_run_id(accepted.run_id),
                    submission_id: accepted
                        .submission_id
                        .as_ref()
                        .map(|id| id.as_str().to_owned()),
                    source: match &accepted.source {
                        RunSource::Input { input } => RunAcceptedSourceView::Input {
                            entries: project_context_entry_inputs(input),
                        },
                    },
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
                RunEvent::Completed { run_id, output } => Ok(SessionEventKindView::RunCompleted {
                    run_id: api_run_id(*run_id),
                    output: output.as_ref().map(content_ref_to_api),
                }),
                RunEvent::Failed { run_id, failure } => Ok(SessionEventKindView::RunFailed {
                    run_id: api_run_id(*run_id),
                    kind: run_failure_kind_to_api(failure.kind.clone()),
                    message: self.run_failure_message(failure).await,
                }),
                RunEvent::Cancelled { run_id }
                | RunEvent::ForceCancelled { run_id }
                | RunEvent::QueuedCancelled { run_id } => Ok(SessionEventKindView::RunCancelled {
                    run_id: api_run_id(*run_id),
                }),
            },
            CoreAgentEvent::Approval(event) => match event {
                engine::ApprovalEvent::Requested { approval } => {
                    Ok(SessionEventKindView::ApprovalRequested {
                        run_id: api_run_id(approval.run_id),
                        approval_id: approval.approval_id.as_str().to_owned(),
                        subject: self.approval_subject_to_api(&approval.subject).await?,
                    })
                }
                engine::ApprovalEvent::RunParked { run_id } => {
                    Ok(SessionEventKindView::ApprovalRunParked {
                        run_id: api_run_id(*run_id),
                    })
                }
                engine::ApprovalEvent::Decided {
                    approval_id,
                    run_id,
                    decision,
                    note,
                    decided_by,
                } => Ok(SessionEventKindView::ApprovalDecided {
                    run_id: api_run_id(*run_id),
                    approval_id: approval_id.as_str().to_owned(),
                    decision: match decision {
                        engine::ApprovalDecision::Approved => ApprovalDecisionKind::Approve,
                        engine::ApprovalDecision::Rejected => ApprovalDecisionKind::Reject,
                    },
                    note: note.clone(),
                    decided_by: decided_by.as_ref().map(approval_principal_to_api),
                }),
                engine::ApprovalEvent::Cancelled {
                    approval_id,
                    run_id,
                } => Ok(SessionEventKindView::ApprovalCancelled {
                    run_id: api_run_id(*run_id),
                    approval_id: approval_id.as_str().to_owned(),
                }),
            },
            CoreAgentEvent::Promise(event) => match event {
                engine::PromiseEvent::Created { promise } => {
                    Ok(SessionEventKindView::PromiseCreated {
                        promise_id: promise.promise_id.as_str().to_owned(),
                        source: promise_source_name(&promise.source).to_owned(),
                    })
                }
                engine::PromiseEvent::Resolved {
                    promise_id,
                    payload_ref,
                } => Ok(SessionEventKindView::PromiseResolved {
                    promise_id: promise_id.as_str().to_owned(),
                    payload_ref: payload_ref.as_ref().map(|ref_| ref_.as_str().to_owned()),
                }),
                engine::PromiseEvent::Failed {
                    promise_id,
                    error_ref,
                } => Ok(SessionEventKindView::PromiseFailed {
                    promise_id: promise_id.as_str().to_owned(),
                    error_ref: error_ref.as_ref().map(|ref_| ref_.as_str().to_owned()),
                }),
                engine::PromiseEvent::Cancelled { promise_id } => {
                    Ok(SessionEventKindView::PromiseCancelled {
                        promise_id: promise_id.as_str().to_owned(),
                    })
                }
                engine::PromiseEvent::Detached { promise_id } => {
                    Ok(SessionEventKindView::PromiseDetached {
                        promise_id: promise_id.as_str().to_owned(),
                    })
                }
            },
            CoreAgentEvent::Turn(event) => match event {
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
                    facts,
                } => Ok(SessionEventKindView::TurnGenerationCompleted {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                    status: llm_generation_status_to_api(status).to_owned(),
                    usage: facts.usage.as_ref().map(llm_usage_to_api),
                }),
                TurnEvent::Completed { turn_id, .. } => Ok(SessionEventKindView::TurnCompleted {
                    turn_id: api_turn_id(*turn_id),
                }),
                TurnEvent::Cancelled { turn_id, run_id } => {
                    Ok(SessionEventKindView::TurnCancelled {
                        run_id: api_run_id(*run_id),
                        turn_id: api_turn_id(*turn_id),
                    })
                }
            },
            CoreAgentEvent::Context(event) => match event {
                ContextEvent::EntriesApplied {
                    base_revision,
                    entries,
                } => {
                    let projected = self.project_context_event_entries(entries).await?;
                    Ok(SessionEventKindView::ContextEntriesApplied {
                        base_revision: *base_revision,
                        revision: context_event_revision(*base_revision)?,
                        entries: projected,
                    })
                }
                ContextEvent::EntriesRemoved {
                    base_revision,
                    entry_ids,
                    reason,
                } => Ok(SessionEventKindView::ContextEntriesRemoved {
                    base_revision: *base_revision,
                    revision: context_event_revision(*base_revision)?,
                    entry_ids: entry_ids
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
                    let projected = self.project_context_event_entries(entries).await?;
                    Ok(SessionEventKindView::ContextKeyPrefixReplaced {
                        base_revision: *base_revision,
                        revision: context_event_revision(*base_revision)?,
                        key_prefix: key_prefix.as_str().to_owned(),
                        entries: projected,
                    })
                }
                ContextEvent::StateReplaced {
                    base_revision,
                    entries,
                    reason,
                } => {
                    let projected = self.project_context_event_entries(entries).await?;
                    Ok(SessionEventKindView::ContextStateReplaced {
                        base_revision: *base_revision,
                        revision: context_event_revision(*base_revision)?,
                        entries: projected,
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
            CoreAgentEvent::Environment(event) => match event {
                engine::EnvironmentEvent::ActiveEnvironmentSet { environment_id } => {
                    Ok(SessionEventKindView::ActiveEnvironmentChanged {
                        environment_id: Some(environment_id.as_str().to_owned()),
                    })
                }
                engine::EnvironmentEvent::ActiveEnvironmentCleared => {
                    Ok(SessionEventKindView::ActiveEnvironmentChanged {
                        environment_id: None,
                    })
                }
            },
            CoreAgentEvent::ToolConfig(event) => match event {
                ToolConfigEvent::ToolsReplaced { base_revision, .. } => {
                    Ok(SessionEventKindView::ToolsReplaced {
                        base_revision: *base_revision,
                        revision: tool_event_revision(*base_revision)?,
                    })
                }
                ToolConfigEvent::ToolsPatched {
                    base_revision,
                    patch,
                } => Ok(SessionEventKindView::ToolsPatched {
                    base_revision: *base_revision,
                    revision: tool_event_revision(*base_revision)?,
                    upserted: patch
                        .upsert
                        .iter()
                        .map(|tool| tool.name.as_str().to_owned())
                        .collect(),
                    removed: patch
                        .remove
                        .iter()
                        .map(|tool_name| tool_name.as_str().to_owned())
                        .collect(),
                }),
            },
            CoreAgentEvent::Tool(event) => match event {
                ToolEvent::BatchStarted {
                    run_id,
                    turn_id,
                    batch_id,
                    calls,
                    ..
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
                    output_bytes: result.output_bytes,
                    truncated: result.truncated,
                }),
                ToolEvent::BatchDeferred {
                    run_id,
                    turn_id,
                    batch_id,
                    ..
                } => Ok(SessionEventKindView::ToolBatchDeferred {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                    batch_id: api_tool_batch_id(*batch_id),
                }),
                ToolEvent::BatchResumed {
                    run_id,
                    turn_id,
                    batch_id,
                } => Ok(SessionEventKindView::ToolBatchResumed {
                    run_id: api_run_id(*run_id),
                    turn_id: api_turn_id(*turn_id),
                    batch_id: api_tool_batch_id(*batch_id),
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
        try_join_all(calls.iter().map(|call| async move {
            let arguments = self.read_blob_text(&call.arguments_ref).await?;
            Ok(ToolCallEventView {
                tool_id: call.tool_id.as_ref().map(|id| id.as_str().to_owned()),
                call_id: call.call_id.as_str().to_owned(),
                tool_name: call.tool_name.as_str().to_owned(),
                arguments_ref: call.arguments_ref.as_str().to_owned(),
                arguments: Some(truncate_utf8(&arguments, MAX_INLINE_TEXT_BYTES)),
                display: tool_call_display(call.tool_name.as_str(), &arguments),
            })
        }))
        .await
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
            let CoreAgentEvent::Tool(event) = &entry.event else {
                continue;
            };
            match event {
                ToolEvent::BatchStarted {
                    run_id: event_run_id,
                    turn_id,
                    batch_id,
                    calls,
                    ..
                } if *event_run_id == run_id => {
                    let projected_calls = try_join_all(calls.iter().map(|call| async {
                        let result = result_by_call.get(call.call_id.as_str());
                        let arguments = self.read_blob_text(&call.arguments_ref).await?;
                        Ok(ToolCallView {
                            tool_id: call.tool_id.as_ref().map(|id| id.as_str().to_owned()),
                            call_id: call.call_id.as_str().to_owned(),
                            tool_name: call.tool_name.as_str().to_owned(),
                            arguments_ref: call.arguments_ref.as_str().to_owned(),
                            arguments: Some(truncate_utf8(&arguments, MAX_INLINE_TEXT_BYTES)),
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
                            started_at_ms: None,
                            completed_at_ms: None,
                            duration_ms: None,
                        })
                    }))
                    .await?;
                    batches.push(ToolBatchView {
                        id: api_tool_batch_id(*batch_id),
                        turn_id: api_turn_id(*turn_id),
                        status: ToolItemStatus::Running,
                        calls: projected_calls,
                    });
                }
                ToolEvent::CallStarted {
                    run_id: event_run_id,
                    batch_id,
                    call_id,
                    ..
                } if *event_run_id == run_id => {
                    let batch_id = api_tool_batch_id(*batch_id);
                    if let Some(call) = batches
                        .iter_mut()
                        .find(|batch| batch.id == batch_id)
                        .and_then(|batch| {
                            batch
                                .calls
                                .iter_mut()
                                .find(|call| call.call_id == call_id.as_str())
                        })
                    {
                        call.started_at_ms = Some(entry.observed_at_ms);
                    }
                }
                // The durable per-call completion carries the engine's call
                // status; it distinguishes `cancelled` from `failed`, which
                // the model-visible result entry alone cannot.
                ToolEvent::CallCompleted {
                    run_id: event_run_id,
                    batch_id,
                    result,
                    ..
                } if *event_run_id == run_id => {
                    let batch_id = api_tool_batch_id(*batch_id);
                    if let Some(call) = batches
                        .iter_mut()
                        .find(|batch| batch.id == batch_id)
                        .and_then(|batch| {
                            batch
                                .calls
                                .iter_mut()
                                .find(|call| call.call_id == result.call_id.as_str())
                        })
                    {
                        call.status = core_tool_status_to_api_status(result.status);
                        call.is_error = result.status.is_error();
                        call.completed_at_ms = Some(entry.observed_at_ms);
                        call.duration_ms = result.duration_ms;
                    }
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
        let projected = try_join_all(context_entries.iter().filter_map(|item| {
            let ContextEntryKind::ToolResult { call_id, is_error } = &item.kind else {
                return None;
            };
            Some(async move {
                Ok((
                    call_id.as_str().to_owned(),
                    ProjectedToolResult {
                        output: Some(truncate_utf8(
                            &self.read_blob_text(&item.content.content_ref).await?,
                            MAX_INLINE_TEXT_BYTES,
                        )),
                        is_error: *is_error,
                        status: if *is_error {
                            ToolItemStatus::Failed
                        } else {
                            ToolItemStatus::Succeeded
                        },
                    },
                ))
            })
        }))
        .await?;
        Ok(projected.into_iter().collect())
    }

    async fn provider_context_display(
        &self,
        item: &ContextEntry,
    ) -> Option<ProviderContextDisplayView> {
        let display: fn(&Value) -> Option<ProviderContextDisplayView> =
            match item.content.provider_kind.as_deref() {
                Some(OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND) => openai_mcp_call_display,
                Some(OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND) => {
                    openai_web_search_call_display
                }
                Some(ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND) => {
                    anthropic_server_tool_use_display
                }
                _ => return None,
            };
        let text = self.read_blob_text(&item.content.content_ref).await.ok()?;
        let value = serde_json::from_str::<Value>(&text).ok()?;
        display(&value)
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

fn citation_view(
    url: &str,
    title: Option<&str>,
    cited_text: Option<&str>,
) -> Option<api::CitationView> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return None;
    }
    Some(api::CitationView {
        url: url.to_owned(),
        title: title.map(ToOwned::to_owned),
        cited_text: cited_text.map(ToOwned::to_owned),
    })
}

fn anthropic_block_citations(blocks: &Value) -> impl Iterator<Item = &Value> {
    blocks.as_array().into_iter().flatten().flat_map(|block| {
        block
            .get("citations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    })
}

fn anthropic_has_document_citations(blocks: &Value) -> bool {
    anthropic_block_citations(blocks)
        .any(|citation| citation.get("url").and_then(Value::as_str).is_none())
}

/// The sources cited across one message's text blocks, in block order and
/// unique by URL. Search citations name their URL; fetch citations locate a
/// document index among the turn's fetch results.
fn anthropic_citations(blocks: &Value, fetched_urls: &[String]) -> Vec<api::CitationView> {
    let mut seen = BTreeSet::new();
    anthropic_block_citations(blocks)
        .filter_map(|citation| {
            let url = match citation.get("url").and_then(Value::as_str) {
                Some(url) => url,
                None => anthropic_fetched_citation_url(citation, fetched_urls)?,
            };
            if !seen.insert(url.to_owned()) {
                return None;
            }
            citation_view(
                url,
                citation
                    .get("title")
                    .or_else(|| citation.get("document_title"))
                    .and_then(Value::as_str),
                citation.get("cited_text").and_then(Value::as_str),
            )
        })
        .collect()
}

fn anthropic_fetched_citation_url<'a>(
    citation: &Value,
    fetched_urls: &'a [String],
) -> Option<&'a str> {
    if !matches!(
        citation.get("type").and_then(Value::as_str),
        Some("char_location" | "page_location" | "content_block_location")
    ) {
        return None;
    }
    let document_index = citation
        .get("document_index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok());
    match document_index {
        Some(index) => fetched_urls.get(index),
        None if fetched_urls.len() == 1 => fetched_urls.first(),
        None => None,
    }
    .map(String::as_str)
}

/// The sources an OpenAI message item cites, in annotation order and unique
/// by URL. The cited text is the annotated span of the output text.
fn openai_citations(item: &Value) -> Vec<api::CitationView> {
    let mut seen = BTreeSet::new();
    let text = llm_clients::content::openai_completion_message(item);
    let root_annotations = item
        .get("annotations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|annotation| (text.as_deref(), annotation));
    let part_annotations = item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("output_text" | "text")
            )
        })
        .flat_map(|part| {
            let text = part.get("text").and_then(Value::as_str);
            part.get("annotations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(move |annotation| (text, annotation))
        });
    root_annotations
        .chain(part_annotations)
        .filter_map(|(text, annotation)| {
            if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
                return None;
            }
            let citation = annotation.get("url_citation").unwrap_or(annotation);
            let url = citation.get("url").and_then(Value::as_str)?;
            if !seen.insert(url.to_owned()) {
                return None;
            }
            let cited_text = text.and_then(|text| openai_annotated_span(text, citation));
            citation_view(
                url,
                citation.get("title").and_then(Value::as_str),
                cited_text.as_deref(),
            )
        })
        .collect()
}

fn openai_annotated_span(text: &str, annotation: &Value) -> Option<String> {
    let start = usize::try_from(annotation.get("start_index")?.as_u64()?).ok()?;
    let end = usize::try_from(annotation.get("end_index")?.as_u64()?).ok()?;
    if start >= end {
        return None;
    }
    let cited = text
        .chars()
        .skip(start)
        .take(end - start)
        .collect::<String>();
    (!cited.is_empty()).then_some(cited)
}

fn run_failure_kind_to_api(kind: RunFailureKind) -> RunFailureKindView {
    match kind {
        RunFailureKind::ModelFailure => RunFailureKindView::ModelFailure,
        RunFailureKind::ToolFailure => RunFailureKindView::ToolFailure,
        RunFailureKind::ContextFailure => RunFailureKindView::ContextFailure,
        RunFailureKind::LimitExceeded => RunFailureKindView::LimitExceeded,
        RunFailureKind::Cancelled => RunFailureKindView::Cancelled,
        RunFailureKind::Internal => RunFailureKindView::Internal,
    }
}

fn session_management_to_api(state: &CoreAgentState) -> Option<SessionManagementView> {
    let version = state.workflow_tools.managed_declaration_version?;
    Some(ManagedSessionWorkflowToolsInput {
        version,
        lifecycle_controller: state.workflow_tools.lifecycle_controller.as_ref().map(
            |controller| WorkflowEndpointInput {
                workflow_id: controller.workflow_id.clone(),
                workflow_kind: controller.workflow_kind.clone(),
            },
        ),
        tools: state
            .workflow_tools
            .bindings
            .iter()
            .filter(|(tool_id, _)| !state.workflow_tools.system_binding_ids.contains(*tool_id))
            .filter_map(|(_, binding)| workflow_tool_declaration_to_api(binding))
            .collect(),
    })
}

fn workflow_tool_declaration_to_api(
    binding: &engine::WorkflowToolBinding,
) -> Option<WorkflowToolDeclarationInput> {
    Some(WorkflowToolDeclarationInput {
        definition: WorkflowToolDefinitionInput {
            tool_id: binding.definition.tool_id.as_str().to_owned(),
            revision: binding.definition.revision,
            semantic_type: binding.definition.semantic_type.clone(),
            tool: WorkflowToolSpecInput {
                name: binding.definition.tool.name.as_str().to_owned(),
                kind: match &binding.definition.tool.kind {
                    engine::ToolKind::Function(function) => WorkflowToolKindInput::Function {
                        description_ref: function
                            .description_ref
                            .as_ref()
                            .map(|value| value.as_str().to_owned()),
                        input_schema_ref: function.input_schema_ref.as_str().to_owned(),
                        output_schema_ref: function
                            .output_schema_ref
                            .as_ref()
                            .map(|value| value.as_str().to_owned()),
                        strict: function.strict,
                        provider_options_ref: function
                            .provider_options_ref
                            .as_ref()
                            .map(|value| value.as_str().to_owned()),
                    },
                    _ => return None,
                },
                parallelism: tool_parallelism_to_api(binding.definition.tool.parallelism),
            },
        },
        target: match &binding.target {
            engine::WorkflowToolTarget::Bound { receiver, dispatch } => {
                WorkflowToolTargetInput::Bound {
                    receiver: WorkflowEndpointInput {
                        workflow_id: receiver.workflow_id.clone(),
                        workflow_kind: receiver.workflow_kind.clone(),
                    },
                    dispatch: match dispatch {
                        engine::BoundWorkflowToolDispatch::Pull => {
                            BoundWorkflowToolDispatchInput::Pull
                        }
                        engine::BoundWorkflowToolDispatch::Push => {
                            BoundWorkflowToolDispatchInput::Push
                        }
                    },
                }
            }
            engine::WorkflowToolTarget::Start { start } => WorkflowToolTargetInput::Start {
                start: WorkflowStartRefInput {
                    recipe_format: start.recipe_format,
                    revision: start.revision,
                    recipe_ref: start.recipe_ref.as_str().to_owned(),
                    recipe_fingerprint: start.recipe_fingerprint.clone(),
                },
            },
        },
        completion: match &binding.completion {
            engine::WorkflowToolCompletion::Accepted => WorkflowToolCompletionInput::Accepted,
            engine::WorkflowToolCompletion::Joined {
                reply_schema_ref,
                deadline_after_ms,
            } => WorkflowToolCompletionInput::Joined {
                reply_schema_ref: reply_schema_ref
                    .as_ref()
                    .map(|value| value.as_str().to_owned()),
                deadline_after_ms: *deadline_after_ms,
            },
            engine::WorkflowToolCompletion::Promises {
                reply_schema_ref,
                deadline_after_ms,
                max_promises,
                key_source,
            } => WorkflowToolCompletionInput::Promises {
                reply_schema_ref: reply_schema_ref
                    .as_ref()
                    .map(|value| value.as_str().to_owned()),
                deadline_after_ms: *deadline_after_ms,
                max_promises: *max_promises,
                key_source: match key_source {
                    engine::WorkflowToolCompletionKeySource::Reply => {
                        WorkflowToolCompletionKeySourceInput::Reply
                    }
                    engine::WorkflowToolCompletionKeySource::StringArray { pointer } => {
                        WorkflowToolCompletionKeySourceInput::StringArray {
                            pointer: pointer.clone(),
                        }
                    }
                    engine::WorkflowToolCompletionKeySource::ArrayItemField { pointer, field } => {
                        WorkflowToolCompletionKeySourceInput::ArrayItemField {
                            pointer: pointer.clone(),
                            field: field.clone(),
                        }
                    }
                    engine::WorkflowToolCompletionKeySource::ArrayIndices { pointer, prefix } => {
                        WorkflowToolCompletionKeySourceInput::ArrayIndices {
                            pointer: pointer.clone(),
                            prefix: prefix.clone(),
                        }
                    }
                },
            },
        },
    })
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

    /// Select committed context entries belonging to a run.
    pub fn context_entries_for_run(&self, run_id: RunId) -> Vec<&'a ContextEntry> {
        let mut seen = BTreeSet::new();
        self.entries
            .iter()
            .filter_map(|entry| {
                let CoreAgentEvent::Context(ContextEvent::EntriesApplied { entries, .. }) =
                    &entry.event
                else {
                    return None;
                };
                Some(
                    entries
                        .iter()
                        .filter(|entry| context_entry_run_id(entry) == Some(run_id)),
                )
            })
            .flatten()
            .filter(|entry| seen.insert(entry.entry_id))
            .collect()
    }
}

pub fn context_entry_run_id(entry: &ContextEntry) -> Option<RunId> {
    match &entry.source {
        ContextEntrySource::RunInput { run_id, .. }
        | ContextEntrySource::Steering { run_id, .. }
        | ContextEntrySource::AssistantOutput { run_id, .. }
        | ContextEntrySource::ApprovalDecision { run_id, .. }
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
            entries.push(decode_stored_entry(&codec, entry)?);
        }
        if page.complete {
            return Ok(entries);
        }
    }
}

pub fn decode_stored_entry(
    codec: &CoreAgentCodec,
    entry: &StoredSessionEntry,
) -> Result<CoreAgentEntry, AgentApiError> {
    codec
        .decode_entry(entry)
        .map_err(|error| AgentApiError::internal(error.to_string()))
}

pub fn replay_core_agent_state(
    entries: &[CoreAgentEntry],
) -> Result<CoreAgentState, AgentApiError> {
    let mut state = CoreAgentState::new();
    for entry in entries {
        engine::apply_event(&mut state, entry)
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
    }
    Ok(state)
}

pub fn input_text(input: &[InputItem]) -> Result<String, AgentApiError> {
    let mut parts = Vec::new();
    for item in input {
        match item {
            InputItem::Text { text, .. } => {
                let text = text.trim();
                if !text.is_empty() {
                    parts.push(text);
                }
            }
            InputItem::TextRef { .. } => {
                return Err(AgentApiError::invalid_request(
                    "session/runs/start textRef input requires blob store resolution",
                ));
            }
            InputItem::Media { .. } => {
                return Err(AgentApiError::invalid_request(
                    "session/runs/start media input requires blob store resolution",
                ));
            }
            InputItem::Catalog { .. } => {
                return Err(AgentApiError::invalid_request(
                    "catalog items are context, not conversation: publish them with session/context/append",
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(AgentApiError::invalid_request(
            "session/runs/start input must contain at least one non-empty text item",
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
    entries.iter().find_map(|entry| match &entry.event {
        CoreAgentEvent::Run(RunEvent::Started { run_id, .. }) => Some(*run_id),
        _ => None,
    })
}

pub fn context_entry_source_to_api(source: &ContextEntrySource) -> ContextEntrySourceView {
    match source {
        ContextEntrySource::ContextEdit => ContextEntrySourceView::ContextEdit,
        ContextEntrySource::RunInput {
            run_id,
            input_index,
        } => ContextEntrySourceView::RunInput {
            run_id: api_run_id(*run_id),
            input_index: *input_index,
        },
        ContextEntrySource::Steering {
            run_id,
            steering_id,
            input_index,
        } => ContextEntrySourceView::Steering {
            run_id: api_run_id(*run_id),
            steering_id: api_steering_id(*steering_id),
            input_index: *input_index,
        },
        ContextEntrySource::AssistantOutput { run_id, turn_id } => {
            ContextEntrySourceView::AssistantOutput {
                run_id: api_run_id(*run_id),
                turn_id: api_turn_id(*turn_id),
            }
        }
        ContextEntrySource::ApprovalDecision {
            run_id,
            approval_id,
        } => ContextEntrySourceView::ApprovalDecision {
            run_id: api_run_id(*run_id),
            approval_id: approval_id.as_str().to_owned(),
        },
        ContextEntrySource::Tool {
            run_id,
            turn_id,
            batch_id,
        } => ContextEntrySourceView::Tool {
            run_id: api_run_id(*run_id),
            turn_id: api_turn_id(*turn_id),
            batch_id: batch_id.map(api_tool_batch_id),
        },
        ContextEntrySource::Reasoning { run_id, turn_id } => ContextEntrySourceView::Reasoning {
            run_id: api_run_id(*run_id),
            turn_id: api_turn_id(*turn_id),
        },
        ContextEntrySource::Runtime { label } => ContextEntrySourceView::Runtime {
            label: label.clone(),
        },
    }
}

pub fn api_run_id(run_id: RunId) -> String {
    format!("run_{}", run_id.as_u64())
}

fn promise_source_name(source: &engine::PromiseSource) -> &'static str {
    match source {
        engine::PromiseSource::Timer { .. } => "timer",
        engine::PromiseSource::Workflow { .. } => "workflow",
    }
}

fn approval_principal_to_api(principal: &engine::ApprovalPrincipal) -> PrincipalRefView {
    PrincipalRefView {
        kind: match principal.kind.as_str() {
            "user" => PrincipalKind::User,
            "service_account" => PrincipalKind::ServiceAccount,
            _ => PrincipalKind::UniverseDefault,
        },
        id: principal.id.clone(),
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
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

fn tool_event_revision(base_revision: u64) -> Result<u64, AgentApiError> {
    base_revision
        .checked_add(1)
        .ok_or_else(|| AgentApiError::internal("tool event revision overflow"))
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

fn compaction_policy_to_api(policy: &CompactionPolicy) -> api::CompactionPolicy {
    match policy {
        CompactionPolicy::Disabled => api::CompactionPolicy::Disabled,
        CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens,
        } => api::CompactionPolicy::ProviderTriggered {
            compact_threshold_tokens: *compact_threshold_tokens,
        },
        CompactionPolicy::ProviderStandalone {
            compact_threshold_tokens,
            target_tokens,
        } => api::CompactionPolicy::ProviderStandalone {
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
        RunStatus::Parked => ApiRunStatus::Parked,
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
        ToolCallStatus::Failed => ToolItemStatus::Failed,
        ToolCallStatus::Cancelled => ToolItemStatus::Cancelled,
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

fn tool_choice_to_api(choice: &ToolChoice) -> api::ToolChoice {
    match choice {
        ToolChoice::Auto => api::ToolChoice::Auto,
        ToolChoice::None => api::ToolChoice::None,
        ToolChoice::RequiredAny => api::ToolChoice::RequiredAny,
        ToolChoice::Specific { tool_name } => api::ToolChoice::Specific {
            tool_id: tool_name.as_str().to_owned(),
        },
    }
}

/// Sparse engine-doc to wire-doc mapping: a section that equals its engine
/// default projects to `None`; engine `Option` fields map 1:1.
pub fn session_config_to_api(config: &SessionConfig) -> Result<api::SessionConfig, AgentApiError> {
    Ok(api::SessionConfig {
        model: Some(model_to_api(&config.model)),
        generation: (!config.generation.is_default())
            .then(|| generation_config_to_api(&config.generation)),
        limits: (!config.limits.is_default()).then_some(api::LimitsConfig {
            max_turns: config.limits.max_turns,
            max_tool_rounds: config.limits.max_tool_rounds,
        }),
        context: (!config.context.is_default()).then(|| api::ContextConfig {
            compaction: config
                .context
                .compaction
                .as_ref()
                .map(compaction_policy_to_api),
        }),
        features: if config.features.is_default() {
            None
        } else {
            Some(features_config_to_api(&config.features)?)
        },
    })
}

fn generation_config_to_api(generation: &engine::GenerationConfig) -> api::GenerationConfig {
    api::GenerationConfig {
        max_output_tokens: generation.max_output_tokens,
        reasoning_effort: generation.reasoning_effort.clone(),
        tool_choice: generation.tool_choice.as_ref().map(tool_choice_to_api),
        parallel_tool_use: generation.parallel_tool_use,
        processing_tier: generation.processing_tier.map(|tier| match tier {
            engine::ModelProcessingTier::Standard => api::ModelProcessingTier::Standard,
            engine::ModelProcessingTier::Fast => api::ModelProcessingTier::Fast,
            engine::ModelProcessingTier::Flex => api::ModelProcessingTier::Flex,
        }),
    }
}

fn features_config_to_api(
    features: &engine::FeaturesConfig,
) -> Result<api::FeaturesConfig, AgentApiError> {
    Ok(api::FeaturesConfig {
        vfs: features.vfs.as_ref().map(vfs_feature_to_api),
        web: features.web.as_ref().map(web_feature_to_api),
        subagents: features
            .subagents
            .as_ref()
            .map(subagents_feature_to_api)
            .transpose()?,
        timers: features.timers.as_ref().map(|timers| api::TimersFeature {
            version: timers.version,
        }),
        environments: features
            .environments
            .as_ref()
            .map(|environments| api::EnvironmentsFeature {
                tools: environments.tools.map(|surface| match surface {
                    engine::EnvironmentToolSurface::ReadOnly => {
                        api::EnvironmentToolSurface::ReadOnly
                    }
                    engine::EnvironmentToolSurface::Edit => api::EnvironmentToolSurface::Edit,
                }),
                commands: environments.commands,
                version: environments.version,
                working_directory: environments.working_directory.clone(),
                prompts: environments.prompts.as_ref().map(|source| {
                    api::EnvironmentPromptsConfig {
                        roots: source.roots.clone(),
                    }
                }),
                providers: environments.providers.clone(),
                registration_keys: environments.registration_keys.clone(),
                selection_tools: environments.selection_tools,
                jobs: environments.jobs,
                skills: environments
                    .skills
                    .as_ref()
                    .map(|skills| api::EnvironmentSkillsConfig {
                        roots: skills.roots.clone(),
                    }),
            }),
        mcp: features.mcp.as_ref().map(mcp_feature_to_api),
    })
}

fn vfs_feature_to_api(vfs: &engine::VfsFeature) -> api::VfsFeature {
    api::VfsFeature {
        version: vfs.version,
        working_directory: vfs.working_directory.clone(),
        workspace_links: vfs
            .workspace_links
            .iter()
            .map(|link| api::WorkspaceLink {
                path: link.path.clone(),
                target: match &link.target {
                    engine::WorkspaceLinkTarget::Workspace { workspace_id } => {
                        api::WorkspaceLinkTarget::Workspace {
                            workspace_id: workspace_id.clone(),
                        }
                    }
                    engine::WorkspaceLinkTarget::Snapshot { snapshot_ref } => {
                        api::WorkspaceLinkTarget::Snapshot {
                            snapshot_ref: snapshot_ref.clone(),
                        }
                    }
                },
                access: match link.access {
                    engine::WorkspaceLinkAccess::ReadOnly => api::WorkspaceLinkAccess::ReadOnly,
                    engine::WorkspaceLinkAccess::ReadWrite => api::WorkspaceLinkAccess::ReadWrite,
                },
            })
            .collect(),
        tools: vfs.tools.map(|tools| match tools {
            engine::VfsToolSurface::ReadOnly => api::VfsToolSurface::ReadOnly,
            engine::VfsToolSurface::Edit => api::VfsToolSurface::Edit,
        }),
        prompts: vfs.prompts.as_ref().map(|prompts| api::VfsPromptsConfig {
            roots: prompts.roots.clone(),
        }),
        skills: vfs.skills.as_ref().map(|skills| api::VfsSkillsConfig {
            roots: skills.roots.clone(),
        }),
    }
}

fn web_feature_to_api(web: &engine::WebFeature) -> api::WebFeature {
    api::WebFeature {
        version: web.version,
        fetch: web.fetch.as_ref().map(|_| api::WebFetchFeature {}),
        search: web.search.as_ref().map(|search| api::WebSearchFeature {
            allowed_domains: search.allowed_domains.clone(),
            blocked_domains: search.blocked_domains.clone(),
        }),
    }
}

fn subagents_feature_to_api(
    subagents: &engine::SubagentsFeature,
) -> Result<api::SubagentsFeature, AgentApiError> {
    Ok(api::SubagentsFeature {
        version: subagents.version,
        agents: subagents
            .agents
            .iter()
            .map(|agent| {
                Ok(api::SubagentAgentRef {
                    profile_id: api::ProfileId::try_new(agent.profile_id.clone()).map_err(
                        |error| {
                            AgentApiError::internal(format!(
                                "invalid subagent profile id {}: {error}",
                                agent.profile_id
                            ))
                        },
                    )?,
                })
            })
            .collect::<Result<Vec<_>, AgentApiError>>()?,
        max_depth: subagents.limits.max_depth,
        max_descendants: subagents.limits.max_descendants,
        max_concurrent: subagents.limits.max_concurrent,
        deadline_ms: subagents.limits.deadline_ms,
    })
}

/// Project a session record's delegation provenance for API views.
pub fn session_origin_to_api(
    origin: &engine::storage::SessionOrigin,
) -> Result<api::SessionOriginView, AgentApiError> {
    Ok(api::SessionOriginView {
        kind: match origin.kind {
            engine::storage::SessionOriginKind::Subagent => api::SessionOriginKind::Subagent,
        },
        parent_session_id: origin.parent_session_id.as_str().to_owned(),
        parent_run_id: format!("run_{}", origin.parent_run_id),
        root_session_id: origin.root_session_id.as_str().to_owned(),
        depth: origin.depth,
        invocation_id: origin.invocation_id.clone(),
        agent: api::SubagentAgentPin {
            profile_id: api::ProfileId::try_new(origin.profile_id.clone()).map_err(|error| {
                AgentApiError::internal(format!(
                    "invalid subagent origin profile id {}: {error}",
                    origin.profile_id
                ))
            })?,
            revision: origin.profile_revision,
        },
        limits: subagent_limits_to_api(origin.limits),
    })
}

pub fn subagent_limits_to_api(limits: engine::SubagentLimits) -> api::SubagentLimitsView {
    api::SubagentLimitsView {
        max_depth: limits.max_depth,
        max_descendants: limits.max_descendants,
        max_concurrent: limits.max_concurrent,
        deadline_ms: limits.deadline_ms,
    }
}

fn mcp_feature_to_api(mcp: &engine::McpFeature) -> api::McpFeature {
    api::McpFeature {
        version: mcp.version,
        servers: mcp
            .servers
            .iter()
            .map(|link| api::McpServerLink {
                server_id: link.server_id.clone(),
            })
            .collect(),
    }
}

fn remote_mcp_approval_to_api(
    policy: &engine::RemoteMcpApprovalPolicy,
) -> api::RemoteMcpApprovalPolicy {
    match policy {
        engine::RemoteMcpApprovalPolicy::Always => api::RemoteMcpApprovalPolicy::Always,
        engine::RemoteMcpApprovalPolicy::Never => api::RemoteMcpApprovalPolicy::Never,
    }
}

fn active_tools_to_api(
    revision: u64,
    tools: &BTreeMap<engine::ToolName, ToolSpec>,
) -> ActiveToolsView {
    ActiveToolsView {
        revision,
        tools: tools.values().map(tool_to_api).collect(),
    }
}

fn tool_to_api(tool: &ToolSpec) -> ToolView {
    ToolView {
        tool_id: tool.name.as_str().to_owned(),
        kind: tool_kind_to_api(&tool.kind),
        parallelism: tool_parallelism_to_api(tool.parallelism),
    }
}

fn tool_kind_to_api(kind: &ToolKind) -> ToolKindView {
    match kind {
        ToolKind::Builtin(builtin) => ToolKindView::Builtin {
            settings: builtin.settings.clone(),
        },
        ToolKind::Function(function) => ToolKindView::Function {
            description_ref: function
                .description_ref
                .as_ref()
                .map(|blob_ref| blob_ref.as_str().to_owned()),
            input_schema_ref: function.input_schema_ref.as_str().to_owned(),
            output_schema_ref: function
                .output_schema_ref
                .as_ref()
                .map(|blob_ref| blob_ref.as_str().to_owned()),
            strict: function.strict,
            provider_options_ref: function
                .provider_options_ref
                .as_ref()
                .map(|blob_ref| blob_ref.as_str().to_owned()),
        },
        ToolKind::ProviderNative(native) => ToolKindView::ProviderNative {
            api_kind: api_kind_to_str(&native.api_kind).to_owned(),
            native_tool_ref: native.native_tool_ref.as_str().to_owned(),
            execution: match native.execution {
                engine::ProviderNativeToolExecution::ProviderHosted => {
                    ProviderNativeToolExecutionView::ProviderHosted
                }
                engine::ProviderNativeToolExecution::ClientEffect => {
                    ProviderNativeToolExecutionView::ClientEffect
                }
            },
        },
        ToolKind::RemoteMcp(remote_mcp) => ToolKindView::RemoteMcp {
            server_id: remote_mcp.server_id.clone(),
            server_label: remote_mcp.server_label.clone(),
            server_url: remote_mcp.server_url.clone(),
            description_ref: remote_mcp
                .description_ref
                .as_ref()
                .map(|blob_ref| blob_ref.as_str().to_owned()),
            allowed_tools: remote_mcp.allowed_tools.clone(),
            approval: remote_mcp_approval_to_api(&remote_mcp.approval),
            defer_loading: remote_mcp.defer_loading,
            auth_required: remote_mcp.auth_required,
        },
    }
}

fn tool_parallelism_to_api(parallelism: ToolParallelism) -> ToolParallelismView {
    match parallelism {
        ToolParallelism::Exclusive => ToolParallelismView::Exclusive,
        ToolParallelism::ParallelSafe => ToolParallelismView::ParallelSafe,
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
    };
    config
        .validate()
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
            content: content_ref_to_api(&entry.content),
            origin: entry.origin.clone(),
            provenance_ref: entry
                .provenance_ref
                .as_ref()
                .map(|reference| reference.as_str().to_owned()),
            preview: entry.preview.clone(),

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
        ContextEntryKind::Catalog { title } => ContextEntryKindView::Catalog {
            title: title.clone(),
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
        ContextEntryKind::McpApprovalResponse { approve, .. } => {
            ContextEntryKindView::McpApprovalResponse { approve: *approve }
        }
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
        SessionStoreError::InvalidRetention { .. } => {
            AgentApiError::invalid_request(error.to_string())
        }
        SessionStoreError::InvalidForkPoint { .. } => {
            AgentApiError::invalid_request(error.to_string())
        }
        SessionStoreError::OriginLimitExceeded { .. } => AgentApiError::rejected(error.to_string()),
        SessionStoreError::SessionNotClosed { .. } => AgentApiError::rejected(error.to_string()),
        SessionStoreError::ManagedSessionCannotBranch { .. } => {
            AgentApiError::rejected(error.to_string())
        }
        SessionStoreError::SessionHasChildren { .. }
        | SessionStoreError::SessionRetentionOwnedBy { .. }
        | SessionStoreError::SessionRetentionNotDue { .. } => {
            AgentApiError::conflict(error.to_string())
        }
        SessionStoreError::SessionTreeNotClosed { .. } => {
            AgentApiError::rejected(error.to_string())
        }
        SessionStoreError::ExpectedHeadMismatch { .. } => {
            AgentApiError::conflict(error.to_string())
        }
        SessionStoreError::MissingBlobs { .. } => AgentApiError::invalid_request(error.to_string()),
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
    if calls
        .iter()
        .any(|call| matches!(call.status, ToolItemStatus::Cancelled))
    {
        return ToolItemStatus::Cancelled;
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

fn tool_effects_for_run(
    projection: &CoreAgentProjection<'_>,
    run_id: RunId,
) -> BTreeMap<String, Vec<ToolEffectView>> {
    let mut effects = BTreeMap::new();
    for entry in projection.entries() {
        let CoreAgentEvent::Tool(ToolEvent::CallCompleted {
            run_id: event_run_id,
            result,
            ..
        }) = &entry.event
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
        .filter(|effect| effect.kind != engine::WORKFLOW_TOOL_EMIT_EFFECT_KIND)
        .map(|effect| ToolEffectView {
            kind: effect.kind.clone(),
            data: effect.data.clone(),
        })
        .collect()
}

fn openai_mcp_call_display(value: &Value) -> Option<ProviderContextDisplayView> {
    if value.get("type").and_then(Value::as_str) != Some("mcp_call") {
        return None;
    }

    let name = json_field_text(value, "name")?;
    let server_label = json_field_text(value, "server_label");
    let tool_name = match server_label.as_deref() {
        Some(server_label) if !server_label.is_empty() => format!("{server_label}.{name}"),
        _ => name.clone(),
    };
    let raw_status = value.get("status").and_then(Value::as_str);
    let error = json_field_text(value, "error");
    let is_error = error.is_some() || matches!(raw_status, Some("failed" | "incomplete"));
    let status = match raw_status {
        Some("in_progress" | "running" | "queued") => ToolItemStatus::Running,
        Some("failed" | "incomplete") => ToolItemStatus::Failed,
        _ if is_error => ToolItemStatus::Failed,
        _ => ToolItemStatus::Succeeded,
    };
    let detail = match raw_status {
        Some("completed") | None if !is_error => None,
        Some(status) => Some(status.to_owned()),
        None => Some("failed".to_owned()),
    };

    Some(ProviderContextDisplayView {
        summary: ToolCallDisplayView {
            group: ToolCallDisplayGroup::Mcp,
            verb: server_label
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| "MCP".to_owned()),
            target: Some(name),
            detail,
        },
        tool_name,
        status,
        is_error,
        arguments: json_field_text(value, "arguments"),
        output: json_field_text(value, "output"),
        error,
    })
}

/// A provider-hosted OpenAI web search shown as a tool step, so a transcript
/// shows the search between the text it interrupts.
fn openai_web_search_call_display(value: &Value) -> Option<ProviderContextDisplayView> {
    if value.get("type").and_then(Value::as_str) != Some("web_search_call") {
        return None;
    }
    let action = value.get("action");
    let target = action
        .and_then(|action| action.get("query").or_else(|| action.get("url")))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let raw_status = value.get("status").and_then(Value::as_str);
    let status = match raw_status {
        Some("in_progress" | "searching" | "queued") => ToolItemStatus::Running,
        Some("failed" | "incomplete") => ToolItemStatus::Failed,
        _ => ToolItemStatus::Succeeded,
    };
    let is_error = status == ToolItemStatus::Failed;
    Some(ProviderContextDisplayView {
        summary: ToolCallDisplayView {
            group: ToolCallDisplayGroup::Explore,
            verb: "Search".to_owned(),
            target,
            detail: is_error.then(|| raw_status.unwrap_or("failed").to_owned()),
        },
        tool_name: "web_search".to_owned(),
        status,
        is_error,
        arguments: action.map(Value::to_string),
        output: None,
        error: None,
    })
}

/// An Anthropic server tool call (`web_search`, `web_fetch`) shown as a tool
/// step. Its result block is a separate entry that stays hidden: the payload
/// is encrypted provider state, and what the model cited is on the message.
fn anthropic_server_tool_use_display(value: &Value) -> Option<ProviderContextDisplayView> {
    if value.get("type").and_then(Value::as_str) != Some("server_tool_use") {
        return None;
    }
    let tool_name = json_field_text(value, "name")?;
    let input = value.get("input");
    let target = input
        .and_then(|input| input.get("query").or_else(|| input.get("url")))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let verb = match tool_name.as_str() {
        "web_search" => "Search",
        "web_fetch" => "Fetch",
        _ => "Run",
    };
    Some(ProviderContextDisplayView {
        summary: ToolCallDisplayView {
            group: ToolCallDisplayGroup::Explore,
            verb: verb.to_owned(),
            target,
            detail: None,
        },
        tool_name,
        status: ToolItemStatus::Succeeded,
        is_error: false,
        arguments: input.map(Value::to_string),
        output: None,
        error: None,
    })
}

fn json_field_text(value: &Value, field: &str) -> Option<String> {
    let text = match value.get(field)? {
        Value::Null => return None,
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).ok()?,
    };
    (!text.is_empty()).then_some(text)
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
        "mcp_find_tools" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Explore,
            verb: json
                .as_ref()
                .and_then(|json| json.get("names"))
                .and_then(Value::as_array)
                .map_or_else(
                    || "Search MCP tools".to_owned(),
                    |_| "Load MCP tool definitions".to_owned(),
                ),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["server"])),
            detail: json.as_ref().and_then(|json| {
                json.get("names")
                    .and_then(Value::as_array)
                    .map(|names| names.iter().filter_map(json_text).collect::<Vec<_>>())
                    .filter(|names| !names.is_empty())
                    .map(|names| names.join(", "))
                    .or_else(|| first_string(json, &["query"]))
            }),
        },
        // Same sentence as a provider-native MCP block: the server is the
        // verb, the tool the target, so both paths read alike.
        "mcp_call" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Mcp,
            verb: json
                .as_ref()
                .and_then(|json| first_string(json, &["server"]))
                .unwrap_or_else(|| "MCP".to_owned()),
            target: json.as_ref().and_then(|json| first_string(json, &["tool"])),
            detail: json
                .as_ref()
                .and_then(|json| json.get("arguments"))
                .and_then(first_scalar_text),
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
        "exec_command" | "bash" | "Bash" | "run_process" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Execute,
            verb: "Run".to_owned(),
            target: json.as_ref().and_then(command_display),
            detail: json
                .as_ref()
                .and_then(|json| first_string(json, &["cwd", "workdir"]))
                .map(|cwd| format!("in {cwd}")),
        },
        "write_stdin" | "continue_process" | "BashOutput" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Execute,
            verb: "Continue process".to_owned(),
            target: json.as_ref().and_then(|json| {
                first_string(
                    json,
                    &["session_id", "handle", "bash_id", "process_id", "id"],
                )
            }),
            detail: None,
        },
        "KillShell" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Execute,
            verb: "Stop process".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["shell_id", "handle", "id"])),
            detail: None,
        },
        "agent_run" | "agent_spawn" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Agent,
            verb: if normalized == "agent_run" {
                "Delegate"
            } else {
                "Spawn"
            }
            .to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["agent"])),
            detail: json.as_ref().and_then(|json| {
                first_string(json, &["label"])
                    .or_else(|| first_string(json, &["input"]).map(|input| summary_line(&input)))
            }),
        },
        "await" | "cancel" | "detach" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Agent,
            verb: match normalized.as_str() {
                "await" => "Await",
                "cancel" => "Cancel",
                _ => "Detach",
            }
            .to_owned(),
            target: json.as_ref().and_then(|json| string_list(json, "promises")),
            detail: json.as_ref().and_then(|json| {
                let count = json
                    .get("promises")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                let any = first_string(json, &["mode"]).is_some_and(|mode| mode == "any");
                match (count, any) {
                    (0 | 1, false) => None,
                    (0 | 1, true) => Some("any".to_owned()),
                    (count, false) => Some(format!("{count} promises")),
                    (count, true) => Some(format!("{count} promises · any")),
                }
            }),
        },
        "bot_emit" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Bot,
            verb: "Emit".to_owned(),
            target: Some(
                match json.as_ref().and_then(|json| first_string(json, &["to"])) {
                    Some(to) => format!("to {to}"),
                    None => "to self".to_owned(),
                },
            ),
            detail: json.as_ref().and_then(|json| {
                let mut parts = first_string(json, &["kind"])
                    .into_iter()
                    .collect::<Vec<_>>();
                if json.get("reply").and_then(Value::as_bool) == Some(true) {
                    parts.push("reply requested".to_owned());
                }
                if let Some(key) = first_string(json, &["sessionKey"]) {
                    parts.push(format!("key {key}"));
                }
                (!parts.is_empty()).then(|| parts.join(" · "))
            }),
        },
        "bot_event_resolve" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Bot,
            verb: "Resolve".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["outcome"])),
            detail: json
                .as_ref()
                .and_then(|json| first_string(json, &["summary"]))
                .map(|summary| summary_line(&summary)),
        },
        "bot_event_read" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Bot,
            verb: "Read event".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["seq"]))
                .map(|seq| format!("#{seq}")),
            detail: json.as_ref().and_then(|json| first_string(json, &["path"])),
        },
        "bot_event_list" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Bot,
            verb: "List events".to_owned(),
            target: None,
            detail: None,
        },
        "bot_status" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Bot,
            verb: "Check status".to_owned(),
            target: None,
            detail: None,
        },
        "bot_trigger_put" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Bot,
            verb: "Set trigger".to_owned(),
            target: json.as_ref().and_then(|json| first_string(json, &["name"])),
            detail: json.as_ref().and_then(|json| first_string(json, &["kind"])),
        },
        "bot_trigger_delete" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Bot,
            verb: "Remove trigger".to_owned(),
            target: json.as_ref().and_then(|json| first_string(json, &["name"])),
            detail: None,
        },
        "bot_trigger_list" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Bot,
            verb: "List triggers".to_owned(),
            target: None,
            detail: None,
        },
        "bot_filter_test" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Bot,
            verb: "Test filter".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["filter"])),
            detail: None,
        },
        "bot_brief_put" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Bot,
            verb: "Update brief".to_owned(),
            target: None,
            detail: json
                .as_ref()
                .and_then(|json| first_string(json, &["brief"]))
                .map(|brief| summary_line(&brief)),
        },
        "message_send" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Message,
            verb: "Send".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["text"]))
                .map(|text| summary_line(&text)),
            detail: json
                .as_ref()
                .and_then(|json| first_string(json, &["replyTo", "reply_to"]))
                .map(|reply| format!("reply to #{reply}")),
        },
        "message_noop" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Message,
            verb: "Skip".to_owned(),
            target: None,
            detail: json
                .as_ref()
                .and_then(|json| first_string(json, &["reason"]))
                .map(|reason| summary_line(&reason)),
        },
        "sleep" => ToolCallDisplayView {
            group: ToolCallDisplayGroup::Other,
            verb: "Sleep".to_owned(),
            target: json
                .as_ref()
                .and_then(|json| first_string(json, &["ms"]))
                .map(|ms| format!("{ms} ms")),
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

fn media_kind_for_mime(mime: &str) -> MediaKind {
    let mime = mime.trim().to_ascii_lowercase();
    if mime.starts_with("image/") {
        MediaKind::Image
    } else if mime.starts_with("audio/") {
        MediaKind::Audio
    } else {
        MediaKind::Document
    }
}

fn is_text_message_media_type(media_type: Option<&str>) -> bool {
    match media_type {
        None => true,
        Some(media_type) => {
            let media_type = media_type.trim().to_ascii_lowercase();
            media_type.starts_with("text/")
                || media_type == "application/json"
                || media_type.is_empty()
        }
    }
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

/// The first non-empty line of a longer text, cut to a readable width so it
/// fits a one-line activity row.
fn summary_line(text: &str) -> String {
    const MAX_CHARS: usize = 80;
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if line.chars().count() <= MAX_CHARS {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(MAX_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// Comma-joined string items of an array field.
fn string_list(json: &Value, key: &str) -> Option<String> {
    let items = json
        .get(key)?
        .as_array()?
        .iter()
        .filter_map(json_text)
        .collect::<Vec<_>>();
    (!items.is_empty()).then(|| items.join(", "))
}

/// The first scalar value inside an object, as a short hint of what a
/// generic call was about (an id, a query, a path).
fn first_scalar_text(value: &Value) -> Option<String> {
    value.as_object()?.values().find_map(|value| match value {
        Value::String(text) if !text.trim().is_empty() => Some(summary_line(text)),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    })
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

/// Map each superseded catalog version to the newer entry that updated it.
fn superseded_by_map(entries: &[&ContextEntry]) -> BTreeMap<ContextEntryId, ContextEntryId> {
    entries
        .iter()
        .filter_map(|entry| entry.supersedes.map(|older| (older, entry.entry_id)))
        .collect()
}

/// Project provider usage. Every adapter already reports `input_tokens` as
/// the whole prompt (the Anthropic adapter folds its separately reported
/// cache read/write counts in and keeps the uncached count in
/// `cache_miss_input_tokens`), so this is a field copy.
fn llm_usage_to_api(usage: &engine::LlmUsage) -> LlmUsageView {
    LlmUsageView {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        total_tokens: usage.total_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_write_input_tokens: usage.cache_write_input_tokens,
    }
}

#[cfg(test)]
mod tests {
    use engine::{
        BlobRef, ContextEntryId, CoreAgentJoins, EventSeq, SessionPosition, TokenEstimate,
        TokenEstimateQuality,
        storage::{BlobStore, InMemoryBlobStore},
    };

    use super::*;

    fn tool_call_with_status(status: ToolItemStatus) -> ToolCallView {
        ToolCallView {
            tool_id: None,
            call_id: "call-1".to_owned(),
            tool_name: "read_file".to_owned(),
            arguments_ref: "sha256:args".to_owned(),
            arguments: None,
            output: None,
            is_error: false,
            status,
            effects: Vec::new(),
            display: None,
            started_at_ms: None,
            completed_at_ms: None,
            duration_ms: None,
        }
    }

    #[test]
    fn native_mcp_search_call_displays_the_resolved_server_and_tool() {
        let display = tool_call_display(
            "mcp_call",
            r#"{"server":"configurator","tool":"lightspeed_models_list","arguments":{}}"#,
        )
        .expect("display");
        assert_eq!(display.group, ToolCallDisplayGroup::Mcp);
        assert_eq!(display.verb, "configurator");
        assert_eq!(display.target.as_deref(), Some("lightspeed_models_list"));
        assert_eq!(display.detail, None);

        let display = tool_call_display(
            "mcp_call",
            r#"{"server":"stripe","tool":"retrieve_subscription","arguments":{"id":"sub_1R0aB3"}}"#,
        )
        .expect("display");
        assert_eq!(display.detail.as_deref(), Some("sub_1R0aB3"));
    }

    #[test]
    fn provider_native_mcp_call_reads_like_the_builtin_bridge() {
        let display = openai_mcp_call_display(&serde_json::json!({
            "type": "mcp_call",
            "name": "retrieve_subscription",
            "server_label": "stripe",
            "status": "completed",
        }))
        .expect("display");
        assert_eq!(display.summary.group, ToolCallDisplayGroup::Mcp);
        assert_eq!(display.summary.verb, "stripe");
        assert_eq!(
            display.summary.target.as_deref(),
            Some("retrieve_subscription")
        );
        assert_eq!(display.tool_name, "stripe.retrieve_subscription");
    }

    #[test]
    fn subagent_calls_display_the_profile_and_the_brief() {
        let display = tool_call_display(
            "agent_run",
            r#"{"agent":"reviewer","input":"Review the diff on skills2.\n\nFocus on tests."}"#,
        )
        .expect("display");
        assert_eq!(display.group, ToolCallDisplayGroup::Agent);
        assert_eq!(display.verb, "Delegate");
        assert_eq!(display.target.as_deref(), Some("reviewer"));
        assert_eq!(
            display.detail.as_deref(),
            Some("Review the diff on skills2.")
        );

        let display = tool_call_display(
            "agent_spawn",
            r#"{"agent":"researcher","input":"Find prior art","label":"Prior art"}"#,
        )
        .expect("display");
        assert_eq!(display.verb, "Spawn");
        assert_eq!(display.detail.as_deref(), Some("Prior art"));

        let display = tool_call_display(
            "await",
            r#"{"promises":["promise_3","promise_4"],"mode":"any"}"#,
        )
        .expect("display");
        assert_eq!(display.group, ToolCallDisplayGroup::Agent);
        assert_eq!(display.verb, "Await");
        assert_eq!(display.target.as_deref(), Some("promise_3, promise_4"));
        assert_eq!(display.detail.as_deref(), Some("2 promises · any"));
    }

    #[test]
    fn bot_emit_distinguishes_self_from_addressed_peers() {
        let display = tool_call_display(
            "bot_emit",
            r#"{"kind":"bug.confirmed","summary":"Duplicates on page boundary","to":"escalations","reply":true}"#,
        )
        .expect("display");
        assert_eq!(display.group, ToolCallDisplayGroup::Bot);
        assert_eq!(display.verb, "Emit");
        assert_eq!(display.target.as_deref(), Some("to escalations"));
        assert_eq!(
            display.detail.as_deref(),
            Some("bug.confirmed · reply requested")
        );

        let display = tool_call_display(
            "bot_emit",
            r#"{"kind":"research.followup","summary":"Nightly","sessionKey":"nightly"}"#,
        )
        .expect("display");
        assert_eq!(display.target.as_deref(), Some("to self"));
        assert_eq!(
            display.detail.as_deref(),
            Some("research.followup · key nightly")
        );
    }

    #[test]
    fn channel_message_tools_display_as_messages() {
        let display = tool_call_display(
            "message_send",
            r#"{"text":"On it — checking the logs now.\nMore soon.","replyTo":12}"#,
        )
        .expect("display");
        assert_eq!(display.group, ToolCallDisplayGroup::Message);
        assert_eq!(display.verb, "Send");
        assert_eq!(
            display.target.as_deref(),
            Some("On it — checking the logs now.")
        );
        assert_eq!(display.detail.as_deref(), Some("reply to #12"));

        let display =
            tool_call_display("message_noop", r#"{"reason":"already answered"}"#).expect("display");
        assert_eq!(display.verb, "Skip");
        assert_eq!(display.detail.as_deref(), Some("already answered"));
    }

    #[test]
    fn summary_line_takes_the_first_line_and_cuts_on_a_char_boundary() {
        assert_eq!(summary_line("\n  first 🦀 line \nsecond"), "first 🦀 line");
        let long = "é".repeat(100);
        let cut = summary_line(&long);
        assert_eq!(cut.chars().count(), 80);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn native_mcp_detail_display_names_the_requested_tools() {
        let display = tool_call_display(
            "mcp_find_tools",
            r#"{"server":"configurator","names":["models_list","sessions_read"]}"#,
        )
        .expect("display");
        assert_eq!(display.verb, "Load MCP tool definitions");
        assert_eq!(display.target.as_deref(), Some("configurator"));
        assert_eq!(
            display.detail.as_deref(),
            Some("models_list, sessions_read")
        );
    }

    #[test]
    fn cancelled_tool_status_is_preserved_and_aggregated_neutrally() {
        assert_eq!(
            core_tool_status_to_api_status(ToolCallStatus::Cancelled),
            ToolItemStatus::Cancelled
        );
        assert_eq!(
            aggregate_api_tool_status(&[
                tool_call_with_status(ToolItemStatus::Succeeded),
                tool_call_with_status(ToolItemStatus::Cancelled),
            ]),
            ToolItemStatus::Cancelled
        );
        assert_eq!(
            aggregate_api_tool_status(&[
                tool_call_with_status(ToolItemStatus::Cancelled),
                tool_call_with_status(ToolItemStatus::Running),
            ]),
            ToolItemStatus::Running
        );
        assert_eq!(
            aggregate_api_tool_status(&[
                tool_call_with_status(ToolItemStatus::Cancelled),
                tool_call_with_status(ToolItemStatus::Failed),
            ]),
            ToolItemStatus::Failed
        );
    }

    #[test]
    fn managed_session_projection_exposes_controller_ownership() {
        let mut state = CoreAgentState::new();
        state.workflow_tools.managed_declaration_version = Some(1);
        state.workflow_tools.lifecycle_controller = Some(engine::WorkflowEndpointRef {
            workflow_id: "channels/session-1".to_owned(),
            workflow_kind: "channelSessionWorkflowV1".to_owned(),
        });

        assert_eq!(
            session_management_to_api(&state),
            Some(ManagedSessionWorkflowToolsInput {
                version: 1,
                lifecycle_controller: Some(WorkflowEndpointInput {
                    workflow_id: "channels/session-1".to_owned(),
                    workflow_kind: "channelSessionWorkflowV1".to_owned(),
                }),
                tools: Vec::new(),
            })
        );
        assert_eq!(session_management_to_api(&CoreAgentState::new()), None);
    }

    #[test]
    fn managed_session_projection_reuses_the_joined_declaration_contract() {
        let mut state = CoreAgentState::new();
        state.workflow_tools.managed_declaration_version = Some(1);
        let universe_id = "00000000-0000-0000-0000-000000000001"
            .parse()
            .expect("universe id");
        let receiver = engine::WorkflowEndpointRef {
            workflow_id: "channels/session-1".to_owned(),
            workflow_kind: "channels.session".to_owned(),
        };
        let input_schema_ref = BlobRef::from_bytes(br#"{"type":"object"}"#);
        let binding = engine::WorkflowToolBinding::admit(
            universe_id,
            engine::WorkflowToolDefinition {
                tool_id: engine::WorkflowToolId::new("message-send"),
                revision: 1,
                semantic_type: "channels.message.send.v1".to_owned(),
                tool: ToolSpec {
                    name: engine::ToolName::new("message_send"),
                    execution: Default::default(),
                    kind: ToolKind::Function(engine::FunctionToolSpec {
                        description_ref: None,
                        input_schema_ref,
                        output_schema_ref: None,
                        strict: Some(true),
                        provider_options_ref: None,
                    }),
                    parallelism: ToolParallelism::ParallelSafe,
                },
            },
            engine::WorkflowToolTarget::Bound {
                receiver: receiver.clone(),
                dispatch: engine::BoundWorkflowToolDispatch::Push,
            },
            engine::WorkflowToolCompletion::Joined {
                reply_schema_ref: None,
                deadline_after_ms: 30_000,
            },
        )
        .expect("joined binding");
        let mut system_definition = binding.definition.clone();
        system_definition.tool_id = engine::WorkflowToolId::new("subagent-run");
        system_definition.semantic_type = "lightspeed.subagent.run.v1".to_owned();
        system_definition.tool.name = engine::ToolName::new("subagent.run");
        system_definition.tool.kind = ToolKind::Builtin(Default::default());
        let system_binding = engine::WorkflowToolBinding::admit(
            universe_id,
            system_definition,
            binding.target.clone(),
            binding.completion.clone(),
        )
        .expect("system builtin binding");
        state
            .workflow_tools
            .system_binding_ids
            .insert(system_binding.definition.tool_id.clone());
        state
            .workflow_tools
            .bindings
            .insert(system_binding.definition.tool_id.clone(), system_binding);
        state
            .workflow_tools
            .bindings
            .insert(binding.definition.tool_id.clone(), binding);

        let projected = session_management_to_api(&state).expect("managed projection");
        assert_eq!(projected.tools.len(), 1);
        assert_eq!(
            projected.tools[0].target,
            WorkflowToolTargetInput::Bound {
                receiver: WorkflowEndpointInput {
                    workflow_id: receiver.workflow_id,
                    workflow_kind: receiver.workflow_kind,
                },
                dispatch: BoundWorkflowToolDispatchInput::Push,
            }
        );
        assert_eq!(
            projected.tools[0].completion,
            WorkflowToolCompletionInput::Joined {
                reply_schema_ref: None,
                deadline_after_ms: 30_000,
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_content_full_text_is_projected_independently_of_preview() {
        let blobs = InMemoryBlobStore::new();
        let full = "héllo ".repeat(2000);
        let raw = serde_json::json!([
            {"type": "text", "text": &full[..7000]},
            {"type": "text", "text": &full[7000..], "citations": []}
        ]);
        let mut entry =
            json_entry(&blobs, 1, ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND, raw).await;
        entry.kind = ContextEntryKind::Message {
            role: ContextMessageRole::Assistant,
        };
        entry.preview = Some("not the answer".to_owned());
        let content = entry.content.clone();
        let projected = CoreAgentProjector::new(&blobs)
            .project_context_entry(&entry, None)
            .await
            .expect("project message");
        assert!(!projected.text_truncated);
        assert_eq!(projected.text.as_deref(), Some(full.as_str()));
        assert_eq!(
            project_content_text(&blobs, &content)
                .await
                .expect("full text"),
            Some(full)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn content_projection_distinguishes_authored_json_native_json_and_media() {
        let blobs = InMemoryBlobStore::new();
        let authored = engine::ContentRef::text(blobs.insert_text("{\"answer\":42}").await);
        assert_eq!(
            project_content_text(&blobs, &authored)
                .await
                .expect("authored JSON"),
            Some("{\"answer\":42}".to_owned())
        );
        let mut content = engine::ContentRef {
            content_ref: blobs.insert_text("{\"answer\":42}").await,
            media_type: Some("application/json".to_owned()),
            provider_kind: Some(ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND.to_owned()),
        };
        assert!(
            project_content_text(&blobs, &content).await.is_err(),
            "malformed recognized payload must fail"
        );
        content.provider_kind = Some("unknown.native_message".to_owned());
        assert!(
            project_content_text(&blobs, &content).await.is_err(),
            "unknown provider JSON must not leak as display text"
        );
        content.media_type = Some("image/png".to_owned());
        assert_eq!(
            project_content_text(&blobs, &content)
                .await
                .expect("binary media"),
            None
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_summaries_page_newest_first_with_explicit_continuation() {
        let blobs = InMemoryBlobStore::new();
        let projector = CoreAgentProjector::new(&blobs);
        let mut state = CoreAgentState::new();
        for id in 1..=3 {
            state.runs.completed.push(engine::RunRecord {
                run_id: RunId::new(id),
                status: RunStatus::Completed,
                submission_id: None,
                submission_digest: None,
                source: RunSource::Input { input: Vec::new() },
                first_seq: EventSeq::new(id * 2 - 1),
                terminal_seq: EventSeq::new(id * 2),
                accepted_at_ms: id,
                started_at_ms: Some(id + 10),
                completed_at_ms: id + 20,
                usage: None,
                output: None,
                failure: None,
                notify_on_terminal: Vec::new(),
            });
        }

        let (first, next, has_older) = projector
            .project_run_summaries(&state, None, 2)
            .await
            .expect("first page");
        assert_eq!(
            first.iter().map(|run| run.id.as_str()).collect::<Vec<_>>(),
            vec!["run_3", "run_2"]
        );
        assert!(has_older);
        assert_eq!(next.as_deref(), Some("run_2"));

        let (last, next, has_older) = projector
            .project_run_summaries(&state, Some(RunId::new(2)), 2)
            .await
            .expect("last page");
        assert_eq!(last[0].id, "run_1");
        assert!(!has_older);
        assert!(next.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detailed_session_projects_managed_catalog_flag() {
        let blobs = InMemoryBlobStore::new();
        let projector = CoreAgentProjector::new(&blobs);
        let session_id = SessionId::new("managed-session");
        let state = CoreAgentState::new();
        let record = SessionRecord {
            metadata: Default::default(),
            session_id: session_id.clone(),
            display_name: None,
            lifecycle_status: engine::storage::SessionLifecycleStatus::New,
            closed_at_seq: None,
            closed_at_ms: None,
            retention_root_session_id: session_id.clone(),
            delete_after_close_ms: None,
            delete_at_ms: None,
            managed: true,
            head: None,
            source_session_id: None,
            source_seq: None,
            origin: None,
            created_at_ms: 1,
            updated_at_ms: 2,
        };
        let retention = SessionRetentionView {
            root_session_id: session_id.as_str().to_owned(),
            delete_after_close_ms: None,
            delete_at_ms: None,
        };

        let session = projector
            .project_session(ProjectSession {
                session_id: &session_id,
                state: &state,
                record: &record,
                retention: &retention,
                run_limit: 20,
                run_cursor: None,
            })
            .await
            .expect("project detailed managed session");

        assert!(session.0.managed);
    }

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
            content: engine::ContentRef::text(blob_ref.clone()),
            preview: Some("hello".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }]);

        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].content.content_ref, blob_ref.as_str());
        assert_eq!(
            projected[0].content.media_type.as_deref(),
            Some("text/plain")
        );
        assert!(matches!(
            projected[0].kind,
            ContextEntryKindView::Message {
                role: ContextMessageRoleView::User
            }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn input_projection_renders_binary_media_as_media_item() {
        let blobs = InMemoryBlobStore::new();
        let image_ref = blobs
            .put_bytes(vec![0xff, 0xd8, 0xff, 0xe0])
            .await
            .expect("store image bytes");
        let projector = CoreAgentProjector::new(&blobs);

        let projected = projector
            .project_input_entries(&[ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                content: engine::ContentRef {
                    content_ref: image_ref.clone(),
                    media_type: Some("image/jpeg".to_owned()),
                    provider_kind: None,
                },
                preview: Some("[image: photo.jpg]".to_owned()),
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            }])
            .await
            .expect("project media input");

        assert_eq!(
            projected,
            vec![InputItem::Media {
                origin: None,
                blob_ref: image_ref.as_str().to_owned(),
                mime: "image/jpeg".to_owned(),
                kind: MediaKind::Image,
                name: None,
            }]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_compaction_context_events_project_reason() {
        let blobs = InMemoryBlobStore::new();
        let projector = CoreAgentProjector::new(&blobs);

        let removed = projector
            .project_event_kind(&CoreAgentEvent::Context(ContextEvent::EntriesRemoved {
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
                entry_ids: vec!["item_11".to_owned(), "item_12".to_owned()],
                reason: "providerCompacted".to_owned(),
            }
        );

        let replaced = projector
            .project_event_kind(&CoreAgentEvent::Context(ContextEvent::StateReplaced {
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
                entries: Vec::new(),
                reason: "providerCompacted".to_owned(),
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn managed_port_config_projects_bounded_diagnostics() {
        let blobs = InMemoryBlobStore::new();
        let projector = CoreAgentProjector::new(&blobs);
        let universe_id = "00000000-0000-0000-0000-000000000001";
        let config_event: engine::WorkflowToolConfigEvent =
            serde_json::from_value(serde_json::json!({
                "managed_bindings_admitted": {
                    "session_universe_id": universe_id,
                    "declaration_version": 1,
                    "lifecycle_controller": {
                        "workflow_id": "global controller/work-1",
                        "workflow_kind": "agent_work",
                    },
                    "creation_fingerprint": "msc:sha256:test",
                    "bindings": [],
                }
            }))
            .expect("decode workflow tool config event");
        let projected = projector
            .project_event_kind(&CoreAgentEvent::WorkflowToolConfig(config_event))
            .await
            .expect("project managed-session tools");

        assert_eq!(
            projected,
            SessionEventKindView::WorkflowToolsConfigured {
                lifecycle_controller_workflow_kind: Some("agent_work".to_owned()),
                creation_fingerprint: "msc:sha256:test".to_owned(),
                tool_ids: Vec::new(),
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workflow_tool_events_project_refs_without_inlining_arguments() {
        let blobs = InMemoryBlobStore::new();
        let projector = CoreAgentProjector::new(&blobs);
        let arguments_ref = BlobRef::from_bytes(br#"{"status":"complete"}"#);
        let error_ref = BlobRef::from_bytes(b"delivery failed");
        let invocation_id = format!("wti:sha256:{}", "a".repeat(64));
        let emitted: engine::WorkflowToolEvent = serde_json::from_value(serde_json::json!({
            "emitted": {
                "invocation": {
                    "invocation_id": invocation_id,
                    "tool_id": "report",
                    "semantic_type": "lightspeed.work.report.v1",
                    "schema_revision": 1,
                    "binding_fingerprint": "wtb:sha256:test",
                    "session_universe_id": "00000000-0000-0000-0000-000000000001",
                    "session_id": "session-1",
                    "run_id": 2,
                    "turn_id": 3,
                    "tool_batch_id": 4,
                    "tool_call_id": "call-5",
                    "arguments_ref": arguments_ref,
                }
            }
        }))
        .expect("decode emitted event");
        let projected = projector
            .project_event_kind(&CoreAgentEvent::WorkflowTool(emitted))
            .await
            .expect("project emitted event");
        assert_eq!(
            projected,
            SessionEventKindView::WorkflowToolEmitted {
                invocation_id: invocation_id.clone(),
                tool_id: "report".to_owned(),
                semantic_type: "lightspeed.work.report.v1".to_owned(),
                schema_revision: 1,
                binding_fingerprint: "wtb:sha256:test".to_owned(),
                run_id: api_run_id(RunId::new(2)),
                turn_id: "turn_3".to_owned(),
                batch_id: "tool_batch_4".to_owned(),
                call_id: "call-5".to_owned(),
                arguments_ref: arguments_ref.as_str().to_owned(),
                completion_promises: None,
            }
        );

        let failed = engine::WorkflowToolEvent::DeliveryFailed {
            invocation_id: engine::WorkflowToolInvocationId::new(invocation_id.clone()),
            error_ref: error_ref.clone(),
        };
        let projected = projector
            .project_event_kind(&CoreAgentEvent::WorkflowTool(failed))
            .await
            .expect("project failed delivery");
        assert_eq!(
            projected,
            SessionEventKindView::WorkflowToolDeliveryFailed {
                invocation_id,
                error_ref: error_ref.as_str().to_owned(),
            }
        );

        let completed = projector
            .project_event_kind(&CoreAgentEvent::Tool(ToolEvent::CallCompleted {
                run_id: RunId::new(2),
                turn_id: TurnId::new(3),
                batch_id: ToolBatchId::new(4),
                result: engine::ToolCallResult {
                    duration_ms: None,
                    output_bytes: None,
                    truncated: false,
                    call_id: engine::ToolCallId::new("call-5"),
                    status: engine::ToolCallStatus::Succeeded,
                    output_ref: None,
                    model_visible_context_entries: Vec::new(),
                    error_ref: None,
                    effects: vec![engine::ToolEffect {
                        kind: engine::WORKFLOW_TOOL_EMIT_EFFECT_KIND.to_owned(),
                        data: Default::default(),
                    }],
                },
            }))
            .await
            .expect("project tool completion");
        assert!(matches!(
            completed,
            SessionEventKindView::ToolCallCompleted { effects, .. } if effects.is_empty()
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_context_item_exposes_debug_metadata() {
        let blobs = InMemoryBlobStore::new();
        let projector = CoreAgentProjector::new(&blobs);
        blobs
            .put_bytes(br#"{"type":"compaction","id":"item_compaction_1"}"#.to_vec())
            .await
            .expect("store native item");
        let item = ContextEntry {
            entry_id: ContextEntryId::new(42),
            key: None,
            kind: ContextEntryKind::ProviderOpaque,
            source: ContextEntrySource::AssistantOutput {
                run_id: RunId::new(7),
                turn_id: TurnId::new(8),
            },
            content: engine::ContentRef {
                content_ref: BlobRef::from_bytes(
                    br#"{"type":"compaction","id":"item_compaction_1"}"#,
                ),
                media_type: Some("application/json".to_owned()),
                provider_kind: Some("openai.responses.compaction".to_owned()),
            },
            preview: Some("OpenAI Responses compaction item".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: Some(TokenEstimate {
                tokens: 123,
                quality: TokenEstimateQuality::ProviderCounted,
            }),
            supersedes: None,
        };

        let projected = projector
            .project_context_entry(&item, None)
            .await
            .expect("project provider context entry");

        assert_eq!(
            projected,
            ContextEntryView {
                id: "item_42".to_owned(),
                key: None,
                kind: ContextEntryKindView::ProviderOpaque,
                content: api::ContentRefView {
                    content_ref: BlobRef::from_bytes(
                        br#"{"type":"compaction","id":"item_compaction_1"}"#
                    )
                    .as_str()
                    .to_owned(),
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some("openai.responses.compaction".to_owned())
                },
                origin: None,
                provenance_ref: None,
                preview: Some("OpenAI Responses compaction item".to_owned()),
                provider_item_id: Some("item_compaction_1".to_owned()),
                token_estimate: Some(TokenEstimateView {
                    tokens: 123,
                    quality: TokenEstimateQualityView::ProviderCounted,
                }),
                text: None,
                text_truncated: false,
                display: None,
                citations: Vec::new(),
                source: Some(ContextEntrySourceView::AssistantOutput {
                    run_id: "run_7".to_owned(),
                    turn_id: "turn_8".to_owned(),
                }),
                supersedes: None,
                superseded_by: None,
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_context_item_projects_mcp_call_display() {
        let blobs = InMemoryBlobStore::new();
        let content_ref = blobs
            .put_bytes(
                br#"{"id":"mcp_1","type":"mcp_call","server_label":"echo","name":"echo","arguments":"{\"data\":\"simba\"}","output":"Echoing your input: simba","error":null,"status":"completed"}"#
                    .to_vec(),
            )
            .await
            .expect("store mcp call");
        let projector = CoreAgentProjector::new(&blobs);
        let item = ContextEntry {
            entry_id: ContextEntryId::new(43),
            key: None,
            kind: ContextEntryKind::ProviderOpaque,
            source: ContextEntrySource::AssistantOutput {
                run_id: RunId::new(7),
                turn_id: TurnId::new(8),
            },
            content: engine::ContentRef {
                content_ref: content_ref.clone(),
                media_type: Some("application/json".to_owned()),
                provider_kind: Some(OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND.to_owned()),
            },
            preview: Some("OpenAI Responses MCP tool call: echo.echo".to_owned()),
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        };

        let projected = projector
            .project_context_entry(&item, None)
            .await
            .expect("project mcp provider context entry");

        assert_eq!(
            projected,
            ContextEntryView {
                id: "item_43".to_owned(),
                key: None,
                kind: ContextEntryKindView::ProviderOpaque,
                content: api::ContentRefView {
                    content_ref: content_ref.as_str().to_owned(),
                    media_type: Some("application/json".to_owned()),
                    provider_kind: Some(OPENAI_RESPONSES_MCP_CALL_PROVIDER_KIND.to_owned())
                },
                origin: None,
                provenance_ref: None,
                preview: Some("OpenAI Responses MCP tool call: echo.echo".to_owned()),
                provider_item_id: Some("mcp_1".to_owned()),
                token_estimate: None,
                text: None,
                text_truncated: false,
                display: Some(ProviderContextDisplayView {
                    summary: ToolCallDisplayView {
                        group: ToolCallDisplayGroup::Mcp,
                        verb: "echo".to_owned(),
                        target: Some("echo".to_owned()),
                        detail: None,
                    },
                    tool_name: "echo.echo".to_owned(),
                    status: ToolItemStatus::Succeeded,
                    is_error: false,
                    arguments: Some(r#"{"data":"simba"}"#.to_owned()),
                    output: Some("Echoing your input: simba".to_owned()),
                    error: None,
                }),
                citations: Vec::new(),
                source: Some(ContextEntrySourceView::AssistantOutput {
                    run_id: "run_7".to_owned(),
                    turn_id: "turn_8".to_owned(),
                }),
                supersedes: None,
                superseded_by: None,
            }
        );
    }

    fn assistant_output_entry(
        entry_id: u64,
        kind: ContextEntryKind,
        content_ref: BlobRef,
        provider_kind: &str,
    ) -> ContextEntry {
        ContextEntry {
            entry_id: ContextEntryId::new(entry_id),
            key: None,
            kind,
            source: ContextEntrySource::AssistantOutput {
                run_id: RunId::new(7),
                turn_id: TurnId::new(8),
            },
            content: engine::ContentRef {
                content_ref,
                media_type: None,
                provider_kind: Some(provider_kind.to_owned()),
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        }
    }

    async fn json_entry(
        blobs: &InMemoryBlobStore,
        entry_id: u64,
        provider_kind: &str,
        value: serde_json::Value,
    ) -> ContextEntry {
        let content_ref = blobs
            .put_bytes(serde_json::to_vec(&value).expect("encode provider JSON"))
            .await
            .expect("store provider JSON");
        let mut entry = assistant_output_entry(
            entry_id,
            ContextEntryKind::ProviderOpaque,
            content_ref,
            provider_kind,
        );
        entry.content.media_type = Some("application/json".to_owned());
        entry
    }

    #[tokio::test(flavor = "current_thread")]
    async fn anthropic_native_message_projects_its_text_and_citations() {
        let blobs = InMemoryBlobStore::new();
        let projector = CoreAgentProjector::new(&blobs);
        let mut cited = json_entry(
            &blobs,
            44,
            ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND,
            serde_json::json!([{
                "type": "text",
                "text": "A sourced answer.",
                "citations": [{
                    "type": "web_search_result_location",
                    "url": "https://example.com/source",
                    "title": "Example source",
                    "cited_text": "A sourced answer",
                    "encrypted_index": "provider-only"
                }, {
                    "type": "web_search_result_location",
                    "url": "https://example.com/source",
                    "title": "Example source again",
                    "encrypted_index": "provider-only"
                }]
            }]),
        )
        .await;

        cited.kind = ContextEntryKind::Message {
            role: ContextMessageRole::Assistant,
        };
        let projected = projector
            .project_context_state(0, &[cited])
            .await
            .expect("project cited message");

        assert_eq!(
            projected.entries[0].text.as_deref(),
            Some("A sourced answer.")
        );
        assert_eq!(
            projected.entries[0].citations,
            vec![api::CitationView {
                url: "https://example.com/source".to_owned(),
                title: Some("Example source".to_owned()),
                cited_text: Some("A sourced answer".to_owned()),
            }]
        );
        assert_eq!(projected.entries.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn anthropic_fetch_citations_resolve_against_the_turn_fetch_result() {
        let blobs = InMemoryBlobStore::new();
        let projector = CoreAgentProjector::new(&blobs);
        let fetched = json_entry(
            &blobs,
            42,
            ANTHROPIC_MESSAGES_SERVER_TOOL_RESULT_PROVIDER_KIND,
            serde_json::json!({
                "type": "web_fetch_tool_result",
                "tool_use_id": "srvtoolu_1",
                "content": {
                    "type": "web_fetch_result",
                    "url": "https://example.com/fetched-source",
                    "content": { "type": "document", "source": { "type": "text", "data": "body" } }
                }
            }),
        )
        .await;
        let mut cited = json_entry(
            &blobs,
            44,
            ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND,
            serde_json::json!([{
                "type": "text",
                "text": "A fetched answer.",
                "citations": [{
                    "type": "char_location",
                    "document_index": 0,
                    "document_title": "Fetched source",
                    "cited_text": "A fetched answer"
                }]
            }]),
        )
        .await;

        cited.kind = ContextEntryKind::Message {
            role: ContextMessageRole::Assistant,
        };
        let projected = projector
            .project_context_state(0, &[fetched, cited])
            .await
            .expect("project fetch citation");

        assert_eq!(
            projected.entries[1].citations,
            vec![api::CitationView {
                url: "https://example.com/fetched-source".to_owned(),
                title: Some("Fetched source".to_owned()),
                cited_text: Some("A fetched answer".to_owned()),
            }]
        );
        assert!(projected.entries[0].citations.is_empty());
        assert_eq!(projected.entries.len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn openai_native_message_projects_its_text_and_citations() {
        let blobs = InMemoryBlobStore::new();
        let projector = CoreAgentProjector::new(&blobs);
        let mut cited = json_entry(
            &blobs,
            44,
            OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND,
            serde_json::json!({
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "A sourced answer.",
                    "annotations": [{
                        "type": "url_citation",
                        "start_index": 0,
                        "end_index": 16,
                        "url": "https://example.com/openai-source",
                        "title": "OpenAI source"
                    }]
                }]
            }),
        )
        .await;

        cited.kind = ContextEntryKind::Message {
            role: ContextMessageRole::Assistant,
        };
        let projected = projector
            .project_event_kind(&CoreAgentEvent::Context(ContextEvent::EntriesApplied {
                base_revision: 0,
                entries: vec![cited],
            }))
            .await
            .expect("project cited context event");

        let SessionEventKindView::ContextEntriesApplied { entries, .. } = projected else {
            panic!("expected context entries applied");
        };
        assert_eq!(
            entries[0].citations,
            vec![api::CitationView {
                url: "https://example.com/openai-source".to_owned(),
                title: Some("OpenAI source".to_owned()),
                cited_text: Some("A sourced answer".to_owned()),
            }]
        );
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn anthropic_server_tool_use_projects_as_a_search_step() {
        let blobs = InMemoryBlobStore::new();
        let content_ref = blobs
            .put_bytes(
                serde_json::to_vec(&serde_json::json!({
                    "type": "server_tool_use",
                    "id": "srvtoolu_1",
                    "name": "web_search",
                    "input": { "query": "lightspeed agent runtime" }
                }))
                .expect("encode server tool use"),
            )
            .await
            .expect("store server tool use");
        let projector = CoreAgentProjector::new(&blobs);
        let item = assistant_output_entry(
            44,
            ContextEntryKind::ProviderOpaque,
            content_ref,
            ANTHROPIC_MESSAGES_SERVER_TOOL_USE_PROVIDER_KIND,
        );

        let view = projector
            .project_context_entry(&item, None)
            .await
            .expect("project server tool use");

        let display = view.display.expect("server tool display");
        assert_eq!(display.tool_name, "web_search");
        assert_eq!(display.status, ToolItemStatus::Succeeded);
        assert_eq!(display.summary.verb, "Search");
        assert_eq!(
            display.summary.target.as_deref(),
            Some("lightspeed agent runtime")
        );
        assert!(view.citations.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn openai_web_search_call_projects_as_a_search_step() {
        let blobs = InMemoryBlobStore::new();
        let content_ref = blobs
            .put_bytes(
                serde_json::to_vec(&serde_json::json!({
                    "id": "ws_1",
                    "type": "web_search_call",
                    "status": "completed",
                    "action": { "type": "search", "query": "lightspeed agent runtime" }
                }))
                .expect("encode web search call"),
            )
            .await
            .expect("store web search call");
        let projector = CoreAgentProjector::new(&blobs);
        let item = assistant_output_entry(
            45,
            ContextEntryKind::ProviderOpaque,
            content_ref,
            OPENAI_RESPONSES_WEB_SEARCH_CALL_PROVIDER_KIND,
        );

        let view = projector
            .project_context_entry(&item, None)
            .await
            .expect("project web search call");

        let display = view.display.expect("web search display");
        assert_eq!(display.tool_name, "web_search");
        assert_eq!(display.status, ToolItemStatus::Succeeded);
        assert_eq!(display.summary.verb, "Search");
        assert_eq!(
            display.summary.target.as_deref(),
            Some("lightspeed agent runtime")
        );
    }

    #[test]
    fn session_config_with_default_sections_projects_sparse_document() {
        let config = SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-5".to_owned(),
            },
            generation: engine::GenerationConfig::default(),
            limits: engine::LimitsConfig::default(),
            context: engine::ContextConfig::default(),
            features: engine::FeaturesConfig::default(),
        };

        let projected = session_config_to_api(&config).expect("project sparse config");

        assert_eq!(
            projected,
            api::SessionConfig {
                model: Some(ModelConfig {
                    provider_id: "openai".to_owned(),
                    api_kind: "openai:responses".to_owned(),
                    model: "gpt-5".to_owned(),
                }),
                generation: None,
                limits: None,
                context: None,
                features: None,
            }
        );
    }

    #[test]
    fn session_config_projects_sections_and_features_field_by_field() {
        let config = SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::AnthropicMessages,
                provider_id: "anthropic".to_owned(),
                model: "claude".to_owned(),
            },
            generation: engine::GenerationConfig {
                max_output_tokens: Some(2048),
                reasoning_effort: Some("high".to_owned()),
                tool_choice: Some(ToolChoice::Specific {
                    tool_name: engine::ToolName::new("read_file"),
                }),
                parallel_tool_use: Some(false),
                processing_tier: None,
            },
            limits: engine::LimitsConfig {
                max_turns: Some(12),
                max_tool_rounds: Some(3),
            },
            context: engine::ContextConfig {
                compaction: Some(engine::CompactionPolicy::ProviderStandalone {
                    compact_threshold_tokens: Some(20_000),
                    target_tokens: Some(8_000),
                }),
            },
            features: engine::FeaturesConfig {
                vfs: Some(engine::VfsFeature {
                    working_directory: None,
                    version: engine::CURRENT_FEATURE_VERSION,
                    workspace_links: Vec::new(),
                    tools: Some(engine::VfsToolSurface::ReadOnly),
                    prompts: Some(engine::VfsPromptsConfig {
                        roots: Some(vec!["/prompts".to_owned()]),
                    }),
                    skills: Some(engine::VfsSkillsConfig {
                        roots: Some(vec!["/workspace/skills".into()]),
                    }),
                }),
                web: Some(engine::WebFeature {
                    version: engine::CURRENT_FEATURE_VERSION,
                    fetch: Some(engine::WebFetchFeature {}),
                    search: Some(engine::WebSearchFeature {
                        allowed_domains: Some(vec!["example.com".to_owned()]),
                        blocked_domains: vec!["blocked.example".to_owned()],
                    }),
                }),
                subagents: Some(engine::SubagentsFeature {
                    version: engine::CURRENT_FEATURE_VERSION,
                    agents: vec![engine::SubagentAgentConfig {
                        profile_id: "researcher".to_owned(),
                    }],
                    limits: engine::SubagentLimits {
                        max_depth: 3,
                        max_descendants: 10,
                        max_concurrent: 2,
                        deadline_ms: 120_000,
                    },
                }),
                timers: Some(engine::TimersFeature::default()),
                environments: Some(engine::EnvironmentsFeature {
                    tools: Some(engine::EnvironmentToolSurface::Edit),
                    commands: true,
                    ..Default::default()
                }),
                mcp: Some(engine::McpFeature {
                    version: engine::CURRENT_FEATURE_VERSION,
                    servers: vec![engine::McpServerLink {
                        server_id: "linear".to_owned(),
                    }],
                }),
            },
        };

        let projected = session_config_to_api(&config).expect("project populated config");

        assert_eq!(
            projected,
            api::SessionConfig {
                model: Some(ModelConfig {
                    provider_id: "anthropic".to_owned(),
                    api_kind: "anthropic:messages".to_owned(),
                    model: "claude".to_owned(),
                }),
                generation: Some(api::GenerationConfig {
                    max_output_tokens: Some(2048),
                    reasoning_effort: Some("high".to_owned()),
                    tool_choice: Some(api::ToolChoice::Specific {
                        tool_id: "read_file".to_owned(),
                    }),
                    parallel_tool_use: Some(false),
                    processing_tier: None,
                }),
                limits: Some(api::LimitsConfig {
                    max_turns: Some(12),
                    max_tool_rounds: Some(3),
                }),
                context: Some(api::ContextConfig {
                    compaction: Some(api::CompactionPolicy::ProviderStandalone {
                        compact_threshold_tokens: Some(20_000),
                        target_tokens: Some(8_000),
                    }),
                }),
                features: Some(api::FeaturesConfig {
                    vfs: Some(api::VfsFeature {
                        working_directory: None,
                        version: api::CURRENT_FEATURE_VERSION,
                        workspace_links: Vec::new(),
                        tools: Some(api::VfsToolSurface::ReadOnly),
                        prompts: Some(api::VfsPromptsConfig {
                            roots: Some(vec!["/prompts".to_owned()]),
                        }),
                        skills: Some(api::VfsSkillsConfig {
                            roots: Some(vec!["/workspace/skills".into()])
                        }),
                    }),
                    web: Some(api::WebFeature {
                        version: api::CURRENT_FEATURE_VERSION,
                        fetch: Some(api::WebFetchFeature {}),
                        search: Some(api::WebSearchFeature {
                            allowed_domains: Some(vec!["example.com".to_owned()]),
                            blocked_domains: vec!["blocked.example".to_owned()],
                        }),
                    }),
                    subagents: Some(api::SubagentsFeature {
                        version: api::CURRENT_FEATURE_VERSION,
                        agents: vec![api::SubagentAgentRef {
                            profile_id: api::ProfileId::try_new("researcher".to_owned())
                                .expect("valid profile id"),
                        }],
                        max_depth: 3,
                        max_descendants: 10,
                        max_concurrent: 2,
                        deadline_ms: 120_000,
                    }),
                    timers: Some(api::TimersFeature {
                        version: api::CURRENT_FEATURE_VERSION,
                    }),
                    environments: Some(api::EnvironmentsFeature {
                        tools: Some(api::EnvironmentToolSurface::Edit),
                        commands: true,
                        working_directory: None,
                        prompts: None,
                        version: api::CURRENT_FEATURE_VERSION,
                        providers: None,
                        registration_keys: None,
                        selection_tools: false,
                        jobs: false,
                        skills: None,
                    }),
                    mcp: Some(api::McpFeature {
                        version: api::CURRENT_FEATURE_VERSION,
                        servers: vec![api::McpServerLink {
                            server_id: "linear".to_owned(),
                        }],
                    }),
                }),
            }
        );
    }

    #[test]
    fn session_origin_projects_lineage_and_pinned_limits() {
        let origin = engine::storage::SessionOrigin {
            kind: engine::storage::SessionOriginKind::Subagent,
            parent_session_id: engine::SessionId::new("parent"),
            parent_run_id: 7,
            root_session_id: engine::SessionId::new("root"),
            depth: 2,
            invocation_id: "wti_1".to_owned(),
            profile_id: "reviewer".to_owned(),
            profile_revision: 4,
            limits: engine::SubagentLimits::default(),
        };
        let projected = session_origin_to_api(&origin).expect("project origin");
        assert_eq!(projected.kind, api::SessionOriginKind::Subagent);
        assert_eq!(projected.parent_session_id, "parent");
        assert_eq!(projected.parent_run_id, "run_7");
        assert_eq!(projected.root_session_id, "root");
        assert_eq!(projected.depth, 2);
        assert_eq!(projected.agent.profile_id.as_str(), "reviewer");
        assert_eq!(projected.agent.revision, 4);
        assert_eq!(
            projected.limits,
            subagent_limits_to_api(engine::SubagentLimits::default())
        );
    }

    #[test]
    fn input_text_joins_non_empty_text_items() {
        let text = input_text(&[
            InputItem::Text {
                origin: None,
                text: " first ".to_owned(),
            },
            InputItem::Text {
                origin: None,
                text: "".to_owned(),
            },
            InputItem::Text {
                origin: None,
                text: "second".to_owned(),
            },
        ])
        .expect("valid input");

        assert_eq!(text, "first\n\nsecond");
    }

    #[test]
    fn input_text_rejects_unresolved_text_refs() {
        let error = input_text(&[InputItem::TextRef {
            origin: None,
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
            event: CoreAgentEvent::Context(ContextEvent::EntriesApplied {
                base_revision: seq - 1,
                entries,
            }),
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
            content: engine::ContentRef {
                content_ref: BlobRef::default(),
                media_type: None,
                provider_kind: None,
            },
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
            supersedes: None,
        }
    }
}

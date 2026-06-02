use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use api::{
    AgentApiOutcome, EventCursor, GenerationConfig, InputItem, ModelConfig,
    ReasoningEffort as ApiReasoningEffort, RunStartConfig, RunStartParams, RunStartResponse,
    SessionConfigInput, SessionEventKindView, SessionEventView, SessionEventsReadParams,
    SessionItemView, SessionReadParams, SessionStartParams, SessionView, ToolBatchView,
    ToolCallEventView, ToolCallView, ToolItemStatus,
};
use clap::Args;
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::api_client::{HttpAgentApi, api_error};
use crate::chat::preview::compact_preview;
use crate::chat::protocol::{
    ChatCommand, ChatConnectionInfo, ChatDelta, ChatDraftSettings, ChatErrorView, ChatEvent,
    ChatMessageView, ChatProgressStatus, ChatReasoningView, ChatRunView, ChatSessionSummary,
    ChatSettingsView, ChatStatus, ChatToolCallDisplayView, ChatToolCallView, ChatToolChainView,
    ChatToolDisplayGroup, ChatTurn, DEFAULT_CHAT_REASONING_EFFORT, GATEWAY_WORLD_ID, run_status,
    session_lifecycle,
};
use crate::chat::session::{new_session_id, validate_session_id};

#[derive(Args, Debug, Clone)]
pub(crate) struct ChatArgs {
    /// Session ID to open or create through the configured Forge API.
    #[arg(long)]
    session: Option<String>,
    /// Start with a fresh session ID.
    #[arg(long)]
    new: bool,
    /// Provider ID for the model adapter.
    #[arg(
        long,
        env = "FORGE_CHAT_PROVIDER",
        default_value = crate::chat::protocol::DEFAULT_CHAT_PROVIDER
    )]
    provider: String,
    /// Provider API kind.
    #[arg(
        long = "api-kind",
        env = "FORGE_CHAT_API_KIND",
        default_value = crate::chat::protocol::DEFAULT_CHAT_API_KIND
    )]
    api_kind: String,
    /// Model name.
    #[arg(
        long,
        env = "FORGE_CHAT_MODEL",
        default_value = crate::chat::protocol::DEFAULT_CHAT_MODEL
    )]
    model: String,
    /// Reasoning effort: low, medium, high, or none.
    #[arg(long, env = "FORGE_CHAT_REASONING_EFFORT", default_value = "high")]
    effort: Option<String>,
    /// Max output token limit.
    #[arg(long, env = "FORGE_CHAT_MAX_TOKENS")]
    max_tokens: Option<u32>,
    /// Working directory for local file tools. Defaults to the current directory.
    #[arg(long)]
    workdir: Option<PathBuf>,
    /// Snapshot a local directory, create a VFS workspace, and mount it for this chat.
    #[arg(long)]
    mount: Option<PathBuf>,
    /// VFS path used for --mount. Defaults to /workspace.
    #[arg(long = "mount-path", default_value = "/workspace")]
    mount_path: String,
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Show full completed tool call arguments and results in the TUI.
    #[arg(long)]
    show_tool_details: bool,
    /// Emit the response as JSON.
    #[arg(long)]
    json: bool,
    /// Submit one message and exit. If omitted, starts the interactive TUI.
    message: Vec<String>,
}

pub(crate) async fn handle(args: ChatArgs) -> Result<()> {
    let draft = draft_settings(&args)?;
    if args.mount.is_some() && args.workdir.is_some() {
        return Err(anyhow!(
            "--workdir cannot be used with --mount; use --mount-path for the VFS cwd"
        ));
    }
    let mount = args.mount.clone();
    let mount_path = args.mount_path.clone();
    let workdir = if mount.is_some() {
        mount_path.clone()
    } else {
        resolve_chat_workdir(args.workdir)?
    };
    let session_id = if args.new {
        new_session_id()
    } else if let Some(session_id) = args.session {
        validate_session_id(&session_id)?
    } else {
        new_session_id()
    };

    let message = (!args.message.is_empty()).then(|| args.message.join(" "));
    let (mut driver, mut initial_events) = ChatSessionDriver::open(ChatSessionDriverOptions {
        session_id,
        draft_settings: draft,
        workdir,
        api_url: args.api_url,
    })
    .await?;
    if let Some(directory) = mount {
        let events = driver.mount_local_directory(directory, mount_path).await?;
        initial_events.extend(events);
    }

    if args.json {
        if let Some(message) = message {
            driver
                .handle_command(ChatCommand::SubmitUserMessage { text: message })
                .await?;
            driver
                .follow_until_quiescent(Duration::from_secs(300), |_| {})
                .await?;
        }
        println!("{}", serde_json::to_string_pretty(driver.turns())?);
        return Ok(());
    }

    if let Some(message) = message {
        for event in &initial_events {
            print_event(event)?;
        }
        for event in driver
            .handle_command(ChatCommand::SubmitUserMessage { text: message })
            .await?
        {
            print_event(&event)?;
        }
        let mut follow_events = Vec::new();
        driver
            .follow_until_quiescent(Duration::from_secs(300), |event| {
                follow_events.push(event);
            })
            .await?;
        for event in &follow_events {
            print_event(event)?;
        }
        return Ok(());
    }

    crate::chat::tui::run_shell(driver, initial_events, args.show_tool_details).await
}

#[derive(Debug, Clone)]
pub(crate) struct ChatSessionDriverOptions {
    pub session_id: String,
    pub draft_settings: ChatDraftSettings,
    pub workdir: String,
    pub api_url: String,
}

pub(crate) struct ChatSessionDriver {
    api: ChatAgentApi,
    session_id: String,
    settings: ChatDraftSettings,
    event_cursor: Option<EventCursor>,
    turns: Vec<ChatTurn>,
    active_tool_chains: Vec<ChatToolChainView>,
    sessions: BTreeSet<String>,
    workdir: String,
    pending_run: Option<PendingRunHandle>,
    notice_seq: u64,
}

type PendingRunHandle =
    JoinHandle<std::result::Result<AgentApiOutcome<RunStartResponse>, api::AgentApiError>>;

type ChatAgentApi = Arc<HttpAgentApi>;

impl ChatSessionDriver {
    pub(crate) async fn open(options: ChatSessionDriverOptions) -> Result<(Self, Vec<ChatEvent>)> {
        let session_id = validate_session_id(&options.session_id)?;
        let api = build_chat_api(&options).await?;
        let started = api
            .open_or_start_session(SessionStartParams {
                session_id: Some(session_id.clone()),
                cwd: Some(options.workdir.clone()),
                config: Some(session_start_config(&options.draft_settings)),
            })
            .await
            .map_err(api_error)?;

        let mut driver = Self {
            api,
            session_id: session_id.clone(),
            settings: options.draft_settings,
            event_cursor: None,
            turns: Vec::new(),
            active_tool_chains: Vec::new(),
            sessions: BTreeSet::from([session_id.clone()]),
            workdir: options.workdir,
            pending_run: None,
            notice_seq: 0,
        };
        let mut events = vec![ChatEvent::Connected(ChatConnectionInfo {
            world_id: GATEWAY_WORLD_ID.into(),
            session_id,
            journal_next_from: None,
            settings: driver.settings_view(),
        })];
        events.push(ChatEvent::SessionSelected(summary_from_session(
            &started.result.session,
        )));
        events.extend(driver.refresh().await?);
        Ok((driver, events))
    }

    pub(crate) fn turns(&self) -> &[ChatTurn] {
        &self.turns
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn status_event(&self, status: impl Into<String>) -> ChatEvent {
        ChatEvent::StatusChanged(ChatStatus {
            session_id: self.session_id.clone(),
            status: status.into(),
            detail: None,
            settings: self.settings_view(),
        })
    }

    pub(crate) async fn mount_local_directory(
        &mut self,
        directory: PathBuf,
        mount_path: String,
    ) -> Result<Vec<ChatEvent>> {
        if !self.is_quiescent() {
            return Err(anyhow!("cannot mount a directory while a run is active"));
        }
        let summary = crate::vfs_transfer::upload_snapshot_directory(
            self.api.as_ref(),
            directory,
            crate::vfs_transfer::SnapshotUploadOptions::default(),
        )
        .await
        .context("failed to upload chat mount directory")?;
        let workspace =
            crate::vfs_cli::create_workspace_from_snapshot(self.api.as_ref(), summary.snapshot_ref)
                .await
                .context("failed to create chat mount workspace")?;
        crate::vfs_cli::mount_workspace(
            self.api.as_ref(),
            self.session_id.clone(),
            mount_path.clone(),
            workspace.workspace_id,
        )
        .await
        .context("failed to mount chat workspace")?;
        self.workdir = mount_path;
        self.refresh().await
    }

    pub(crate) async fn handle_command(&mut self, command: ChatCommand) -> Result<Vec<ChatEvent>> {
        match command {
            ChatCommand::SubmitUserMessage { text } => self.submit_user_message(text).await,
            ChatCommand::SetDraftProvider { provider } => self.set_provider(provider).await,
            ChatCommand::SetDraftModel { model } => self.set_model(model).await,
            ChatCommand::SetDraftReasoningEffort { effort } => self.set_effort(effort).await,
            ChatCommand::SetDraftMaxTokens { max_tokens } => self.set_max_tokens(max_tokens).await,
            ChatCommand::ListSessions => Ok(vec![ChatEvent::SessionsListed {
                world_id: GATEWAY_WORLD_ID.into(),
                sessions: self
                    .sessions
                    .iter()
                    .map(|session_id| ChatSessionSummary {
                        session_id: session_id.clone(),
                        status: Some(api::SessionStatus::Idle),
                        lifecycle: Some(crate::chat::protocol::ChatSessionLifecycle::Idle),
                        updated_at_ns: None,
                        run_count: 0,
                        provider: Some(self.settings.provider.clone()),
                        model: Some(self.settings.model.clone()),
                        active_run: None,
                    })
                    .collect(),
            }]),
            ChatCommand::ListSkills => self.list_skills().await,
            ChatCommand::ListActiveSkills => self.list_active_skills().await,
            ChatCommand::PickSkill { scope } => self.pick_skill(scope).await,
            ChatCommand::ActivateSkill { skill_id, scope } => {
                self.activate_skill(skill_id, scope).await
            }
            ChatCommand::DeactivateSkill { skill_id } => self.deactivate_skill(skill_id).await,
            ChatCommand::NewSession => self.new_session().await,
            ChatCommand::SteerRun { .. } => Ok(vec![ChatEvent::Error(ChatErrorView {
                message:
                    "steering an active run is not implemented by the current Forge API boundary"
                        .into(),
                action: Some("wait for the run to finish and submit a follow-up message".into()),
            })]),
            ChatCommand::InterruptRun { .. } => Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "interrupt is not implemented by the current Forge API boundary".into(),
                action: Some(
                    "cancel support belongs at the API boundary and will be added there".into(),
                ),
            })]),
            ChatCommand::PauseSession | ChatCommand::ResumeSession => {
                Ok(vec![ChatEvent::Error(ChatErrorView {
                    message: "pause/resume is not implemented for Forge API sessions".into(),
                    action: None,
                })])
            }
            ChatCommand::SwitchSession { session_id } => self.switch_session(session_id).await,
            ChatCommand::Refresh => self.refresh().await,
            ChatCommand::Shutdown => Ok(vec![ChatEvent::StatusChanged(ChatStatus {
                session_id: self.session_id.clone(),
                status: "shutdown".into(),
                detail: None,
                settings: self.settings_view(),
            })]),
        }
    }

    pub(crate) async fn follow_until_quiescent<F>(
        &mut self,
        timeout: Duration,
        mut emit: F,
    ) -> Result<()>
    where
        F: FnMut(ChatEvent),
    {
        let mut inactivity_deadline = InactivityDeadline::new(Instant::now(), timeout);
        loop {
            let events = self.drain_event_log().await?;
            let mut saw_activity = !events.is_empty();
            for event in events {
                emit(event);
            }

            let finished_events = self.collect_finished_run().await?;
            saw_activity |= !finished_events.is_empty();
            for event in finished_events {
                emit(event);
            }
            if saw_activity {
                inactivity_deadline.record_activity(Instant::now());
            }

            if self.is_quiescent() {
                let events = self.drain_event_log().await?;
                for event in events {
                    emit(event);
                }
                for event in self.refresh_snapshot().await? {
                    emit(event);
                }
                emit(ChatEvent::ToolChainsChanged {
                    session_id: self.session_id.clone(),
                    chains: Vec::new(),
                });
                return Ok(());
            }
            let now = Instant::now();
            if should_timeout_after_inactivity(
                &inactivity_deadline,
                now,
                self.pending_run_in_flight(),
            ) {
                return Err(anyhow!(
                    "timed out waiting for session '{}' to become idle after {:?} without events",
                    self.session_id,
                    timeout
                ));
            }
            if saw_activity {
                tokio::task::yield_now().await;
            } else {
                sleep(Duration::from_millis(250)).await;
            }
        }
    }

    async fn submit_user_message(&mut self, text: String) -> Result<Vec<ChatEvent>> {
        if !self.is_quiescent() {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "a run is already active in this session".into(),
                action: Some("wait for it to finish before submitting another message".into()),
            })]);
        }

        let events = vec![self.status_event("working")];

        let api = self.api.clone();
        let session_id = self.session_id.clone();
        let config = run_start_config(&self.settings);
        self.pending_run = Some(tokio::spawn(async move {
            api.start_run(RunStartParams {
                session_id,
                input: vec![InputItem::Text { text }],
                config: Some(config),
            })
            .await
        }));

        Ok(events)
    }

    async fn list_skills(&mut self) -> Result<Vec<ChatEvent>> {
        let response = self
            .api
            .list_skills(api::SkillListParams {
                session_id: self.session_id.clone(),
            })
            .await
            .map_err(api_error)?
            .result;
        Ok(vec![
            self.notice_event("skills", format_skill_list(&response)),
        ])
    }

    async fn list_active_skills(&mut self) -> Result<Vec<ChatEvent>> {
        let response = self
            .api
            .active_skills(api::SkillActiveParams {
                session_id: self.session_id.clone(),
            })
            .await
            .map_err(api_error)?
            .result;
        Ok(vec![self.notice_event(
            "active-skills",
            format_active_skills(&response),
        )])
    }

    async fn pick_skill(&mut self, scope: api::SkillActivationScope) -> Result<Vec<ChatEvent>> {
        let response = self
            .api
            .list_skills(api::SkillListParams {
                session_id: self.session_id.clone(),
            })
            .await
            .map_err(api_error)?
            .result;
        Ok(vec![ChatEvent::SkillsListed {
            session_id: self.session_id.clone(),
            catalog_ref: response.catalog_ref,
            skills: response.skills,
            scope,
        }])
    }

    async fn activate_skill(
        &mut self,
        skill_id: String,
        scope: api::SkillActivationScope,
    ) -> Result<Vec<ChatEvent>> {
        if !self.is_quiescent() {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "skill activation is only available while no run is active".into(),
                action: Some("wait for the current run to finish first".into()),
            })]);
        }

        let response = self
            .api
            .activate_skill(api::SkillActivateParams {
                session_id: self.session_id.clone(),
                skill_id,
                scope,
            })
            .await
            .map_err(api_error)?
            .result;
        Ok(vec![self.notice_event(
            "skill-activated",
            format_skill_activation_response(&response),
        )])
    }

    async fn deactivate_skill(&mut self, skill_id: String) -> Result<Vec<ChatEvent>> {
        if !self.is_quiescent() {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "skill deactivation is only available while no run is active".into(),
                action: Some("wait for the current run to finish first".into()),
            })]);
        }

        let response = self
            .api
            .deactivate_skill(api::SkillDeactivateParams {
                session_id: self.session_id.clone(),
                skill_id,
            })
            .await
            .map_err(api_error)?
            .result;
        Ok(vec![self.notice_event(
            "skill-deactivated",
            format_skill_deactivation_response(&response),
        )])
    }

    async fn collect_finished_run(&mut self) -> Result<Vec<ChatEvent>> {
        let Some(handle) = self.pending_run.as_ref() else {
            return Ok(Vec::new());
        };
        if !handle.is_finished() {
            return Ok(Vec::new());
        }

        let Some(handle) = self.pending_run.take() else {
            return Ok(Vec::new());
        };
        match handle.await {
            Ok(Ok(_outcome)) => {
                let mut events = self.drain_event_log().await?;
                events.extend(self.refresh_snapshot().await?);
                Ok(events)
            }
            Ok(Err(error)) => Ok(vec![ChatEvent::Error(ChatErrorView {
                message: error.to_string(),
                action: None,
            })]),
            Err(error) => Ok(vec![ChatEvent::Error(ChatErrorView {
                message: format!("run task failed: {error}"),
                action: None,
            })]),
        }
    }

    async fn refresh(&mut self) -> Result<Vec<ChatEvent>> {
        self.sync_event_cursor().await?;
        self.refresh_snapshot().await
    }

    async fn refresh_snapshot(&mut self) -> Result<Vec<ChatEvent>> {
        let read = self
            .api
            .read_session(SessionReadParams {
                session_id: self.session_id.clone(),
            })
            .await
            .map_err(api_error)?;
        let session = read.result.session;
        let old_turns = self.turns.clone();
        let old_active_tool_chains = self.active_tool_chains.clone();
        self.turns = project_turns(&session, &self.settings);
        self.active_tool_chains = project_active_tool_chains(&session, &self.settings);

        let mut events = Vec::new();
        events.push(ChatEvent::SessionSelected(summary_from_session(&session)));
        if old_turns != self.turns {
            events.push(ChatEvent::TranscriptDelta(ChatDelta::ReplaceTurns {
                session_id: self.session_id.clone(),
                turns: self.turns.clone(),
            }));
        }
        if old_active_tool_chains != self.active_tool_chains {
            events.push(ChatEvent::ToolChainsChanged {
                session_id: self.session_id.clone(),
                chains: self.active_tool_chains.clone(),
            });
        }
        if let Some(active_run) = session
            .runs
            .iter()
            .find(|run| matches!(run.status, api::RunStatus::Running))
        {
            events.push(run_event_from_view(
                active_run,
                &self.settings,
                run_seq_from_id(&active_run.id),
            ));
        }
        events.push(ChatEvent::StatusChanged(ChatStatus {
            session_id: self.session_id.clone(),
            status: session_status_text(session.status).to_string(),
            detail: None,
            settings: self.settings_view(),
        }));
        Ok(events)
    }

    async fn drain_event_log(&mut self) -> Result<Vec<ChatEvent>> {
        let mut events = Vec::new();
        let mut needs_snapshot = false;
        loop {
            let page = self
                .api
                .read_session_events(SessionEventsReadParams {
                    session_id: self.session_id.clone(),
                    after: self.event_cursor,
                    limit: Some(128),
                })
                .await
                .map_err(api_error)?;

            if let Some(gap) = page.result.gap.as_ref() {
                events.push(ChatEvent::GapObserved {
                    requested_from: gap
                        .requested_after
                        .map(|cursor| cursor.seq.saturating_add(1))
                        .unwrap_or_default(),
                    retained_from: gap
                        .retained_after
                        .map(|cursor| cursor.seq.saturating_add(1))
                        .unwrap_or_default(),
                });
                needs_snapshot = true;
            }

            for event in &page.result.events {
                needs_snapshot |= event_needs_snapshot(&event.kind);
                events.extend(self.chat_events_from_session_event(event));
            }

            self.event_cursor = page.result.next_cursor.or(page.result.head_cursor);
            if page.result.complete {
                break;
            }
        }

        if needs_snapshot {
            events.extend(self.refresh_snapshot().await?);
        }
        Ok(events)
    }

    fn chat_events_from_session_event(&mut self, event: &SessionEventView) -> Vec<ChatEvent> {
        let mut events = Vec::new();
        match &event.kind {
            SessionEventKindView::RunStarted { run_id, .. } => {
                events.push(ChatEvent::RunChanged(self.run_view_from_status(
                    run_id,
                    api::RunStatus::Running,
                    event.observed_at_ms,
                )));
                events.push(self.status_event("running"));
            }
            SessionEventKindView::RunCompleted { run_id, .. } => {
                events.push(ChatEvent::RunChanged(self.run_view_from_status(
                    run_id,
                    api::RunStatus::Completed,
                    event.observed_at_ms,
                )));
                events.push(self.status_event("finishing"));
            }
            SessionEventKindView::RunFailed { run_id, message } => {
                events.push(ChatEvent::RunChanged(self.run_view_from_status(
                    run_id,
                    api::RunStatus::Failed,
                    event.observed_at_ms,
                )));
                events.push(ChatEvent::Error(ChatErrorView {
                    message: message.clone(),
                    action: None,
                }));
            }
            SessionEventKindView::RunCancelled { run_id } => {
                events.push(ChatEvent::RunChanged(self.run_view_from_status(
                    run_id,
                    api::RunStatus::Cancelled,
                    event.observed_at_ms,
                )));
                events.push(self.status_event("cancelled"));
            }
            SessionEventKindView::TurnStarted { .. } => events.push(self.status_event("planning")),
            SessionEventKindView::TurnPlanned { .. } => events.push(self.status_event("thinking")),
            SessionEventKindView::TurnGenerationRequested { .. } => {
                events.push(self.status_event("thinking"))
            }
            SessionEventKindView::TurnGenerationCompleted { .. } => {}
            SessionEventKindView::ToolBatchStarted {
                run_id,
                batch_id,
                calls,
                ..
            } => {
                let chain = self.tool_chain_from_started_event(run_id, batch_id, calls);
                self.active_tool_chains = vec![chain.clone()];
                events.push(ChatEvent::ToolChainsChanged {
                    session_id: event.session_id.clone(),
                    chains: vec![chain],
                });
                events.push(self.status_event("running tools"));
            }
            SessionEventKindView::ToolBatchCompleted { .. } => {
                events.push(self.status_event("tools complete"));
            }
            SessionEventKindView::ToolCallStarted { .. } => {
                events.push(self.status_event("running tools"));
            }
            SessionEventKindView::ToolCallCompleted { .. } => {
                events.push(self.status_event("tool result received"));
            }
            SessionEventKindView::ItemsRecorded { .. } => {}
            SessionEventKindView::CompactionRecorded { .. } => {
                events.push(ChatEvent::CompactionsChanged {
                    session_id: event.session_id.clone(),
                    compactions: Vec::new(),
                });
            }
            SessionEventKindView::SessionOpened { .. }
            | SessionEventKindView::SessionConfigChanged { .. }
            | SessionEventKindView::SessionClosed
            | SessionEventKindView::RunQueued { .. }
            | SessionEventKindView::RunSteeringAdded { .. }
            | SessionEventKindView::RunCancellationRequested { .. }
            | SessionEventKindView::SkillCatalogSet { .. }
            | SessionEventKindView::SkillActivationsSet { .. }
            | SessionEventKindView::TurnCompleted { .. }
            | SessionEventKindView::ContextWindowPlanned { .. }
            | SessionEventKindView::ToolRegistryChanged
            | SessionEventKindView::ToolProfileSelected { .. }
            | SessionEventKindView::ToolDefaultTargetChanged { .. } => {}
        }
        events
    }

    fn run_view_from_status(
        &self,
        run_id: &str,
        status: api::RunStatus,
        observed_at_ms: u64,
    ) -> ChatRunView {
        ChatRunView {
            id: run_id.to_string(),
            run_seq: run_seq_from_id(run_id),
            lifecycle: status,
            status: run_status(status),
            provider: self.settings.provider.clone(),
            model: self.settings.model.clone(),
            reasoning_effort: self.settings.reasoning_effort,
            input_refs: Vec::new(),
            output_ref: None,
            started_at_ns: observed_at_ms.saturating_mul(1_000_000),
            updated_at_ns: observed_at_ms.saturating_mul(1_000_000),
        }
    }

    fn tool_chain_from_started_event(
        &self,
        run_id: &str,
        batch_id: &str,
        calls: &[ToolCallEventView],
    ) -> ChatToolChainView {
        let calls = calls
            .iter()
            .enumerate()
            .map(|(index, call)| tool_call_from_event(index, call))
            .collect::<Vec<_>>();
        ChatToolChainView {
            id: format!("{run_id}:{batch_id}"),
            title: format!("tools {} calls", calls.len()),
            status: ChatProgressStatus::Running,
            reasoning: None,
            summary: tool_activity_summary(&calls).or_else(|| Some("tools".into())),
            calls,
        }
    }

    async fn sync_event_cursor(&mut self) -> Result<()> {
        loop {
            let page = self
                .api
                .read_session_events(SessionEventsReadParams {
                    session_id: self.session_id.clone(),
                    after: self.event_cursor,
                    limit: Some(512),
                })
                .await
                .map_err(api_error)?;
            self.event_cursor = page.result.next_cursor.or(page.result.head_cursor);
            if page.result.complete {
                return Ok(());
            }
        }
    }

    async fn new_session(&mut self) -> Result<Vec<ChatEvent>> {
        if !self.is_quiescent() {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "cannot create a new session while a run is active".into(),
                action: Some("wait for the current run to finish first".into()),
            })]);
        }
        let session_id = new_session_id();
        self.sessions.insert(session_id.clone());
        self.session_id = session_id.clone();
        self.event_cursor = None;
        self.turns.clear();
        self.active_tool_chains.clear();
        self.api
            .start_session(SessionStartParams {
                session_id: Some(session_id.clone()),
                cwd: Some(self.workdir.clone()),
                config: Some(session_start_config(&self.settings)),
            })
            .await
            .map_err(api_error)?;
        let mut events = vec![ChatEvent::HistoryReset { session_id }];
        events.extend(self.refresh().await?);
        Ok(events)
    }

    async fn switch_session(&mut self, session_id: String) -> Result<Vec<ChatEvent>> {
        if !self.is_quiescent() {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "cannot switch sessions while a run is active".into(),
                action: Some("wait for the current run to finish first".into()),
            })]);
        }
        let session_id = validate_session_id(&session_id)?;
        if !self.sessions.contains(&session_id) {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: format!("unknown loaded session: {session_id}"),
                action: Some("use /new to create a session in this process".into()),
            })]);
        }
        self.session_id = session_id.clone();
        self.event_cursor = None;
        self.turns.clear();
        self.active_tool_chains.clear();
        let mut events = vec![ChatEvent::HistoryReset { session_id }];
        events.extend(self.refresh().await?);
        Ok(events)
    }

    async fn set_provider(&mut self, provider: String) -> Result<Vec<ChatEvent>> {
        if self.model_locked() {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message:
                    "provider switching is not supported after this session has accepted a run"
                        .into(),
                action: Some("start a new session with /new for another provider".into()),
            })]);
        }
        self.settings.provider = provider;
        Ok(vec![self.setting_status("provider updated")])
    }

    async fn set_model(&mut self, model: String) -> Result<Vec<ChatEvent>> {
        if self.model_locked() {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "model switching is not supported after this session has accepted a run"
                    .into(),
                action: Some("start a new session with /new for another model".into()),
            })]);
        }
        self.settings.model = model;
        Ok(vec![self.setting_status("model updated")])
    }

    async fn set_effort(
        &mut self,
        effort: Option<crate::chat::protocol::ReasoningEffort>,
    ) -> Result<Vec<ChatEvent>> {
        if self.run_active() {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "reasoning effort cannot be changed while a run is active".into(),
                action: Some(
                    "wait for the current run to finish, then set effort for the next session"
                        .into(),
                ),
            })]);
        }
        self.settings.reasoning_effort = effort;
        Ok(vec![self.setting_status("reasoning effort updated")])
    }

    async fn set_max_tokens(&mut self, max_tokens: Option<u32>) -> Result<Vec<ChatEvent>> {
        if self.run_active() {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "max tokens cannot be changed while a run is active".into(),
                action: Some(
                    "wait for the current run to finish, then set max tokens for the next session"
                        .into(),
                ),
            })]);
        }
        self.settings.max_tokens = max_tokens;
        Ok(vec![self.setting_status("max tokens updated")])
    }

    fn setting_status(&self, status: &str) -> ChatEvent {
        self.status_event(status)
    }

    fn notice_event(&mut self, prefix: &str, content: String) -> ChatEvent {
        self.notice_seq = self.notice_seq.saturating_add(1);
        ChatEvent::TranscriptDelta(ChatDelta::AppendMessage {
            session_id: self.session_id.clone(),
            message: ChatMessageView {
                id: format!("{prefix}:{}", self.notice_seq),
                role: "system".into(),
                content,
                ref_: None,
            },
        })
    }

    fn model_locked(&self) -> bool {
        self.run_active()
    }

    fn run_active(&self) -> bool {
        self.turns.iter().any(|turn| {
            turn.run.as_ref().is_some_and(|run| {
                matches!(
                    run.status,
                    ChatProgressStatus::Queued
                        | ChatProgressStatus::Running
                        | ChatProgressStatus::Waiting
                )
            })
        })
    }

    fn is_quiescent(&self) -> bool {
        self.pending_run.is_none() && !self.run_active()
    }

    fn pending_run_in_flight(&self) -> bool {
        self.pending_run
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }

    fn settings_view(&self) -> ChatSettingsView {
        let run_editable = self.is_quiescent();
        let model_editable = !self.model_locked();
        ChatSettingsView {
            provider: self.settings.provider.clone(),
            api_kind: self.settings.api_kind.clone(),
            model: self.settings.model.clone(),
            reasoning_effort: self.settings.reasoning_effort,
            max_tokens: self.settings.max_tokens,
            provider_editable: model_editable,
            model_editable,
            effort_editable: run_editable,
            max_tokens_editable: run_editable,
        }
    }
}

async fn build_chat_api(options: &ChatSessionDriverOptions) -> Result<ChatAgentApi> {
    Ok(Arc::new(HttpAgentApi::new(options.api_url.clone())))
}

fn project_turns(session: &SessionView, settings: &ChatDraftSettings) -> Vec<ChatTurn> {
    session
        .runs
        .iter()
        .map(|run| {
            let user = run
                .input
                .iter()
                .enumerate()
                .find_map(|(index, item)| match item {
                    InputItem::Text { text } => Some(ChatMessageView {
                        id: format!("{}:input:{index}", run.id),
                        role: "user".into(),
                        content: text.clone(),
                        ref_: None,
                    }),
                    InputItem::TextRef { blob_ref } => Some(ChatMessageView {
                        id: format!("{}:input:{index}", run.id),
                        role: "user".into(),
                        content: format!("text input ref {blob_ref}"),
                        ref_: Some(blob_ref.clone()),
                    }),
                });
            let assistant = run.items.iter().rev().find_map(|item| match item {
                SessionItemView::AssistantMessage { id, text } => Some(ChatMessageView {
                    id: id.clone(),
                    role: "assistant".into(),
                    content: text.clone(),
                    ref_: None,
                }),
                _ => None,
            });
            let assistant_reasoning = run.items.iter().rev().find_map(|item| match item {
                SessionItemView::SystemEvent { id, text } if displayable_reasoning_text(text) => {
                    Some(ChatReasoningView {
                        id: id.clone(),
                        content: text.clone(),
                        ref_: None,
                        output_ref: None,
                    })
                }
                _ => None,
            });
            ChatTurn {
                turn_id: run.id.clone(),
                user,
                assistant_reasoning,
                assistant,
                run: Some(run_event_from_view(run, settings, run_seq_from_id(&run.id)).run()),
                tool_chains: project_tool_chains(run),
            }
        })
        .collect()
}

fn project_active_tool_chains(
    session: &SessionView,
    _settings: &ChatDraftSettings,
) -> Vec<ChatToolChainView> {
    session
        .runs
        .iter()
        .filter(|run| matches!(run.status, api::RunStatus::Running))
        .flat_map(project_tool_chains)
        .filter(|chain| !tool_chain_terminal(chain))
        .collect()
}

fn project_tool_chains(run: &api::RunView) -> Vec<ChatToolChainView> {
    run.tool_batches
        .iter()
        .map(|batch| project_tool_batch(&run.id, batch))
        .collect()
}

fn project_tool_batch(run_id: &str, batch: &ToolBatchView) -> ChatToolChainView {
    let calls = batch
        .calls
        .iter()
        .enumerate()
        .map(|(index, call)| tool_call_from_batch(index, call))
        .collect::<Vec<_>>();
    ChatToolChainView {
        id: format!("{run_id}:{}", batch.id),
        title: format!("tools {} calls", calls.len()),
        status: tool_status(batch.status),
        reasoning: None,
        summary: tool_activity_summary(&calls).or_else(|| Some("tools".into())),
        calls,
    }
}

fn event_needs_snapshot(kind: &SessionEventKindView) -> bool {
    matches!(
        kind,
        SessionEventKindView::ItemsRecorded { .. }
            | SessionEventKindView::RunCompleted { .. }
            | SessionEventKindView::RunFailed { .. }
            | SessionEventKindView::RunCancelled { .. }
            | SessionEventKindView::ToolBatchCompleted { .. }
            | SessionEventKindView::CompactionRecorded { .. }
    )
}

fn tool_call_from_event(index: usize, call: &ToolCallEventView) -> ChatToolCallView {
    ChatToolCallView {
        id: call.call_id.clone(),
        tool_id: None,
        tool_name: call.tool_name.clone(),
        status: ChatProgressStatus::Running,
        group_index: Some(index as u64 + 1),
        parallel_safe: None,
        resource_key: call
            .arguments
            .as_deref()
            .and_then(resource_key_from_arguments),
        arguments_preview: call.arguments.as_ref().map(|value| preview(value)),
        result_preview: None,
        error: None,
        display: call.display.as_ref().map(tool_display_from_api),
    }
}

fn tool_call_from_batch(index: usize, call: &ToolCallView) -> ChatToolCallView {
    ChatToolCallView {
        id: call.call_id.clone(),
        tool_id: None,
        tool_name: call.tool_name.clone(),
        status: tool_status(call.status),
        group_index: Some(index as u64 + 1),
        parallel_safe: None,
        resource_key: call
            .arguments
            .as_deref()
            .and_then(resource_key_from_arguments),
        arguments_preview: call.arguments.as_ref().map(|value| preview(value)),
        result_preview: call.output.as_ref().map(|value| preview(value)),
        error: call.is_error.then(|| call.output.clone()).flatten(),
        display: call.display.as_ref().map(tool_display_from_api),
    }
}

fn tool_display_from_api(display: &api::ToolCallDisplayView) -> ChatToolCallDisplayView {
    ChatToolCallDisplayView {
        group: match display.group {
            api::ToolCallDisplayGroup::Explore => ChatToolDisplayGroup::Explore,
            api::ToolCallDisplayGroup::Edit => ChatToolDisplayGroup::Edit,
            api::ToolCallDisplayGroup::Execute => ChatToolDisplayGroup::Execute,
            api::ToolCallDisplayGroup::Other => ChatToolDisplayGroup::Other,
        },
        verb: display.verb.clone(),
        target: display.target.clone(),
        detail: display.detail.clone(),
    }
}

fn tool_activity_summary(calls: &[ChatToolCallView]) -> Option<String> {
    let mut groups = calls.iter().map(|call| {
        call.display
            .as_ref()
            .map(|display| display.group)
            .unwrap_or(ChatToolDisplayGroup::Other)
    });
    let first = groups.next()?;
    if groups.any(|group| group != first) {
        return Some("mixed".into());
    }
    Some(
        match first {
            ChatToolDisplayGroup::Explore => "explore",
            ChatToolDisplayGroup::Edit => "edit",
            ChatToolDisplayGroup::Execute => "execute",
            ChatToolDisplayGroup::Other => "tools",
        }
        .into(),
    )
}

fn tool_status(status: ToolItemStatus) -> ChatProgressStatus {
    match status {
        ToolItemStatus::Requested | ToolItemStatus::Running => ChatProgressStatus::Running,
        ToolItemStatus::Succeeded => ChatProgressStatus::Succeeded,
        ToolItemStatus::Failed | ToolItemStatus::Unavailable => ChatProgressStatus::Failed,
    }
}

fn tool_chain_terminal(chain: &ChatToolChainView) -> bool {
    matches!(
        chain.status,
        ChatProgressStatus::Succeeded
            | ChatProgressStatus::Failed
            | ChatProgressStatus::Cancelled
            | ChatProgressStatus::Stale
    )
}

fn summary_from_session(session: &SessionView) -> ChatSessionSummary {
    ChatSessionSummary {
        session_id: session.id.clone(),
        status: Some(session.status),
        lifecycle: Some(session_lifecycle(session.status)),
        updated_at_ns: Some(session.updated_at_ms.saturating_mul(1_000_000)),
        run_count: session.runs.len() as u64,
        provider: session
            .config
            .as_ref()
            .map(|config| config.model.provider_id.clone()),
        model: session
            .config
            .as_ref()
            .map(|config| config.model.model.clone()),
        active_run: session
            .runs
            .iter()
            .find(|run| matches!(run.status, api::RunStatus::Running))
            .map(|run| run.id.clone()),
    }
}

fn run_event_from_view(
    run: &api::RunView,
    settings: &ChatDraftSettings,
    fallback_seq: u64,
) -> ChatEvent {
    ChatEvent::RunChanged(ChatRunView {
        id: run.id.clone(),
        run_seq: run_seq_from_id(&run.id).max(fallback_seq),
        lifecycle: run.status,
        status: run_status(run.status),
        provider: settings.provider.clone(),
        model: settings.model.clone(),
        reasoning_effort: settings.reasoning_effort,
        input_refs: Vec::new(),
        output_ref: None,
        started_at_ns: 0,
        updated_at_ns: 0,
    })
}

trait ChatRunEventExt {
    fn run(self) -> ChatRunView;
}

impl ChatRunEventExt for ChatEvent {
    fn run(self) -> ChatRunView {
        match self {
            ChatEvent::RunChanged(run) => run,
            _ => unreachable!("run_event_from_view always returns RunChanged"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct InactivityDeadline {
    timeout: Duration,
    deadline: Instant,
}

impl InactivityDeadline {
    fn new(now: Instant, timeout: Duration) -> Self {
        Self {
            timeout,
            deadline: now + timeout,
        }
    }

    fn record_activity(&mut self, now: Instant) {
        self.deadline = now + self.timeout;
    }

    fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }
}

fn should_timeout_after_inactivity(
    deadline: &InactivityDeadline,
    now: Instant,
    pending_run_in_flight: bool,
) -> bool {
    deadline.expired(now) && !pending_run_in_flight
}

fn session_status_text(status: api::SessionStatus) -> &'static str {
    match status {
        api::SessionStatus::NotLoaded => "not loaded",
        api::SessionStatus::Idle => "idle",
        api::SessionStatus::Active => "active",
        api::SessionStatus::Closed => "closed",
        api::SessionStatus::Error => "error",
    }
}

fn draft_settings(args: &ChatArgs) -> Result<ChatDraftSettings> {
    let reasoning_effort = match args.effort.as_deref() {
        Some(value) => crate::chat::protocol::parse_reasoning_effort(value)?,
        None => Some(DEFAULT_CHAT_REASONING_EFFORT),
    };

    Ok(ChatDraftSettings {
        provider: args.provider.clone(),
        api_kind: args.api_kind.clone(),
        model: args.model.clone(),
        reasoning_effort,
        max_tokens: args.max_tokens,
    })
}

fn model_config(settings: &ChatDraftSettings) -> ModelConfig {
    ModelConfig {
        provider_id: settings.provider.clone(),
        api_kind: settings.api_kind.clone(),
        model: settings.model.clone(),
    }
}

fn session_start_config(settings: &ChatDraftSettings) -> SessionConfigInput {
    SessionConfigInput {
        instructions: None,
        model: Some(model_config(settings)),
        generation: Some(generation_config(settings)),
        context: None,
        run_defaults: None,
    }
}

fn run_start_config(settings: &ChatDraftSettings) -> RunStartConfig {
    RunStartConfig {
        model: Some(model_config(settings)),
        generation: Some(generation_config(settings)),
        limits: None,
    }
}

fn generation_config(settings: &ChatDraftSettings) -> GenerationConfig {
    GenerationConfig {
        max_output_tokens: settings.max_tokens,
        reasoning_effort: api_reasoning_effort(settings),
    }
}

fn api_reasoning_effort(settings: &ChatDraftSettings) -> Option<ApiReasoningEffort> {
    if settings.api_kind != "openai:responses" {
        return None;
    }
    Some(match settings.reasoning_effort {
        None => ApiReasoningEffort::None,
        Some(crate::chat::protocol::ReasoningEffort::Low) => ApiReasoningEffort::Low,
        Some(crate::chat::protocol::ReasoningEffort::Medium) => ApiReasoningEffort::Medium,
        Some(crate::chat::protocol::ReasoningEffort::High) => ApiReasoningEffort::High,
    })
}

fn resolve_chat_workdir(workdir: Option<PathBuf>) -> Result<String> {
    let path = match workdir {
        Some(path) if path.is_absolute() => path,
        Some(path) => std::env::current_dir()
            .context("resolve current directory")?
            .join(path),
        None => std::env::current_dir().context("resolve current directory")?,
    };
    let path = path
        .canonicalize()
        .with_context(|| format!("resolve chat workdir '{}'", path.display()))?;
    Ok(path.to_string_lossy().into_owned())
}

fn print_event(event: &ChatEvent) -> Result<()> {
    match event {
        ChatEvent::Connected(info) => {
            println!(
                "connected session={} model={}",
                info.session_id, info.settings.model
            );
        }
        ChatEvent::SessionsListed { sessions, .. } => {
            for session in sessions {
                let status = session.status.map(session_status_text).unwrap_or("unknown");
                println!("{} {status}", session.session_id);
            }
        }
        ChatEvent::SkillsListed { .. } => {}
        ChatEvent::SessionSelected(summary) => {
            let status = summary.status.map(session_status_text).unwrap_or("unknown");
            println!(
                "session {} {} runs={}",
                summary.session_id, status, summary.run_count
            );
        }
        ChatEvent::HistoryReset { session_id } => {
            println!("switched to session {session_id}");
        }
        ChatEvent::TranscriptDelta(ChatDelta::ReplaceTurns { turns, .. }) => {
            if let Some(turn) = turns.last()
                && let Some(message) = &turn.assistant
            {
                println!("\nassistant: {}\n", message.content);
            }
        }
        ChatEvent::TranscriptDelta(ChatDelta::AppendMessage { .. }) => {}
        ChatEvent::RunChanged(run) => {
            println!("run {} {}", run.id, progress_label(run.status));
        }
        ChatEvent::ToolChainsChanged { .. }
        | ChatEvent::CompactionsChanged { .. }
        | ChatEvent::GapObserved { .. }
        | ChatEvent::Reconnecting { .. } => {}
        ChatEvent::StatusChanged(status) => {
            eprintln!("status: {}", status.status);
        }
        ChatEvent::Error(error) => {
            eprintln!("error: {}", error.message);
            if let Some(action) = &error.action {
                eprintln!("action: {action}");
            }
        }
    }
    Ok(())
}

fn progress_label(status: ChatProgressStatus) -> &'static str {
    match status {
        ChatProgressStatus::Queued => "queued",
        ChatProgressStatus::Running => "running",
        ChatProgressStatus::Waiting => "waiting",
        ChatProgressStatus::Succeeded => "done",
        ChatProgressStatus::Failed => "failed",
        ChatProgressStatus::Cancelled => "cancelled",
        ChatProgressStatus::Stale => "stale",
        ChatProgressStatus::Unknown => "unknown",
    }
}

fn format_skill_list(response: &api::SkillListResponse) -> String {
    let mut lines = vec![format_catalog_ref(response.catalog_ref.as_deref())];
    if response.skills.is_empty() {
        lines.push("skills 0".into());
        return lines.join("\n");
    }

    lines.push(format!("skills {}", response.skills.len()));
    for skill in &response.skills {
        let active = if skill.active { "active" } else { "inactive" };
        let enabled = if skill.enabled { "enabled" } else { "disabled" };
        lines.push(format!(
            "- {} [{} {}] {}",
            skill.skill_id, active, enabled, skill.name
        ));
        if !skill.description.trim().is_empty() {
            lines.push(format!("  {}", preview(&skill.description)));
        }
        if let Some(short_description) = &skill.short_description {
            lines.push(format!("  short {}", preview(short_description)));
        }
    }
    lines.join("\n")
}

fn format_active_skills(response: &api::SkillActiveResponse) -> String {
    let mut lines = vec![format_catalog_ref(response.catalog_ref.as_deref())];
    if response.activations.is_empty() {
        lines.push("active 0".into());
        return lines.join("\n");
    }

    lines.push(format!("active {}", response.activations.len()));
    for activation in &response.activations {
        push_skill_activation_lines(&mut lines, activation);
    }
    lines.join("\n")
}

fn format_skill_activation_response(response: &api::SkillActivateResponse) -> String {
    let mut lines = vec![format!(
        "activated {} ({})",
        response.activation.skill_id,
        skill_scope_label(response.activation.scope)
    )];
    push_skill_activation_lines(&mut lines, &response.activation);
    lines.push(format!("active {}", response.active.len()));
    lines.join("\n")
}

fn format_skill_deactivation_response(response: &api::SkillDeactivateResponse) -> String {
    [
        format!("deactivated {}", response.skill_id),
        format!("active {}", response.active.len()),
    ]
    .join("\n")
}

fn push_skill_activation_lines(lines: &mut Vec<String>, activation: &api::SkillActivationView) {
    let name = activation.name.as_deref().unwrap_or("-");
    lines.push(format!(
        "- {} [{} {}] {}",
        activation.skill_id,
        skill_scope_label(activation.scope),
        skill_source_label(&activation.source),
        name
    ));
    if let Some(description) = &activation.description
        && !description.trim().is_empty()
    {
        lines.push(format!("  {}", preview(description)));
    }
    lines.push(format!("  catalogRef {}", activation.catalog_ref));
}

fn format_catalog_ref(catalog_ref: Option<&str>) -> String {
    format!("catalogRef {}", catalog_ref.unwrap_or("-"))
}

fn skill_scope_label(scope: api::SkillActivationScope) -> &'static str {
    match scope {
        api::SkillActivationScope::Run => "run",
        api::SkillActivationScope::Session => "session",
    }
}

fn skill_source_label(source: &api::SkillActivationSource) -> String {
    match source {
        api::SkillActivationSource::ToolResult { call_id } => format!("toolResult:{call_id}"),
        api::SkillActivationSource::DirectContext { context_ref } => {
            format!("directContext:{context_ref}")
        }
    }
}

fn preview(value: &str) -> String {
    compact_preview(value, 180)
}

fn resource_key_from_arguments(value: &str) -> Option<String> {
    let json = serde_json::from_str::<Value>(value).ok()?;
    ["path", "file", "cwd", "command", "cmd"]
        .into_iter()
        .find_map(|key| json.get(key).and_then(Value::as_str).map(str::to_owned))
}

fn run_seq_from_id(id: &str) -> u64 {
    id.strip_prefix("run_")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default()
}

fn displayable_reasoning_text(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    if lower == "context item"
        || lower == "reasoning state"
        || lower.starts_with("reasoning state rs_")
        || lower == "compaction state"
        || lower.starts_with("compaction state ")
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_tool_chains_preserves_forge_tool_call_details() {
        let run = api::RunView {
            id: "run_7".into(),
            status: api::RunStatus::Running,
            input: Vec::new(),
            items: Vec::new(),
            tool_batches: vec![ToolBatchView {
                id: "tool_batch_1".into(),
                turn_id: "turn_1".into(),
                status: ToolItemStatus::Succeeded,
                calls: vec![ToolCallView {
                    call_id: "call_1".into(),
                    tool_name: "read_file".into(),
                    arguments_ref: "sha256:args".into(),
                    arguments: Some(r#"{"path":"README.md"}"#.into()),
                    output: Some(r#"{"ok":true}"#.into()),
                    is_error: false,
                    status: ToolItemStatus::Succeeded,
                    effects: Vec::new(),
                    display: Some(api::ToolCallDisplayView {
                        group: api::ToolCallDisplayGroup::Explore,
                        verb: "Read".into(),
                        target: Some("README.md".into()),
                        detail: None,
                    }),
                }],
            }],
        };

        let chains = project_tool_chains(&run);

        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].id, "run_7:tool_batch_1");
        assert_eq!(chains[0].title, "tools 1 calls");
        assert_eq!(chains[0].status, ChatProgressStatus::Succeeded);
        assert_eq!(chains[0].calls[0].tool_name, "read_file");
        assert_eq!(
            chains[0].calls[0].resource_key.as_deref(),
            Some("README.md")
        );
        assert_eq!(
            chains[0].calls[0].result_preview.as_deref(),
            Some(r#"{"ok":true}"#)
        );
        assert_eq!(
            chains[0].calls[0]
                .display
                .as_ref()
                .and_then(|display| display.target.as_deref()),
            Some("README.md")
        );
    }

    #[test]
    fn project_active_tool_chains_ignores_terminal_batches() {
        fn call(call_id: &str, status: ToolItemStatus) -> ToolCallView {
            ToolCallView {
                call_id: call_id.into(),
                tool_name: "read_file".into(),
                arguments_ref: "sha256:args".into(),
                arguments: Some(r#"{"path":"README.md"}"#.into()),
                output: None,
                is_error: false,
                status,
                effects: Vec::new(),
                display: None,
            }
        }

        let session = SessionView {
            id: "session_1".into(),
            status: api::SessionStatus::Active,
            cwd: None,
            config_revision: 0,
            config: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            runs: vec![api::RunView {
                id: "run_1".into(),
                status: api::RunStatus::Running,
                input: Vec::new(),
                items: Vec::new(),
                tool_batches: vec![
                    ToolBatchView {
                        id: "tool_batch_1".into(),
                        turn_id: "turn_1".into(),
                        status: ToolItemStatus::Succeeded,
                        calls: vec![call("call_1", ToolItemStatus::Succeeded)],
                    },
                    ToolBatchView {
                        id: "tool_batch_2".into(),
                        turn_id: "turn_2".into(),
                        status: ToolItemStatus::Running,
                        calls: vec![call("call_2", ToolItemStatus::Running)],
                    },
                ],
            }],
            vfs_mounts: Vec::new(),
        };

        let settings = ChatDraftSettings {
            provider: "openai".into(),
            api_kind: "openai:responses".into(),
            model: "gpt-5.5".into(),
            reasoning_effort: None,
            max_tokens: None,
        };

        let chains = project_active_tool_chains(&session, &settings);

        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].id, "run_1:tool_batch_2");
        assert_eq!(chains[0].status, ChatProgressStatus::Running);
    }

    #[test]
    fn formats_skill_list_for_transcript_notice() {
        let response = api::SkillListResponse {
            catalog_ref: Some("sha256:catalog".into()),
            skills: vec![api::SkillListItem {
                skill_id: "forge:review".into(),
                name: "Review".into(),
                description: "Review repository changes.".into(),
                short_description: Some("review diffs".into()),
                enabled: true,
                active: true,
            }],
        };

        let rendered = format_skill_list(&response);

        assert!(rendered.contains("catalogRef sha256:catalog"));
        assert!(rendered.contains("- forge:review [active enabled] Review"));
        assert!(rendered.contains("Review repository changes."));
        assert!(rendered.contains("short review diffs"));
    }

    #[test]
    fn formats_active_skills_for_transcript_notice() {
        let response = api::SkillActiveResponse {
            catalog_ref: Some("sha256:catalog".into()),
            activations: vec![api::SkillActivationView {
                skill_id: "forge:review".into(),
                name: Some("Review".into()),
                description: Some("Review repository changes.".into()),
                short_description: None,
                catalog_ref: "sha256:catalog".into(),
                scope: api::SkillActivationScope::Session,
                source: api::SkillActivationSource::DirectContext {
                    context_ref: "sha256:skill-doc".into(),
                },
            }],
        };

        let rendered = format_active_skills(&response);

        assert!(rendered.contains("active 1"));
        assert!(
            rendered.contains("- forge:review [session directContext:sha256:skill-doc] Review")
        );
        assert!(rendered.contains("catalogRef sha256:catalog"));
    }

    #[test]
    fn project_turns_prefers_visible_reasoning_summary_over_opaque_state() {
        let session = SessionView {
            id: "session_1".into(),
            status: api::SessionStatus::Idle,
            cwd: None,
            config_revision: 0,
            config: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            runs: vec![api::RunView {
                id: "run_1".into(),
                status: api::RunStatus::Completed,
                input: Vec::new(),
                items: vec![
                    SessionItemView::SystemEvent {
                        id: "item_1".into(),
                        text: "I should inspect the crate layout first.".into(),
                    },
                    SessionItemView::SystemEvent {
                        id: "item_2".into(),
                        text: "reasoning state rs_abc123".into(),
                    },
                ],
                tool_batches: Vec::new(),
            }],
            vfs_mounts: Vec::new(),
        };
        let settings = ChatDraftSettings {
            provider: "openai".into(),
            api_kind: "openai:responses".into(),
            model: "gpt-5.5".into(),
            reasoning_effort: None,
            max_tokens: None,
        };

        let turns = project_turns(&session, &settings);

        assert_eq!(
            turns[0]
                .assistant_reasoning
                .as_ref()
                .map(|reasoning| reasoning.content.as_str()),
            Some("I should inspect the crate layout first.")
        );
    }

    #[test]
    fn project_turns_hides_opaque_reasoning_state_markers() {
        let session = SessionView {
            id: "session_1".into(),
            status: api::SessionStatus::Idle,
            cwd: None,
            config_revision: 0,
            config: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            runs: vec![api::RunView {
                id: "run_1".into(),
                status: api::RunStatus::Completed,
                input: Vec::new(),
                items: vec![SessionItemView::SystemEvent {
                    id: "item_1".into(),
                    text: "reasoning state rs_abc123".into(),
                }],
                tool_batches: Vec::new(),
            }],
            vfs_mounts: Vec::new(),
        };
        let settings = ChatDraftSettings {
            provider: "openai".into(),
            api_kind: "openai:responses".into(),
            model: "gpt-5.5".into(),
            reasoning_effort: None,
            max_tokens: None,
        };

        let turns = project_turns(&session, &settings);

        assert!(turns[0].assistant_reasoning.is_none());
    }

    #[test]
    fn run_seq_from_id_reads_forge_api_run_ids() {
        assert_eq!(run_seq_from_id("run_42"), 42);
        assert_eq!(run_seq_from_id("other"), 0);
    }

    #[test]
    fn inactivity_deadline_resets_on_activity() {
        let start = Instant::now();
        let mut deadline = InactivityDeadline::new(start, Duration::from_secs(10));

        deadline.record_activity(start + Duration::from_secs(8));

        assert!(!deadline.expired(start + Duration::from_secs(17)));
        assert!(deadline.expired(start + Duration::from_secs(18)));
    }

    #[test]
    fn inactivity_timeout_waits_for_in_flight_run_task() {
        let start = Instant::now();
        let deadline = InactivityDeadline::new(start, Duration::from_secs(10));
        let expired = start + Duration::from_secs(11);

        assert!(!should_timeout_after_inactivity(&deadline, expired, true));
        assert!(should_timeout_after_inactivity(&deadline, expired, false));
    }

    #[test]
    fn draft_settings_defaults_reasoning_effort_to_high() {
        let settings = draft_settings(&chat_args_with_effort(None)).expect("draft settings");

        assert_eq!(
            settings.reasoning_effort,
            Some(crate::chat::protocol::ReasoningEffort::High)
        );
    }

    #[test]
    fn draft_settings_can_disable_reasoning_effort() {
        let settings =
            draft_settings(&chat_args_with_effort(Some("none"))).expect("draft settings");

        assert_eq!(settings.reasoning_effort, None);
    }

    #[test]
    fn run_start_config_sends_model_generation_and_disabled_reasoning() {
        let mut settings =
            draft_settings(&chat_args_with_effort(Some("none"))).expect("draft settings");
        settings.max_tokens = Some(2048);

        let config = run_start_config(&settings);

        assert_eq!(config.model.expect("model").model, "gpt-5.5");
        let generation = config.generation.expect("generation");
        assert_eq!(generation.max_output_tokens, Some(2048));
        assert_eq!(generation.reasoning_effort, Some(ApiReasoningEffort::None));
    }

    #[test]
    fn run_start_config_omits_reasoning_for_non_responses_api_kinds() {
        let mut settings =
            draft_settings(&chat_args_with_effort(Some("high"))).expect("draft settings");
        settings.api_kind = "anthropic:messages".to_owned();

        let config = run_start_config(&settings);

        assert_eq!(
            config.generation.expect("generation").reasoning_effort,
            None
        );
    }

    fn chat_args_with_effort(effort: Option<&str>) -> ChatArgs {
        ChatArgs {
            session: None,
            new: true,
            provider: "openai".into(),
            api_kind: "openai:responses".into(),
            model: "gpt-5.5".into(),
            effort: effort.map(str::to_string),
            max_tokens: None,
            workdir: None,
            mount: None,
            mount_path: "/workspace".into(),
            api_url: "http://127.0.0.1:18080/rpc".into(),
            show_tool_details: false,
            json: false,
            message: Vec::new(),
        }
    }

    #[test]
    fn tool_call_from_event_uses_inline_arguments_for_active_tui_cell() {
        let call = tool_call_from_event(
            0,
            &ToolCallEventView {
                call_id: "call_1".into(),
                tool_name: "read_file".into(),
                arguments_ref: "sha256:args".into(),
                arguments: Some(r#"{"path":"src/lib.rs"}"#.into()),
                display: Some(api::ToolCallDisplayView {
                    group: api::ToolCallDisplayGroup::Explore,
                    verb: "Read".into(),
                    target: Some("src/lib.rs".into()),
                    detail: None,
                }),
            },
        );

        assert_eq!(call.status, ChatProgressStatus::Running);
        assert_eq!(call.resource_key.as_deref(), Some("src/lib.rs"));
        assert_eq!(
            call.arguments_preview.as_deref(),
            Some(r#"{"path":"src/lib.rs"}"#)
        );
        assert_eq!(
            call.display.as_ref().map(|display| display.verb.as_str()),
            Some("Read")
        );
    }

    #[test]
    fn terminal_event_kinds_request_snapshot_reconciliation() {
        assert!(event_needs_snapshot(&SessionEventKindView::RunCompleted {
            run_id: "run_1".into(),
            output_ref: None,
        }));
        assert!(event_needs_snapshot(&SessionEventKindView::ItemsRecorded {
            items: Vec::new(),
        }));
        assert!(!event_needs_snapshot(&SessionEventKindView::RunStarted {
            run_id: "run_1".into(),
            submission_id: None,
            input_ref: "sha256:input".into(),
        }));
    }
}

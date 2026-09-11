use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use api::{
    AgentApiOutcome, EventCursor, FeaturesConfig, GenerationConfig, InlineAgentProfile, InputItem,
    ModelConfig, ProfileId, ProfileSource, RunStartConfig, RunStartParams, RunStartResponse,
    RunStartSource, SessionEventKindView, SessionEventView, SessionEventsReadParams,
    SessionReadParams, SessionStartParams, SessionView, TimersFeature, ToolCallEventView,
    VfsFeature, VfsPromptsConfig, VfsToolSurface, WebFeature, WebFetchFeature, WebSearchFeature,
};
#[cfg(test)]
use api::{ContextEntryKindView, ContextEntryView, ToolBatchView, ToolCallView, ToolItemStatus};
use clap::Args;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::api_client::{HttpAgentApi, api_error};
use crate::chat::preview::compact_preview;
use crate::chat::protocol::{
    ChatCommand, ChatConnectionInfo, ChatDelta, ChatDraftSettings, ChatErrorView, ChatEvent,
    ChatMessageView, ChatProgressStatus, ChatRunView, ChatSessionSummary, ChatSettingsView,
    ChatStatus, ChatToolCallDisplayView, ChatToolCallView, ChatToolChainView, ChatToolDisplayGroup,
    ChatTurn, DEFAULT_CHAT_REASONING_EFFORT, GATEWAY_WORLD_ID, run_status, session_lifecycle,
};
use crate::chat::session::{new_session_id, new_submission_id, validate_session_id};

#[derive(Args, Debug, Clone)]
pub(crate) struct ChatArgs {
    /// Session ID to open or create through the configured Lightspeed API.
    #[arg(long)]
    session: Option<String>,
    /// Start with a fresh session ID.
    #[arg(long)]
    new: bool,
    /// Provider ID for the model adapter.
    #[arg(
        long,
        env = "LIGHTSPEED_CHAT_PROVIDER",
        default_value = crate::chat::protocol::DEFAULT_CHAT_PROVIDER
    )]
    provider: String,
    /// Provider API kind.
    #[arg(
        long = "api-kind",
        env = "LIGHTSPEED_CHAT_API_KIND",
        default_value = crate::chat::protocol::DEFAULT_CHAT_API_KIND
    )]
    api_kind: String,
    /// Model name.
    #[arg(
        long,
        env = "LIGHTSPEED_CHAT_MODEL",
        default_value = crate::chat::protocol::DEFAULT_CHAT_MODEL
    )]
    model: String,
    /// Reasoning effort: low, medium, high, or none.
    #[arg(long, env = "LIGHTSPEED_CHAT_REASONING_EFFORT", default_value = "high")]
    effort: Option<String>,
    /// Max output token limit.
    #[arg(long, env = "LIGHTSPEED_CHAT_MAX_TOKENS")]
    max_tokens: Option<u32>,
    /// Disable provider-hosted web search for this session.
    #[arg(long = "no-web-search")]
    no_web_search: bool,
    /// Disable web fetch for this session.
    #[arg(long = "no-web-fetch")]
    no_web_fetch: bool,
    /// Filesystem tool mode for this session: edit, read-only, or none.
    #[arg(long = "filesystem-tools")]
    filesystem_tools: Option<String>,
    /// Start with no feature grants at all (model + runs only) instead of
    /// the CLI's dev defaults (vfs, web, timers).
    #[arg(long)]
    bare: bool,
    /// Start a new session from a named agent profile.
    #[arg(long)]
    profile: Option<String>,
    /// Start a new session from an inline agent profile JSON file or literal.
    #[arg(long = "profile-json")]
    profile_json: Option<String>,
    /// Snapshot a local directory, create a VFS workspace, and mount it for this chat.
    #[arg(long)]
    mount: Option<PathBuf>,
    /// VFS path used for --mount. Defaults to /workspace.
    #[arg(long = "mount-path", default_value = "/workspace")]
    mount_path: String,
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
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
    let profile = profile_source_from_args(args.profile.as_deref(), args.profile_json.as_deref())?;
    let mount = args.mount.clone();
    let mount_path = args.mount_path.clone();
    let session_id = if args.new {
        new_session_id()
    } else if let Some(session_id) = args.session.as_ref() {
        validate_session_id(session_id)?
    } else {
        new_session_id()
    };

    let message = (!args.message.is_empty()).then(|| args.message.join(" "));
    let (mut driver, mut initial_events) = ChatSessionDriver::open(ChatSessionDriverOptions {
        session_id,
        draft_settings: draft,
        api_url: args.api_url,
        profile,
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

fn profile_source_from_args(
    profile: Option<&str>,
    profile_json: Option<&str>,
) -> Result<Option<ProfileSource>> {
    match (profile, profile_json) {
        (Some(_), Some(_)) => Err(anyhow!(
            "--profile and --profile-json are mutually exclusive"
        )),
        (Some(profile_id), None) => Ok(Some(ProfileSource::Named {
            profile_id: ProfileId::try_new(profile_id.to_owned())
                .map_err(|error| anyhow!("invalid profile id: {error}"))?,
        })),
        (None, Some(json_arg)) => {
            let path = PathBuf::from(json_arg);
            let json = if path.exists() {
                std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read profile JSON {}", path.display()))?
            } else {
                json_arg.to_owned()
            };
            let profile: InlineAgentProfile =
                serde_json::from_str(&json).context("failed to parse inline profile JSON")?;
            Ok(Some(ProfileSource::Inline {
                profile: Box::new(profile),
            }))
        }
        (None, None) => Ok(None),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ChatSessionDriverOptions {
    pub session_id: String,
    pub draft_settings: ChatDraftSettings,
    pub api_url: String,
    pub profile: Option<ProfileSource>,
}

pub(crate) struct ChatSessionDriver {
    api: ChatAgentApi,
    session_id: String,
    settings: ChatDraftSettings,
    event_cursor: Option<EventCursor>,
    turns: Vec<ChatTurn>,
    active_tool_chains: Vec<ChatToolChainView>,
    /// Run lifecycle facts keyed by run sequence, fed by the event tail and
    /// reconciled against `session/read`; `/steer`, `/interrupt`, and the
    /// model lock derive the active run from this, not the transcript.
    run_states: BTreeMap<u64, TrackedRun>,
    sessions: BTreeSet<String>,
    pending_run: Option<PendingRunHandle>,
    notice_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrackedRun {
    id: String,
    status: api::RunStatus,
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
                metadata: Default::default(),
                session_id: Some(session_id.clone()),
                display_name: None,
                config: Some(session_start_config(&options.draft_settings)),
                profile: options.profile.clone(),
                environment: None,
                delete_after_close_ms: None,
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
            run_states: BTreeMap::new(),
            sessions: BTreeSet::from([session_id.clone()]),
            pending_run: None,
            notice_seq: 0,
        };
        let mut events = vec![ChatEvent::Connected(ChatConnectionInfo {
            world_id: GATEWAY_WORLD_ID.into(),
            session_id,
            journal_next_from: None,
            settings: driver.settings_view(),
        })];
        events.push(ChatEvent::SessionSelected(summary_from_mutation(
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
            mount_path,
            workspace.workspace_id,
        )
        .await
        .context("failed to mount chat workspace")?;
        self.refresh().await
    }

    pub(crate) async fn handle_command(&mut self, command: ChatCommand) -> Result<Vec<ChatEvent>> {
        match command {
            ChatCommand::SubmitUserMessage { text } => self.submit_user_message(text).await,
            ChatCommand::SetDraftProvider { provider } => self.set_provider(provider).await,
            ChatCommand::SetDraftModel { model } => self.set_model(model).await,
            ChatCommand::SetDraftReasoningEffort { effort } => self.set_effort(effort).await,
            ChatCommand::SetDraftMaxTokens { max_tokens } => self.set_max_tokens(max_tokens).await,
            ChatCommand::ListSessions => {
                let listed = self
                    .api
                    .list_sessions(api::SessionListParams::default())
                    .await
                    .map_err(api_error)?;
                Ok(vec![ChatEvent::SessionsListed {
                    world_id: GATEWAY_WORLD_ID.into(),
                    sessions: listed
                        .result
                        .sessions
                        .iter()
                        .map(|session| ChatSessionSummary {
                            session_id: session.id.clone(),
                            status: None,
                            lifecycle: None,
                            updated_at_ns: Some(session.updated_at_ms.saturating_mul(1_000_000)),
                            run_count: 0,
                            provider: None,
                            model: None,
                            active_run: None,
                        })
                        .collect(),
                }])
            }
            ChatCommand::ListSkills => self.list_skills().await,
            ChatCommand::PickSkill => self.pick_skill().await,
            ChatCommand::UseSkill { skill_id } => self.use_skill(skill_id).await,
            ChatCommand::NewSession => self.new_session().await,
            ChatCommand::SteerRun { text } => self.steer_active_run(text).await,
            ChatCommand::InterruptRun { .. } => self.cancel_active_run().await,
            ChatCommand::DecideApproval {
                approval_id,
                decision,
                note,
            } => self.decide_approval(approval_id, decision, note).await,
            ChatCommand::PauseSession | ChatCommand::ResumeSession => {
                Ok(vec![ChatEvent::Error(ChatErrorView {
                    message: "pause/resume is not implemented for Lightspeed API sessions".into(),
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
        const FOLLOW_EVENT_WAIT_MS: u64 = 2_000;
        let mut inactivity_deadline = InactivityDeadline::new(Instant::now(), timeout);
        let mut wait_ms = None;
        loop {
            let events = self.drain_event_log_with_wait(wait_ms).await?;
            // The first drain is immediate to flush backlog; subsequent
            // drains long-poll server-side instead of sleeping client-side.
            wait_ms = Some(FOLLOW_EVENT_WAIT_MS);
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
            }
            // No client-side sleep: the next drain's long-poll parks
            // server-side until events arrive or the wait elapses.
        }
    }

    /// Submit a user message as a new run. While another run is active the
    /// message is queued behind it: `session/runs/start` returns the
    /// run as `queued`, and it starts when the active run ends.
    async fn submit_user_message(&mut self, text: String) -> Result<Vec<ChatEvent>> {
        if self.pending_run_in_flight() {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "the previous message is still being submitted".into(),
                action: Some("retry in a moment".into()),
            })]);
        }
        if self.pending_run.is_some() {
            let mut events = self.collect_finished_run().await?;
            events.extend(self.submit_user_message_now(text).await?);
            return Ok(events);
        }
        self.submit_user_message_now(text).await
    }

    async fn submit_user_message_now(&mut self, text: String) -> Result<Vec<ChatEvent>> {
        let events = vec![self.status_event(if self.run_active() {
            "queued"
        } else {
            "working"
        })];

        let api = self.api.clone();
        let session_id = self.session_id.clone();
        let config = run_start_config(&self.settings);
        self.pending_run = Some(tokio::spawn(async move {
            api.start_run(RunStartParams {
                notify_on_terminal: None,
                session_id,
                source: RunStartSource::Input {
                    items: vec![InputItem::Text { origin: None, text }],
                },
                submission_id: Some(new_submission_id()),
                config: Some(config),
            })
            .await
        }));

        Ok(events)
    }

    /// The run currently executing (running or parked), if any. Queued runs
    /// do not count: they cannot be steered yet and are cancelled by id.
    fn active_run_id(&self) -> Option<String> {
        self.run_states.values().rev().find_map(|run| {
            matches!(run.status, api::RunStatus::Running | api::RunStatus::Parked)
                .then(|| run.id.clone())
        })
    }

    /// The newest run that is still queued or active: what `/interrupt`
    /// stops. Queued runs are cancelled first so a stop never lets the next
    /// message start behind the one being stopped.
    fn interruptible_run_id(&self) -> Option<String> {
        self.run_states
            .values()
            .rev()
            .find_map(|run| (run.status == api::RunStatus::Queued).then(|| run.id.clone()))
            .or_else(|| self.active_run_id())
    }

    async fn cancel_active_run(&mut self) -> Result<Vec<ChatEvent>> {
        let Some(run_id) = self.interruptible_run_id() else {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "no run is active in this session".into(),
                action: None,
            })]);
        };
        let response = self
            .api
            .cancel_run(api::RunCancelParams {
                session_id: self.session_id.clone(),
                run_id: run_id.clone(),
            })
            .await
            .map_err(api_error)?
            .result;
        let mut events = vec![self.status_event(match response.run.status {
            api::RunStatus::Cancelled => "cancelled",
            _ => "cancelling",
        })];
        events.extend(self.drain_event_log().await?);
        Ok(events)
    }

    async fn steer_active_run(&mut self, text: String) -> Result<Vec<ChatEvent>> {
        let Some(run_id) = self.active_run_id() else {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "no run is active in this session".into(),
                action: Some("send the message normally; it starts a new run".into()),
            })]);
        };
        let response = self
            .api
            .steer_run(api::RunSteerParams {
                session_id: self.session_id.clone(),
                run_id,
                items: vec![InputItem::Text { origin: None, text }],
            })
            .await
            .map_err(api_error)?
            .result;
        let mut events = vec![self.notice_event(
            "steered",
            format!(
                "steering {} accepted; the model sees it at the next turn",
                response.steering_id
            ),
        )];
        events.extend(self.drain_event_log().await?);
        Ok(events)
    }

    async fn decide_approval(
        &mut self,
        approval_id: String,
        decision: api::ApprovalDecisionKind,
        note: Option<String>,
    ) -> Result<Vec<ChatEvent>> {
        let session = self
            .api
            .read_session(SessionReadParams {
                session_id: self.session_id.clone(),
                run_limit: None,
            })
            .await
            .map_err(api_error)?
            .result
            .session;
        let run = session
            .runs
            .iter()
            .find(|run| {
                run.pending_approvals
                    .iter()
                    .any(|approval| approval.approval_id == approval_id)
            })
            .ok_or_else(|| anyhow!("pending approval not found: {approval_id}"))?;
        let response = self
            .api
            .decide_run_approvals(api::RunApprovalsDecideParams {
                session_id: self.session_id.clone(),
                run_id: run.id.clone(),
                decisions: vec![api::ApprovalDecisionInput {
                    approval_id: approval_id.clone(),
                    decision,
                    note,
                }],
            })
            .await
            .map_err(api_error)?
            .result;
        let result = response
            .results
            .first()
            .ok_or_else(|| anyhow!("approval decision returned no result"))?;
        if result.status == api::ApprovalDecisionStatus::Failed {
            return Err(anyhow!(
                "approval decision failed: {}",
                result
                    .failure
                    .as_ref()
                    .map(|failure| failure.message.as_str())
                    .unwrap_or("unknown failure")
            ));
        }
        let mut events = vec![self.notice_event(
            "approval",
            format!("{} {approval_id}", approval_decision_label(decision)),
        )];
        events.extend(self.drain_event_log().await?);
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

    async fn pick_skill(&mut self) -> Result<Vec<ChatEvent>> {
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
            catalogs: response.catalogs,
        }])
    }

    async fn use_skill(&mut self, skill_id: String) -> Result<Vec<ChatEvent>> {
        let catalog = self
            .api
            .list_skills(api::SkillListParams {
                session_id: self.session_id.clone(),
            })
            .await
            .map_err(api_error)?
            .result;
        let text = crate::skills_cli::skill_selection_input(&catalog, &skill_id)?;
        if self.active_run_id().is_some() {
            self.steer_active_run(text).await
        } else {
            self.submit_user_message(text).await
        }
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
                run_limit: None,
            })
            .await
            .map_err(api_error)?;
        let session = read.result.session;
        let old_turns = self.turns.clone();
        let old_active_tool_chains = self.active_tool_chains.clone();
        // Detailed transcript and tool state are maintained from the bounded
        // event tail. `session/read` reconciles current state and run summaries.
        self.run_states = session
            .runs
            .iter()
            .chain(session.active_run.as_ref())
            .map(|run| {
                (
                    run_seq_from_id(&run.id),
                    TrackedRun {
                        id: run.id.clone(),
                        status: run.status,
                    },
                )
            })
            .collect();

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
            .active_run
            .as_ref()
            .filter(|run| matches!(run.status, api::RunStatus::Running | api::RunStatus::Parked))
        {
            events.push(run_event_from_summary(
                active_run,
                &self.settings,
                run_seq_from_id(&active_run.id),
            ));
        }
        for run in session
            .runs
            .iter()
            .filter(|run| !run.pending_approvals.is_empty())
        {
            events.push(ChatEvent::ApprovalsPending {
                session_id: self.session_id.clone(),
                run_id: run.id.clone(),
                approvals: run.pending_approvals.clone(),
            });
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
        self.drain_event_log_with_wait(None).await
    }

    /// Drains the event log; `wait_first_ms` long-polls the first page so
    /// callers park server-side instead of sleeping between empty drains.
    async fn drain_event_log_with_wait(
        &mut self,
        wait_first_ms: Option<u64>,
    ) -> Result<Vec<ChatEvent>> {
        let mut events = Vec::new();
        let mut needs_snapshot = false;
        let mut wait_ms = wait_first_ms;
        loop {
            let page = self
                .api
                .read_session_events(SessionEventsReadParams {
                    direction: Default::default(),
                    before: None,
                    session_id: self.session_id.clone(),
                    after: self.event_cursor,
                    limit: Some(128),
                    wait_ms: wait_ms.take(),
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
            SessionEventKindView::RunAccepted { run_id, .. } => {
                events.push(ChatEvent::RunChanged(self.run_view_from_status(
                    run_id,
                    api::RunStatus::Queued,
                    event.observed_at_ms,
                )));
                events.push(self.status_event("queued"));
            }
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
            SessionEventKindView::RunFailed {
                run_id, message, ..
            } => {
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
            SessionEventKindView::PromiseCreated { .. }
            | SessionEventKindView::PromiseResolved { .. }
            | SessionEventKindView::PromiseFailed { .. }
            | SessionEventKindView::PromiseCancelled { .. }
            | SessionEventKindView::PromiseDetached { .. } => {}
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
            SessionEventKindView::RunSteeringAccepted { .. } => {
                events.push(self.status_event("steering accepted"));
            }
            SessionEventKindView::RunCancellationRequested { .. } => {
                events.push(self.status_event("cancelling"));
            }
            SessionEventKindView::ApprovalRequested { .. }
            | SessionEventKindView::ApprovalRunParked { .. } => {
                events.push(self.status_event("waiting for approval"));
            }
            SessionEventKindView::ApprovalDecided { .. }
            | SessionEventKindView::ApprovalCancelled { .. } => {
                events.push(self.status_event("approval resolved"));
            }
            SessionEventKindView::SessionOpened { .. }
            | SessionEventKindView::SessionConfigChanged { .. }
            | SessionEventKindView::WorkflowToolsConfigured { .. }
            | SessionEventKindView::SystemWorkflowToolConfigured { .. }
            | SessionEventKindView::WorkflowToolEmitted { .. }
            | SessionEventKindView::WorkflowToolDeliveryFailed { .. }
            | SessionEventKindView::WorkflowToolStartRequested { .. }
            | SessionEventKindView::WorkflowToolStartFailed { .. }
            | SessionEventKindView::SessionClosed
            | SessionEventKindView::ContextEntriesApplied { .. }
            | SessionEventKindView::ContextEntriesRemoved { .. }
            | SessionEventKindView::ContextKeysRemoved { .. }
            | SessionEventKindView::ContextKeyPrefixReplaced { .. }
            | SessionEventKindView::ContextStateReplaced { .. }
            | SessionEventKindView::ContextCompactionRequested { .. }
            | SessionEventKindView::ContextCompactionFinished { .. }
            | SessionEventKindView::SkillCatalogSet { .. }
            | SessionEventKindView::TurnCompleted { .. }
            | SessionEventKindView::TurnCancelled { .. }
            | SessionEventKindView::ToolsReplaced { .. }
            | SessionEventKindView::ToolsPatched { .. }
            | SessionEventKindView::ToolBatchDeferred { .. }
            | SessionEventKindView::ToolBatchResumed { .. }
            | SessionEventKindView::ActiveEnvironmentChanged { .. } => {}
        }
        events
    }

    fn run_view_from_status(
        &mut self,
        run_id: &str,
        status: api::RunStatus,
        observed_at_ms: u64,
    ) -> ChatRunView {
        // Every run lifecycle event routes through here, so this is the one
        // event-tail write into the run-state index behind /steer,
        // /interrupt, and the model lock.
        self.run_states.insert(
            run_seq_from_id(run_id),
            TrackedRun {
                id: run_id.to_string(),
                status,
            },
        );
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
                    direction: Default::default(),
                    before: None,
                    session_id: self.session_id.clone(),
                    after: self.event_cursor,
                    limit: Some(512),
                    wait_ms: None,
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
        self.run_states.clear();
        self.api
            .start_session(SessionStartParams {
                metadata: Default::default(),
                session_id: Some(session_id.clone()),
                display_name: None,
                config: Some(session_start_config(&self.settings)),
                profile: None,
                environment: None,
                delete_after_close_ms: None,
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
        self.run_states.clear();
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
        self.run_states.values().any(|run| {
            matches!(
                run.status,
                api::RunStatus::Queued | api::RunStatus::Running | api::RunStatus::Parked
            )
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

#[cfg(test)]
fn project_tool_chains(run: &api::RunView) -> Vec<ChatToolChainView> {
    let mut chains = run
        .tool_batches
        .iter()
        .map(|batch| project_tool_batch(&run.id, batch))
        .collect::<Vec<_>>();
    chains.extend(project_provider_tool_chains(&run.id, &run.entries));
    chains
}

#[cfg(test)]
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

#[cfg(test)]
fn project_provider_tool_chains(
    run_id: &str,
    entries: &[ContextEntryView],
) -> Vec<ChatToolChainView> {
    entries
        .iter()
        .filter_map(|entry| match (&entry.kind, &entry.display) {
            (ContextEntryKindView::ProviderOpaque, Some(display)) => Some(
                project_provider_tool_chain(run_id, &entry.id, &entry.provider_item_id, display),
            ),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
fn project_provider_tool_chain(
    run_id: &str,
    item_id: &str,
    provider_item_id: &Option<String>,
    display: &api::ProviderContextDisplayView,
) -> ChatToolChainView {
    let status = tool_status(display.status);
    let call_id = provider_item_id.as_deref().unwrap_or(item_id).to_owned();
    ChatToolChainView {
        id: format!("{run_id}:provider:{item_id}"),
        title: "mcp 1 call".to_owned(),
        status,
        reasoning: None,
        summary: Some("mcp".to_owned()),
        calls: vec![ChatToolCallView {
            id: call_id,
            tool_id: None,
            tool_name: display.tool_name.clone(),
            status,
            group_index: Some(1),
            parallel_safe: None,
            resource_key: None,
            arguments_preview: display.arguments.as_ref().map(|value| preview(value)),
            result_preview: display.output.as_ref().map(|value| preview(value)),
            error: display
                .error
                .clone()
                .or_else(|| display.is_error.then(|| display.output.clone()).flatten()),
            display: Some(tool_display_from_api(&display.summary)),
        }],
    }
}

fn event_needs_snapshot(kind: &SessionEventKindView) -> bool {
    matches!(
        kind,
        SessionEventKindView::ContextEntriesApplied { .. }
            | SessionEventKindView::ContextEntriesRemoved { .. }
            | SessionEventKindView::ContextKeysRemoved { .. }
            | SessionEventKindView::ContextKeyPrefixReplaced { .. }
            | SessionEventKindView::ContextStateReplaced { .. }
            | SessionEventKindView::ContextCompactionFinished { .. }
            | SessionEventKindView::RunCompleted { .. }
            | SessionEventKindView::RunFailed { .. }
            | SessionEventKindView::RunCancelled { .. }
            | SessionEventKindView::ApprovalRequested { .. }
            | SessionEventKindView::ApprovalDecided { .. }
            | SessionEventKindView::ApprovalCancelled { .. }
            | SessionEventKindView::ToolBatchCompleted { .. }
    )
}

fn approval_decision_label(decision: api::ApprovalDecisionKind) -> &'static str {
    match decision {
        api::ApprovalDecisionKind::Approve => "approved",
        api::ApprovalDecisionKind::Reject => "rejected",
    }
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

#[cfg(test)]
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
            api::ToolCallDisplayGroup::Mcp => ChatToolDisplayGroup::Mcp,
            api::ToolCallDisplayGroup::Agent => ChatToolDisplayGroup::Agent,
            api::ToolCallDisplayGroup::Bot => ChatToolDisplayGroup::Bot,
            api::ToolCallDisplayGroup::Message => ChatToolDisplayGroup::Message,
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
            ChatToolDisplayGroup::Mcp => "mcp",
            ChatToolDisplayGroup::Agent => "agents",
            ChatToolDisplayGroup::Bot => "bots",
            ChatToolDisplayGroup::Message => "messages",
            ChatToolDisplayGroup::Other => "tools",
        }
        .into(),
    )
}

#[cfg(test)]
fn tool_status(status: ToolItemStatus) -> ChatProgressStatus {
    match status {
        ToolItemStatus::Requested | ToolItemStatus::Running => ChatProgressStatus::Running,
        ToolItemStatus::Succeeded => ChatProgressStatus::Succeeded,
        ToolItemStatus::Cancelled => ChatProgressStatus::Cancelled,
        ToolItemStatus::Failed | ToolItemStatus::Unavailable => ChatProgressStatus::Failed,
    }
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
            .and_then(|config| config.model.as_ref())
            .map(|model| model.provider_id.clone()),
        model: session
            .config
            .as_ref()
            .and_then(|config| config.model.as_ref())
            .map(|model| model.model.clone()),
        active_run: session
            .runs
            .iter()
            .find(|run| matches!(run.status, api::RunStatus::Running | api::RunStatus::Parked))
            .map(|run| run.id.clone()),
    }
}

fn summary_from_mutation(session: &api::SessionMutationView) -> ChatSessionSummary {
    ChatSessionSummary {
        session_id: session.id.clone(),
        status: Some(session.status),
        lifecycle: Some(session_lifecycle(session.status)),
        updated_at_ns: None,
        run_count: 0,
        provider: None,
        model: None,
        active_run: None,
    }
}

fn run_event_from_summary(
    run: &api::RunSummaryView,
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
        started_at_ns: run.started_at_ms.unwrap_or(0).saturating_mul(1_000_000),
        updated_at_ns: run
            .completed_at_ms
            .or(run.started_at_ms)
            .unwrap_or(run.accepted_at_ms)
            .saturating_mul(1_000_000),
    })
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
        web_search: args.no_web_search.then_some(false),
        web_fetch: args.no_web_fetch.then_some(false),
        bare: args.bare,
        filesystem_tools: args
            .filesystem_tools
            .as_deref()
            .map(parse_filesystem_tool_mode)
            .transpose()?,
    })
}

fn parse_filesystem_tool_mode(value: &str) -> Result<crate::chat::protocol::FilesystemToolMode> {
    use crate::chat::protocol::FilesystemToolMode;
    match value {
        "edit" => Ok(FilesystemToolMode::Edit),
        "read-only" | "read_only" | "readonly" => Ok(FilesystemToolMode::ReadOnly),
        "none" | "off" | "disabled" => Ok(FilesystemToolMode::None),
        other => Err(anyhow!(
            "invalid filesystem tool mode '{other}'; expected edit, read-only, or none"
        )),
    }
}

fn model_config(settings: &ChatDraftSettings) -> ModelConfig {
    ModelConfig {
        provider_id: settings.provider.clone(),
        api_kind: settings.api_kind.clone(),
        model: settings.model.clone(),
    }
}

fn session_start_config(settings: &ChatDraftSettings) -> api::SessionConfig {
    api::SessionConfig {
        model: Some(model_config(settings)),
        generation: Some(generation_config(settings)),
        limits: None,
        context: None,
        features: (!settings.bare).then(|| dev_features(settings)),
    }
}

/// The CLI's development defaults: features are secure-by-default on the
/// server (absent = off), so the chat client grants a usable dev surface
/// explicitly — VFS with fs tools and prompt sourcing, web, timers. Skill discovery
/// requires an explicit profile/session configuration.
fn dev_features(settings: &ChatDraftSettings) -> FeaturesConfig {
    let vfs_tools = match settings.filesystem_tools {
        None => Some(VfsToolSurface::Edit),
        Some(crate::chat::protocol::FilesystemToolMode::Edit) => Some(VfsToolSurface::Edit),
        Some(crate::chat::protocol::FilesystemToolMode::ReadOnly) => Some(VfsToolSurface::ReadOnly),
        Some(crate::chat::protocol::FilesystemToolMode::None) => None,
    };
    let web_fetch = settings.web_fetch.unwrap_or(true);
    let web_search = settings.web_search.unwrap_or(true)
        && matches!(
            settings.api_kind.as_str(),
            "openai:responses" | "anthropic:messages"
        );
    FeaturesConfig {
        vfs: Some(VfsFeature {
            working_directory: None,
            version: api::CURRENT_FEATURE_VERSION,
            workspace_links: Vec::new(),
            tools: vfs_tools,
            prompts: Some(VfsPromptsConfig::default()),
            skills: None,
        }),
        web: (web_fetch || web_search).then(|| WebFeature {
            version: api::CURRENT_FEATURE_VERSION,
            fetch: web_fetch.then(WebFetchFeature::default),
            search: web_search.then(WebSearchFeature::default),
        }),
        timers: Some(TimersFeature {
            version: api::CURRENT_FEATURE_VERSION,
        }),
        ..FeaturesConfig::default()
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
        tool_choice: None,
        parallel_tool_use: None,
        processing_tier: None,
    }
}

fn api_reasoning_effort(settings: &ChatDraftSettings) -> Option<String> {
    if !matches!(
        settings.api_kind.as_str(),
        "openai:responses" | "openai:completions"
    ) {
        return None;
    }
    Some(
        match settings.reasoning_effort {
            None => "none",
            Some(crate::chat::protocol::ReasoningEffort::Low) => "low",
            Some(crate::chat::protocol::ReasoningEffort::Medium) => "medium",
            Some(crate::chat::protocol::ReasoningEffort::High) => "high",
        }
        .to_owned(),
    )
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
        ChatEvent::ApprovalsPending {
            run_id, approvals, ..
        } => {
            for approval in approvals {
                let api::ApprovalSubjectView::McpToolCall {
                    server_label,
                    tool_name,
                    arguments_preview,
                    ..
                } = &approval.subject;
                println!(
                    "approval {} pending for run {}: {} on {}\n{}\n  /approve {}\n  /reject {} [note]",
                    approval.approval_id,
                    run_id,
                    tool_name,
                    server_label,
                    arguments_preview,
                    approval.approval_id,
                    approval.approval_id
                );
            }
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
    crate::skills_cli::format_skill_catalogs(response)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_tool_status_is_rendered_neutrally() {
        assert_eq!(
            tool_status(ToolItemStatus::Cancelled),
            ChatProgressStatus::Cancelled
        );
    }

    #[test]
    fn project_tool_chains_preserves_lightspeed_tool_call_details() {
        let run = api::RunView {
            output: None,
            output_text: None,
            id: "run_7".into(),
            status: api::RunStatus::Running,
            started_at_ms: None,
            completed_at_ms: None,
            source: api::RunViewSource::Input { items: Vec::new() },
            entries: Vec::new(),
            tool_batches: vec![ToolBatchView {
                id: "tool_batch_1".into(),
                turn_id: "turn_1".into(),
                status: ToolItemStatus::Succeeded,
                calls: vec![ToolCallView {
                    tool_id: Some("env.read_file".into()),
                    started_at_ms: None,
                    completed_at_ms: None,
                    duration_ms: None,
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
            usage: None,
            pending_approvals: Vec::new(),
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
    fn project_tool_chains_renders_projected_mcp_calls() {
        let run = api::RunView {
            output: None,
            output_text: None,
            id: "run_7".into(),
            status: api::RunStatus::Completed,
            started_at_ms: None,
            completed_at_ms: None,
            source: api::RunViewSource::Input { items: Vec::new() },
            entries: vec![ContextEntryView {
                id: "item_43".into(),
                key: None,
                kind: ContextEntryKindView::ProviderOpaque,
                content: api::ContentRefView {
                    content_ref: "sha256:mcp".into(),
                    media_type: Some("application/json".into()),
                    provider_kind: Some("openai.responses.mcp_call".into()),
                },
                origin: None,
                provenance_ref: None,
                preview: Some("OpenAI Responses MCP tool call: echo.echo".into()),
                provider_item_id: Some("mcp_1".into()),
                token_estimate: None,
                text: None,
                text_truncated: false,
                display: Some(api::ProviderContextDisplayView {
                    summary: api::ToolCallDisplayView {
                        group: api::ToolCallDisplayGroup::Other,
                        verb: "MCP".into(),
                        target: Some("echo.echo".into()),
                        detail: None,
                    },
                    tool_name: "echo.echo".into(),
                    status: ToolItemStatus::Succeeded,
                    is_error: false,
                    arguments: Some(r#"{"data":"simba"}"#.into()),
                    output: Some("Echoing your input: simba".into()),
                    error: None,
                }),
                citations: Vec::new(),
                source: None,
                supersedes: None,
                superseded_by: None,
            }],
            tool_batches: Vec::new(),
            usage: None,
            pending_approvals: Vec::new(),
        };

        let chains = project_tool_chains(&run);

        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].id, "run_7:provider:item_43");
        assert_eq!(chains[0].title, "mcp 1 call");
        assert_eq!(chains[0].summary.as_deref(), Some("mcp"));
        assert_eq!(chains[0].status, ChatProgressStatus::Succeeded);
        assert_eq!(chains[0].calls[0].id, "mcp_1");
        assert_eq!(chains[0].calls[0].tool_name, "echo.echo");
        assert_eq!(
            chains[0].calls[0].arguments_preview.as_deref(),
            Some(r#"{"data":"simba"}"#)
        );
        assert_eq!(
            chains[0].calls[0].result_preview.as_deref(),
            Some("Echoing your input: simba")
        );
        assert_eq!(
            chains[0].calls[0]
                .display
                .as_ref()
                .map(|display| (display.verb.as_str(), display.target.as_deref())),
            Some(("MCP", Some("echo.echo")))
        );
    }

    #[test]
    fn formats_skill_list_for_transcript_notice() {
        let response = api::SkillListResponse {
            catalogs: vec![api::SkillCatalogView {
                source: api::SkillCatalogSource::Vfs,
                availability: api::SkillCatalogAvailability::Available,
                warnings: vec![],
                catalog_ref: Some("sha256:catalog".into()),
                skills: vec![api::SkillListItem {
                    skill_id: "lightspeed:review".into(),
                    name: "Review".into(),
                    description: "Review repository changes.".into(),
                    short_description: Some("review diffs".into()),
                    enabled: true,
                    location: api::SkillLocationView {
                        skill_dir_path: "/skills/review".into(),
                        skill_doc_path: "/skills/review/SKILL.md".into(),
                    },
                }],
            }],
        };

        let rendered = format_skill_list(&response);

        assert!(rendered.contains("catalogRef sha256:catalog"));
        assert!(rendered.contains("- lightspeed:review [enabled] Review"));
        assert!(rendered.contains("Review repository changes."));
        assert!(rendered.contains("short review diffs"));
    }

    #[test]
    fn run_seq_from_id_reads_lightspeed_api_run_ids() {
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
        assert_eq!(generation.reasoning_effort, Some("none".to_owned()));
    }

    #[test]
    fn session_start_config_grants_dev_features_by_default() {
        let settings = draft_settings(&chat_args_with_effort(None)).expect("draft settings");

        let config = session_start_config(&settings);

        let features = config.features.expect("features");
        let vfs = features.vfs.expect("vfs");
        assert_eq!(vfs.tools, Some(VfsToolSurface::Edit));
        assert!(vfs.prompts.is_some());
        assert!(vfs.skills.is_none());
        let web = features.web.expect("web");
        assert!(web.fetch.is_some());
        assert!(web.search.is_some());
        assert!(features.timers.is_some());
    }

    #[test]
    fn session_start_config_can_disable_web_search() {
        let mut args = chat_args_with_effort(None);
        args.no_web_search = true;
        let settings = draft_settings(&args).expect("draft settings");

        let config = session_start_config(&settings);

        let web = config.features.expect("features").web.expect("web");
        assert!(web.search.is_none());
        assert!(web.fetch.is_some());
    }

    #[test]
    fn session_start_config_can_disable_web_fetch() {
        let mut args = chat_args_with_effort(None);
        args.no_web_fetch = true;
        let settings = draft_settings(&args).expect("draft settings");

        let config = session_start_config(&settings);

        let web = config.features.expect("features").web.expect("web");
        assert!(web.fetch.is_none());
        assert!(web.search.is_some());
    }

    #[test]
    fn session_start_config_can_select_read_only_filesystem_tools() {
        let mut args = chat_args_with_effort(None);
        args.filesystem_tools = Some("read-only".to_owned());
        let settings = draft_settings(&args).expect("draft settings");

        let config = session_start_config(&settings);

        let vfs = config.features.expect("features").vfs.expect("vfs");
        assert_eq!(vfs.tools, Some(VfsToolSurface::ReadOnly));
    }

    #[test]
    fn session_start_config_bare_sends_no_feature_grants() {
        let mut args = chat_args_with_effort(None);
        args.bare = true;
        let settings = draft_settings(&args).expect("draft settings");

        let config = session_start_config(&settings);

        assert!(config.features.is_none());
    }

    #[test]
    fn run_start_config_omits_reasoning_for_anthropic() {
        let mut settings =
            draft_settings(&chat_args_with_effort(Some("high"))).expect("draft settings");
        settings.api_kind = "anthropic:messages".to_owned();

        let config = run_start_config(&settings);

        assert_eq!(
            config.generation.expect("generation").reasoning_effort,
            None
        );
    }

    #[test]
    fn completions_draft_round_trips_model_reasoning_and_compatible_features() {
        let mut settings =
            draft_settings(&chat_args_with_effort(Some("high"))).expect("draft settings");
        settings.api_kind = "openai:completions".to_owned();

        let session = session_start_config(&settings);
        let model = session.model.expect("model");
        let generation = session.generation.expect("generation");
        let features = session.features.expect("features");

        assert_eq!(model.api_kind, "openai:completions");
        assert_eq!(model.provider_id, "openai");
        assert_eq!(generation.reasoning_effort.as_deref(), Some("high"));
        assert!(features.vfs.is_some());
        assert!(features.web.as_ref().is_some_and(|web| web.fetch.is_some()));
        assert!(features.web.as_ref().is_none_or(|web| web.search.is_none()));
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
            no_web_search: false,
            no_web_fetch: false,
            filesystem_tools: None,
            bare: false,
            profile: None,
            profile_json: None,
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
                tool_id: None,
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
            output: None,
        }));
        assert!(event_needs_snapshot(
            &SessionEventKindView::ContextEntriesApplied {
                base_revision: 0,
                revision: 1,
                entries: Vec::new(),
            }
        ));
        assert!(event_needs_snapshot(
            &SessionEventKindView::ContextStateReplaced {
                base_revision: 1,
                revision: 2,
                entries: Vec::new(),
                reason: "pruned".into(),
            }
        ));
        assert!(event_needs_snapshot(
            &SessionEventKindView::ContextEntriesRemoved {
                base_revision: 2,
                revision: 3,
                entry_ids: Vec::new(),
                reason: "pruned".into(),
            }
        ));
        assert!(event_needs_snapshot(
            &SessionEventKindView::ContextKeysRemoved {
                base_revision: 3,
                revision: 4,
                keys: Vec::new(),
            }
        ));
        assert!(event_needs_snapshot(
            &SessionEventKindView::ToolBatchCompleted {
                run_id: "run_1".into(),
                turn_id: "turn_1".into(),
                batch_id: "batch_1".into(),
            }
        ));
        assert!(!event_needs_snapshot(&SessionEventKindView::RunAccepted {
            run_id: "run_1".into(),
            submission_id: Some("submit_1".into()),
            source: api::RunAcceptedSourceView::Input {
                entries: Vec::new(),
            },
        }));
        assert!(!event_needs_snapshot(&SessionEventKindView::RunStarted {
            run_id: "run_1".into(),
        }));
    }
}

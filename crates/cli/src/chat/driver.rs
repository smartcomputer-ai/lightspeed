use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use api::{
    AgentApiOutcome, ContextEntryKindView, ContextEntryView, ContextMessageRoleView, EventCursor,
    FeaturesConfig, GenerationConfig, InlineAgentProfile, InputItem, ModelConfig, ProfileId,
    ProfileSource, RunReadParams, RunStartConfig, RunStartParams, RunStartResponse, RunStartSource,
    SessionEventKindView, SessionEventView, SessionEventsReadParams, SessionReadParams,
    SessionStartParams, SessionView, TimersFeature, ToolBatchView, ToolCallEventView, ToolCallView,
    ToolItemStatus, VfsFeature, VfsPromptsConfig, WebFeature, WebFetchFeature, WebSearchFeature,
    WorkspaceAccess,
};
use clap::Args;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::api_client::{HttpAgentApi, api_error};
use crate::chat::preview::compact_preview;
use crate::chat::protocol::{
    ChatCommand, ChatConnectionInfo, ChatDelta, ChatDraftSettings, ChatErrorView, ChatEvent,
    ChatMessageView, ChatProgressStatus, ChatRunStats, ChatRunView, ChatSessionSummary,
    ChatSettingsView, ChatStatus, ChatToolCallDisplayView, ChatToolCallView, ChatToolChainView,
    ChatToolDisplayGroup, ChatTurn, DEFAULT_CHAT_REASONING_EFFORT, GATEWAY_WORLD_ID,
    ModelPickerPurpose, run_stats_summary, run_status, session_lifecycle,
};
use crate::chat::session::{new_session_id, new_submission_id, validate_session_id};

#[derive(Args, Debug, Clone)]
pub(crate) struct ChatArgs {
    /// Session ID to open or create through the configured Lightspeed API.
    #[arg(short = 's', long, conflicts_with_all = ["new", "resume", "list"])]
    session: Option<String>,
    /// List the 10 most recently updated unmanaged root sessions, then exit.
    #[arg(long, conflicts_with_all = ["new", "resume", "message", "workspace_source", "workspace_path", "workspace_access", "profile", "profile_json", "provider", "api_kind", "model", "no_web_search", "no_web_fetch", "bare", "show_tool_details", "show_stats"])]
    list: bool,
    /// Continue the most recently updated unmanaged root session that is not closed.
    #[arg(long, visible_alias = "continue", conflicts_with_all = ["new", "profile", "profile_json", "bare", "no_web_search", "no_web_fetch"])]
    resume: bool,
    /// Start with a fresh session ID.
    #[arg(long)]
    new: bool,
    /// Provider ID for the model adapter. With --api-kind and --model;
    /// omit all three for the deployment default.
    #[arg(long, requires_all = ["api_kind", "model"])]
    provider: Option<String>,
    /// Provider API kind.
    #[arg(long = "api-kind", requires_all = ["provider", "model"])]
    api_kind: Option<String>,
    /// Model name.
    #[arg(long, requires_all = ["provider", "api_kind"])]
    model: Option<String>,
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
    /// Upload a local directory snapshot into a new workspace; no live sync or local writeback.
    #[arg(long, group = "workspace_source")]
    upload: Option<PathBuf>,
    /// Attach an existing runtime workspace to this session.
    #[arg(long, group = "workspace_source")]
    workspace: Option<String>,
    /// Path of the workspace inside the session.
    #[arg(long, default_value = "/workspace", requires = "workspace_source")]
    workspace_path: String,
    /// Access granted to the attached workspace.
    #[arg(
        long,
        value_enum,
        default_value = "edit",
        requires = "workspace_source"
    )]
    workspace_access: crate::vfs_cli::WorkspaceAccessArg,
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Show tool call arguments and results in the TUI.
    #[arg(long)]
    show_tool_details: bool,
    /// Show run statistics: timing, token usage and context details (toggle with /stats).
    #[arg(long)]
    show_stats: bool,
    /// Emit the response as JSON.
    #[arg(long)]
    json: bool,
    /// Submit one message and exit. If omitted, starts the interactive TUI.
    message: Vec<String>,
}

pub(crate) async fn handle(args: ChatArgs) -> Result<()> {
    if args.list {
        return super::recent::list(&HttpAgentApi::new(args.api_url), args.json).await;
    }
    let draft = draft_settings(&args)?;
    let profile = profile_source_from_args(args.profile.as_deref(), args.profile_json.as_deref())?;
    let session_id = if args.resume {
        super::recent::resume_id(&HttpAgentApi::new(args.api_url.clone())).await?
    } else if args.new {
        new_session_id()
    } else if let Some(session_id) = args.session.as_ref() {
        validate_session_id(session_id)?
    } else {
        new_session_id()
    };

    let message = (!args.message.is_empty()).then(|| args.message.join(" "));
    let options = ChatSessionDriverOptions {
        session_id,
        draft_settings: draft,
        api_url: args.api_url,
        profile,
    };
    let (mut driver, mut initial_events) = if args.resume {
        ChatSessionDriver::open_with_mode(options, true).await?
    } else {
        ChatSessionDriver::open(options).await?
    };
    if let Some(directory) = args.upload {
        initial_events.extend(
            driver
                .upload_directory(directory, args.workspace_path, args.workspace_access.into())
                .await?,
        );
    } else if let Some(workspace) = args.workspace {
        initial_events.extend(
            driver
                .attach_workspace(workspace, args.workspace_path, args.workspace_access.into())
                .await?,
        );
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
        driver.ensure_transcript_loaded()?;
        println!("{}", serde_json::to_string_pretty(driver.turns())?);
        return Ok(());
    }

    if let Some(message) = message {
        for event in &initial_events {
            print_event(event, args.show_stats)?;
        }
        for event in driver
            .handle_command(ChatCommand::SubmitUserMessage { text: message })
            .await?
        {
            print_event(&event, args.show_stats)?;
        }
        let mut follow_events = Vec::new();
        driver
            .follow_until_quiescent(Duration::from_secs(300), |event| {
                follow_events.push(event);
            })
            .await?;
        for event in &follow_events {
            print_event(event, args.show_stats)?;
        }
        driver.ensure_transcript_loaded()?;
        return Ok(());
    }

    crate::chat::tui::run_shell(
        driver,
        initial_events,
        args.show_tool_details,
        args.show_stats,
    )
    .await
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
    observed_tool_chains: BTreeMap<String, Vec<ChatToolChainView>>,
    /// Run lifecycle facts keyed by run sequence, fed by the event tail and
    /// reconciled against `session/read`; `/steer`, `/interrupt`, and the
    /// model lock derive the active run from this, not the transcript.
    run_states: BTreeMap<u64, TrackedRun>,
    /// Projected turns of terminal runs, keyed by run id. A terminal run
    /// never changes, so each is read through `session/runs/read` once.
    finished_turns: BTreeMap<String, ChatTurn>,
    /// Stored model route; provider identity and API kind are immutable.
    session_model: Option<ModelConfig>,
    transcript_errors: BTreeMap<String, String>,
    /// Model calls and last prompt size per run id, from observed
    /// `turnGenerationCompleted` events; the run views do not carry them.
    generation_stats: BTreeMap<String, GenerationStats>,
    pending_run: Option<PendingRunHandle>,
    notice_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrackedRun {
    id: String,
    status: api::RunStatus,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GenerationStats {
    calls: u32,
    last_input_tokens: Option<u32>,
}

pub(crate) struct FollowProgress {
    pub quiescent: bool,
    pub activity: bool,
}

type PendingRunHandle =
    JoinHandle<std::result::Result<AgentApiOutcome<RunStartResponse>, api::AgentApiError>>;

type ChatAgentApi = Arc<HttpAgentApi>;

impl ChatSessionDriver {
    pub(crate) async fn open(options: ChatSessionDriverOptions) -> Result<(Self, Vec<ChatEvent>)> {
        Self::open_with_mode(options, false).await
    }

    async fn open_with_mode(
        options: ChatSessionDriverOptions,
        resume_only: bool,
    ) -> Result<(Self, Vec<ChatEvent>)> {
        let session_id = validate_session_id(&options.session_id)?;
        let api = build_chat_api(&options).await?;
        let summary = if resume_only {
            // Resume must never recreate a session deleted after listing it.
            let session = api
                .read_session(SessionReadParams {
                    session_id: session_id.clone(),
                    run_limit: Some(1),
                })
                .await
                .map_err(api_error)?
                .result
                .session;
            if session.status == api::SessionStatus::Closed {
                anyhow::bail!(
                    "session {session_id} closed before it could be resumed; use `chat --list` to choose another"
                );
            }
            summary_from_session(&session)
        } else {
            let started = api
                .open_or_start_session(SessionStartParams {
                    metadata: Default::default(),
                    session_id: Some(session_id.clone()),
                    display_name: None,
                    config: Some(session_start_config(&options.draft_settings)),
                    profile: options.profile.clone(),
                    delete_after_close_ms: None,
                    access: None,
                })
                .await
                .map_err(api_error)?;
            summary_from_mutation(&started.result.session)
        };

        let mut driver = Self {
            api,
            session_id: session_id.clone(),
            settings: options.draft_settings,
            event_cursor: None,
            turns: Vec::new(),
            active_tool_chains: Vec::new(),
            observed_tool_chains: BTreeMap::new(),
            run_states: BTreeMap::new(),
            finished_turns: BTreeMap::new(),
            session_model: None,
            transcript_errors: BTreeMap::new(),
            generation_stats: BTreeMap::new(),
            pending_run: None,
            notice_seq: 0,
        };
        let mut events = vec![ChatEvent::Connected(ChatConnectionInfo {
            world_id: GATEWAY_WORLD_ID.into(),
            session_id,
            journal_next_from: None,
            settings: driver.settings_view(),
        })];
        events.push(ChatEvent::SessionSelected(summary));
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

    async fn upload_directory(
        &mut self,
        directory: PathBuf,
        workspace_path: String,
        access: WorkspaceAccess,
    ) -> Result<Vec<ChatEvent>> {
        if !self.is_quiescent() {
            return Err(anyhow!(
                "cannot upload and attach a workspace while a run is active"
            ));
        }
        let summary = crate::vfs_transfer::upload_snapshot_directory(
            self.api.as_ref(),
            directory,
            crate::vfs_transfer::SnapshotUploadOptions::default(),
        )
        .await
        .context("failed to upload local directory snapshot")?;
        let workspace =
            crate::vfs_cli::create_workspace_from_snapshot(self.api.as_ref(), summary.snapshot_ref)
                .await
                .context("failed to create workspace from uploaded snapshot")?;
        self.attach_workspace(workspace.workspace_id, workspace_path, access)
            .await
    }

    async fn attach_workspace(
        &mut self,
        workspace_id: String,
        workspace_path: String,
        access: WorkspaceAccess,
    ) -> Result<Vec<ChatEvent>> {
        if !self.is_quiescent() {
            return Err(anyhow!("cannot attach a workspace while a run is active"));
        }
        crate::vfs_cli::attach_workspace(
            self.api.as_ref(),
            self.session_id.clone(),
            workspace_path,
            workspace_id,
            access,
        )
        .await
        .context("failed to attach chat workspace")?;
        self.refresh().await
    }

    pub(crate) async fn handle_command(&mut self, command: ChatCommand) -> Result<Vec<ChatEvent>> {
        match command {
            ChatCommand::SubmitUserMessage { text } => self.submit_user_message(text).await,
            ChatCommand::SetDraftProvider { provider } => self.set_provider(provider).await,
            ChatCommand::SetDraftModel { model } => self.set_model(model).await,
            ChatCommand::SetDraftRoute {
                provider,
                api_kind,
                model,
            } => self.set_route(provider, api_kind, model).await,
            ChatCommand::ListModels { purpose } => self.list_models(purpose).await,
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
            let progress = self.follow_once(wait_ms, &mut emit).await?;
            // First flush backlog immediately, then long-poll for new events.
            wait_ms = Some(FOLLOW_EVENT_WAIT_MS);
            if progress.activity {
                inactivity_deadline.record_activity(Instant::now());
            }
            if progress.quiescent {
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
            if progress.activity {
                tokio::task::yield_now().await;
            }
            // No client-side sleep: the next drain's long-poll parks
            // server-side until events arrive or the wait elapses.
        }
    }

    /// A bounded follow step lets the TUI handle commands between event reads.
    pub(crate) async fn follow_once(
        &mut self,
        wait_ms: Option<u64>,
        emit: &mut impl FnMut(ChatEvent),
    ) -> Result<FollowProgress> {
        let mut activity = self.stream_event_log(wait_ms, emit).await?;
        activity |= self.collect_finished_run_into(emit).await?;
        if self.is_quiescent() {
            activity |= self.stream_event_log(None, emit).await?;
            for event in self.refresh_snapshot().await? {
                emit(event);
            }
            if self.is_quiescent() {
                emit(ChatEvent::ToolChainsChanged {
                    session_id: self.session_id.clone(),
                    chains: Vec::new(),
                });
            }
        }
        Ok(FollowProgress {
            quiescent: self.is_quiescent(),
            activity,
        })
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
        let mut events = Vec::new();
        self.collect_finished_run_into(&mut |event| events.push(event))
            .await?;
        Ok(events)
    }

    async fn collect_finished_run_into(
        &mut self,
        emit: &mut impl FnMut(ChatEvent),
    ) -> Result<bool> {
        let Some(handle) = self.pending_run.as_ref() else {
            return Ok(false);
        };
        if !handle.is_finished() {
            return Ok(false);
        }
        let Some(handle) = self.pending_run.take() else {
            return Ok(false);
        };
        match handle.await {
            Ok(Ok(_outcome)) => {
                self.stream_event_log(None, emit).await?;
                for event in self.refresh_snapshot().await? {
                    emit(event);
                }
            }
            Ok(Err(error)) => emit(ChatEvent::Error(ChatErrorView {
                message: error.to_string(),
                action: None,
            })),
            Err(error) => emit(ChatEvent::Error(ChatErrorView {
                message: format!("run task failed: {error}"),
                action: None,
            })),
        }
        Ok(true)
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
        self.sync_session_model(&session)?;
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
        let (turns, transcript_events) = self.project_turns(&session).await;
        self.turns = turns;
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
        // Emit errors after replacing the transcript so the TUI retains them.
        events.extend(transcript_events);
        Ok(events)
    }

    /// Transcript turns for the session's runs. `session/read` carries only
    /// run summaries, so a terminal run is read once through
    /// `session/runs/read` for its reply and tool chains, then cached; a run
    /// still in flight shows its input until it finishes. A failed read
    /// (e.g. a run over the server's detail ceiling) falls back to the
    /// summary and is retried on the next refresh.
    async fn project_turns(&mut self, session: &SessionView) -> (Vec<ChatTurn>, Vec<ChatEvent>) {
        let mut events = Vec::new();
        self.transcript_errors
            .retain(|id, _| session.runs.iter().any(|run| &run.id == id));
        let active = session
            .active_run
            .as_ref()
            .filter(|active| !session.runs.iter().any(|run| run.id == active.id));
        let mut turns = Vec::with_capacity(session.runs.len() + 1);
        for summary in session.runs.iter().chain(active) {
            if let Some(turn) = self.finished_turns.get(&summary.id) {
                turns.push(turn.clone());
                continue;
            }
            let mut turn = turn_from_summary(summary, &self.settings);
            turn.tool_chains = self
                .observed_tool_chains
                .get(&summary.id)
                .cloned()
                .unwrap_or_default();
            if let (Some(run), Some(generations)) =
                (turn.run.as_mut(), self.generation_stats.get(&summary.id))
            {
                run.stats.model_calls = Some(generations.calls);
                run.stats.context_tokens = generations.last_input_tokens;
            }
            if run_is_terminal(summary.status) {
                match self
                    .api
                    .read_run(RunReadParams {
                        session_id: self.session_id.clone(),
                        run_id: summary.id.clone(),
                    })
                    .await
                {
                    Ok(read) => {
                        apply_run_detail(&mut turn, &read.result.run);
                        self.finished_turns.insert(summary.id.clone(), turn.clone());
                        self.observed_tool_chains.remove(&summary.id);
                        self.transcript_errors.remove(&summary.id);
                    }
                    Err(error) => {
                        let message = format!(
                            "could not load transcript for {}: {}",
                            summary.id,
                            api_error(error)
                        );
                        if self.transcript_errors.get(&summary.id) != Some(&message) {
                            events.push(ChatEvent::Error(ChatErrorView {
                                message: message.clone(),
                                action: Some("use /refresh to retry; oversized runs can be read through session/events/read".into()),
                            }));
                        }
                        self.transcript_errors.insert(summary.id.clone(), message);
                    }
                }
            }
            turns.push(turn);
        }
        // Summaries arrive newest first; the transcript reads oldest first.
        turns.sort_by_key(|turn| run_seq_from_id(&turn.turn_id));
        (turns, events)
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
        self.stream_event_log(wait_first_ms, &mut |event| events.push(event))
            .await?;
        Ok(events)
    }

    async fn stream_event_log(
        &mut self,
        wait_first_ms: Option<u64>,
        emit: &mut impl FnMut(ChatEvent),
    ) -> Result<bool> {
        let mut saw_activity = false;
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
                emit(ChatEvent::GapObserved {
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
                saw_activity = true;
            }

            for event in &page.result.events {
                needs_snapshot |= event_needs_snapshot(&event.kind);
                saw_activity = true;
                for update in self.chat_events_from_session_event(event) {
                    emit(update);
                }
            }

            self.event_cursor = page.result.next_cursor.or(page.result.head_cursor);
            if page.result.complete {
                break;
            }
        }

        if needs_snapshot {
            for update in self.refresh_snapshot().await? {
                emit(update);
            }
        }
        Ok(saw_activity)
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
            SessionEventKindView::RunCancelled { run_id, .. } => {
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
            SessionEventKindView::TurnGenerationCompleted { run_id, usage, .. } => {
                let stats = self.generation_stats.entry(run_id.clone()).or_default();
                stats.calls = stats.calls.saturating_add(1);
                if let Some(input) = usage.as_ref().and_then(|usage| usage.input_tokens) {
                    stats.last_input_tokens = Some(input);
                }
            }
            SessionEventKindView::ToolBatchStarted {
                run_id,
                batch_id,
                calls,
                ..
            } => {
                let chain = self.tool_chain_from_started_event(run_id, batch_id, calls);
                let observed = self.observed_tool_chains.entry(run_id.clone()).or_default();
                observed.retain(|previous| previous.id != chain.id);
                observed.push(chain.clone());
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
            SessionEventKindView::ToolCallStarted {
                run_id,
                batch_id,
                call_id,
                ..
            } => {
                events.extend(self.update_live_tool(
                    run_id,
                    batch_id,
                    call_id,
                    ChatProgressStatus::Running,
                ));
                events.push(self.status_event("running tools"));
            }
            SessionEventKindView::ToolCallCompleted {
                run_id,
                batch_id,
                call_id,
                status,
                ..
            } => {
                events.extend(self.update_live_tool(
                    run_id,
                    batch_id,
                    call_id,
                    tool_status(*status),
                ));
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

    fn update_live_tool(
        &mut self,
        run_id: &str,
        batch_id: &str,
        call_id: &str,
        status: ChatProgressStatus,
    ) -> Vec<ChatEvent> {
        let Some(chain) = self
            .observed_tool_chains
            .get_mut(run_id)
            .and_then(|chains| {
                chains
                    .iter_mut()
                    .find(|chain| chain.id == format!("{run_id}:{batch_id}"))
            })
        else {
            return Vec::new();
        };
        let Some(call) = chain.calls.iter_mut().find(|call| call.id == call_id) else {
            return Vec::new();
        };
        call.status = status;
        chain.status = if chain.calls.iter().any(|call| {
            matches!(
                call.status,
                ChatProgressStatus::Queued
                    | ChatProgressStatus::Running
                    | ChatProgressStatus::Waiting
            )
        }) {
            ChatProgressStatus::Running
        } else if chain
            .calls
            .iter()
            .any(|call| call.status == ChatProgressStatus::Failed)
        {
            ChatProgressStatus::Failed
        } else if chain
            .calls
            .iter()
            .any(|call| call.status == ChatProgressStatus::Cancelled)
        {
            ChatProgressStatus::Cancelled
        } else {
            ChatProgressStatus::Succeeded
        };
        self.active_tool_chains = vec![chain.clone()];
        vec![ChatEvent::ToolChainsChanged {
            session_id: self.session_id.clone(),
            chains: self.active_tool_chains.clone(),
        }]
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
            stats: Default::default(),
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
        self.session_id = session_id.clone();
        self.event_cursor = None;
        self.turns.clear();
        self.finished_turns.clear();
        self.session_model = None;
        self.transcript_errors.clear();
        self.generation_stats.clear();
        self.active_tool_chains.clear();
        self.observed_tool_chains.clear();
        self.run_states.clear();
        self.api
            .start_session(SessionStartParams {
                metadata: Default::default(),
                session_id: Some(session_id.clone()),
                display_name: None,
                config: Some(session_start_config(&self.settings)),
                profile: None,
                delete_after_close_ms: None,
                access: None,
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
        // `/sessions` lists every session the gateway holds, so any of them
        // may be opened; confirm it exists before dropping the current one.
        let read = match self
            .api
            .read_session(SessionReadParams {
                session_id: session_id.clone(),
                run_limit: Some(1),
            })
            .await
        {
            Ok(read) => read,
            Err(error) => {
                return Ok(vec![ChatEvent::Error(ChatErrorView {
                    message: format!("cannot open session {session_id}: {}", api_error(error)),
                    action: Some("pick a session from /sessions or use /new".into()),
                })]);
            }
        };
        self.settings.route_requested = false;
        self.session_id = session_id.clone();
        self.event_cursor = None;
        self.turns.clear();
        self.finished_turns.clear();
        self.sync_session_model(&read.result.session)?;
        self.transcript_errors.clear();
        self.generation_stats.clear();
        self.active_tool_chains.clear();
        self.observed_tool_chains.clear();
        self.run_states.clear();
        let mut events = vec![ChatEvent::HistoryReset { session_id }];
        events.extend(self.refresh().await?);
        Ok(events)
    }

    async fn set_provider(&mut self, provider: String) -> Result<Vec<ChatEvent>> {
        self.set_route(
            provider,
            self.settings.api_kind.clone(),
            self.settings.model.clone(),
        )
        .await
    }

    async fn set_model(&mut self, model: String) -> Result<Vec<ChatEvent>> {
        self.set_route(
            self.settings.provider.clone(),
            self.settings.api_kind.clone(),
            model,
        )
        .await
    }

    async fn set_route(
        &mut self,
        provider: String,
        api_kind: String,
        model: String,
    ) -> Result<Vec<ChatEvent>> {
        if self.model_locked() {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: "model switching is not supported while a run is active".into(),
                action: Some("wait for the current run to finish first".into()),
            })]);
        }
        if let Err(error) = self.validate_route(&provider, &api_kind) {
            return Ok(vec![ChatEvent::Error(ChatErrorView {
                message: error.to_string(),
                action: Some("start another chat with --provider, --api-kind and --model for a different route".into()),
            })]);
        }
        self.settings.provider = provider;
        self.settings.api_kind = api_kind;
        self.settings.model = model;
        self.settings.route_requested = true;
        Ok(vec![self.setting_status("model updated")])
    }

    /// Discovery failure reports an error and opens no picker.
    async fn list_models(&mut self, purpose: ModelPickerPurpose) -> Result<Vec<ChatEvent>> {
        match self
            .api
            .list_models(api::ModelListParams {
                selectable_only: true,
            })
            .await
        {
            Ok(outcome) => {
                let mut events = Vec::new();
                let failed = outcome
                    .result
                    .providers
                    .iter()
                    .filter_map(|provider| {
                        let error = provider.error.as_ref()?;
                        Some(format!("{}: {error}", provider.provider_id))
                    })
                    .collect::<Vec<_>>();
                if !failed.is_empty() {
                    events.push(self.notice_event(
                        "models",
                        format!("model discovery incomplete\n{}", failed.join("\n")),
                    ));
                }
                events.push(ChatEvent::ModelsListed {
                    purpose,
                    models: outcome.result.models,
                    providers: outcome.result.providers,
                });
                Ok(events)
            }
            Err(error) => Ok(vec![ChatEvent::Error(ChatErrorView {
                message: format!("model discovery failed: {}", api_error(error)),
                action: Some("set a model directly with /model <name>".into()),
            })]),
        }
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

    fn validate_route(&self, provider: &str, api_kind: &str) -> Result<()> {
        let pinned = self
            .session_model
            .as_ref()
            .context("session model has not been loaded")?;
        if provider != pinned.provider_id || api_kind != pinned.api_kind {
            return Err(anyhow!(
                "session provider and API kind are fixed to {} / {}; requested {} / {}",
                pinned.provider_id,
                pinned.api_kind,
                provider,
                api_kind
            ));
        }
        Ok(())
    }

    fn sync_session_model(&mut self, session: &SessionView) -> Result<()> {
        self.session_model = session
            .config
            .as_ref()
            .and_then(|config| config.model.clone());
        if self.settings.route_requested {
            self.validate_route(&self.settings.provider, &self.settings.api_kind)?;
        } else if let Some(model) = &self.session_model {
            self.settings.provider = model.provider_id.clone();
            self.settings.api_kind = model.api_kind.clone();
            self.settings.model = model.model.clone();
        }
        Ok(())
    }

    fn ensure_transcript_loaded(&self) -> Result<()> {
        if self.transcript_errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "incomplete chat transcript: {}",
                self.transcript_errors
                    .values()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            ))
        }
    }

    fn model_locked(&self) -> bool {
        !self.is_quiescent()
    }

    fn run_active(&self) -> bool {
        self.run_states.values().any(|run| {
            matches!(
                run.status,
                api::RunStatus::Queued | api::RunStatus::Running | api::RunStatus::Parked
            )
        })
    }

    pub(crate) fn is_quiescent(&self) -> bool {
        self.pending_run.is_none() && !self.run_active()
    }

    pub(crate) fn pending_run_in_flight(&self) -> bool {
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
            session_api_kind: self
                .session_model
                .as_ref()
                .map(|model| model.api_kind.clone()),
            session_provider: self
                .session_model
                .as_ref()
                .map(|model| model.provider_id.clone().into_boxed_str()),
            reasoning_effort: self.settings.reasoning_effort,
            max_tokens: self.settings.max_tokens,
            provider_editable: false,
            model_editable,
            effort_editable: run_editable,
            max_tokens_editable: run_editable,
        }
    }
}

async fn build_chat_api(options: &ChatSessionDriverOptions) -> Result<ChatAgentApi> {
    Ok(Arc::new(HttpAgentApi::new(options.api_url.clone())))
}

fn run_is_terminal(status: api::RunStatus) -> bool {
    matches!(
        status,
        api::RunStatus::Completed | api::RunStatus::Failed | api::RunStatus::Cancelled
    )
}

fn turn_from_summary(summary: &api::RunSummaryView, settings: &ChatDraftSettings) -> ChatTurn {
    let api::RunSummarySourceView::Input { preview, .. } = &summary.source;
    let run = match run_event_from_summary(summary, settings, run_seq_from_id(&summary.id)) {
        ChatEvent::RunChanged(run) => Some(run),
        _ => None,
    };
    ChatTurn {
        turn_id: summary.id.clone(),
        user: preview.clone().map(|content| ChatMessageView {
            id: format!("{}:input:0", summary.id),
            role: "user".into(),
            content,
            ref_: None,
        }),
        assistant_reasoning: None,
        assistant: None,
        run,
        tool_chains: Vec::new(),
    }
}

/// Fills a summary turn from the full run: the untruncated text input, the
/// last assistant message, and the run's tool chains.
fn apply_run_detail(turn: &mut ChatTurn, run: &api::RunView) {
    let api::RunViewSource::Input { items } = &run.source;
    if let Some(InputItem::Text { text, .. }) = items.first()
        && let Some(user) = turn.user.as_mut()
    {
        user.content = text.clone();
    }
    turn.assistant = run.entries.iter().rev().find_map(|entry| match entry.kind {
        ContextEntryKindView::Message {
            role: ContextMessageRoleView::Assistant,
        } => Some(ChatMessageView {
            id: entry.id.clone(),
            role: "assistant".into(),
            content: entry
                .text
                .clone()
                .or_else(|| entry.preview.clone())
                .unwrap_or_else(|| "[media]".to_owned()),
            ref_: None,
        }),
        _ => None,
    });
    turn.tool_chains = project_tool_chains(run);
    if let Some(view) = turn.run.as_mut() {
        let stats = &mut view.stats;
        stats.usage = run.usage.clone().or(stats.usage.take());
        stats.duration_ms =
            run_duration_ms(run.started_at_ms, run.completed_at_ms).or(stats.duration_ms);
        stats.tool_calls = Some(turn.tool_chains.iter().map(|chain| chain.calls.len()).sum());
    }
}

fn run_duration_ms(started_at_ms: Option<u64>, completed_at_ms: Option<u64>) -> Option<u64> {
    Some(completed_at_ms?.saturating_sub(started_at_ms?))
}

fn project_tool_chains(run: &api::RunView) -> Vec<ChatToolChainView> {
    let mut chains = run
        .tool_batches
        .iter()
        .map(|batch| project_tool_batch(&run.id, batch))
        .collect::<Vec<_>>();
    chains.extend(project_provider_tool_chains(&run.id, &run.entries));
    chains
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
        stats: Box::new(ChatRunStats {
            usage: run.usage.clone(),
            duration_ms: run_duration_ms(run.started_at_ms, run.completed_at_ms),
            ..Default::default()
        }),
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
        provider: args.provider.clone().unwrap_or_default(),
        api_kind: args.api_kind.clone().unwrap_or_default(),
        model: args.model.clone().unwrap_or_default(),
        route_requested: args.model.is_some(),
        reasoning_effort,
        max_tokens: args.max_tokens,
        web_search: args.no_web_search.then_some(false),
        web_fetch: args.no_web_fetch.then_some(false),
        bare: args.bare,
    })
}

/// `None` leaves the model to the session, or to the deployment default.
fn model_config(settings: &ChatDraftSettings) -> Option<ModelConfig> {
    settings.route_requested.then(|| ModelConfig {
        provider_id: settings.provider.clone(),
        api_kind: settings.api_kind.clone(),
        model: settings.model.clone(),
    })
}

fn session_start_config(settings: &ChatDraftSettings) -> api::SessionConfig {
    api::SessionConfig {
        model: model_config(settings),
        generation: Some(generation_config(settings)),
        limits: None,
        context: None,
        features: (!settings.bare).then(|| dev_features(settings)),
    }
}

/// The CLI's development defaults: features are secure-by-default on the
/// server (absent = off), so the chat client grants a usable dev surface
/// explicitly — VFS with prompt sourcing, web, timers. File tools appear once
/// a workspace is attached (`--upload` or `--workspace`); skill discovery requires an explicit
/// profile/session configuration.
fn dev_features(settings: &ChatDraftSettings) -> FeaturesConfig {
    let web_fetch = settings.web_fetch.unwrap_or(true);
    // An unknown api kind (deployment default) is left to server validation.
    let web_search = settings.web_search.unwrap_or(true)
        && matches!(
            settings.api_kind.as_str(),
            "" | "openai:responses" | "anthropic:messages"
        );
    FeaturesConfig {
        vfs: Some(VfsFeature {
            working_directory: None,
            version: api::CURRENT_FEATURE_VERSION,
            workspaces: Vec::new(),
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
        model: model_config(settings),
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

fn print_event(event: &ChatEvent, show_stats: bool) -> Result<()> {
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
        ChatEvent::SkillsListed { .. } | ChatEvent::ModelsListed { .. } => {}
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
                if show_stats
                    && let Some(summary) = turn
                        .run
                        .as_ref()
                        .and_then(|run| run_stats_summary(&run.stats))
                {
                    println!("{summary}\n");
                }
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

    fn session_fixture(provider: &str, api_kind: &str, model: &str) -> SessionView {
        serde_json::from_value(serde_json::json!({
            "id": "session_target", "status": "idle", "activity": "idle",
            "retention": {"rootSessionId": "session_target"}, "managed": false,
            "access": {"visibility": "restricted"}, "configRevision": 0,
            "createdAtMs": 1, "updatedAtMs": 1, "activeContext": {"revision": 0},
            "config": {"model": {"providerId": provider, "apiKind": api_kind, "model": model}},
        }))
        .expect("session fixture")
    }

    fn driver_fixture(
        endpoint: &str,
        provider: &str,
        api_kind: &str,
        model: &str,
    ) -> ChatSessionDriver {
        let mut driver = ChatSessionDriver {
            api: Arc::new(HttpAgentApi::new(endpoint)),
            session_id: "session_original".into(),
            settings: ChatDraftSettings::default(),
            event_cursor: None,
            turns: Vec::new(),
            active_tool_chains: Vec::new(),
            observed_tool_chains: BTreeMap::new(),
            run_states: BTreeMap::new(),
            finished_turns: BTreeMap::new(),
            session_model: None,
            transcript_errors: BTreeMap::new(),
            generation_stats: BTreeMap::new(),
            pending_run: None,
            notice_seq: 0,
        };
        driver
            .sync_session_model(&session_fixture(provider, api_kind, model))
            .unwrap();
        driver
    }

    /// Expected JSON-RPC exchanges, with a finite accept deadline so a missing
    /// request fails the test instead of hanging it.
    async fn mock_api(exchanges: Vec<(&'static str, Value)>) -> (String, JoinHandle<Vec<Value>>) {
        mock_api_with_hook(exchanges, |_| {}).await
    }

    async fn mock_api_with_hook(
        exchanges: Vec<(&'static str, Value)>,
        hook: impl Fn(&Value) + Send + 'static,
    ) -> (String, JoinHandle<Vec<Value>>) {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (method, response) in exchanges {
                let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                    .await
                    .expect("expected request")
                    .unwrap();
                let mut stream = BufReader::new(stream);
                let mut length = None;
                loop {
                    let mut line = String::new();
                    assert!(stream.read_line(&mut line).await.unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = Some(value.trim().parse::<usize>().unwrap());
                    }
                }
                let mut body = vec![0; length.expect("request length")];
                stream.read_exact(&mut body).await.unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(request["method"], method);
                hook(&request);
                let mut response = response;
                response["jsonrpc"] = "2.0".into();
                response["id"] = request["id"].clone();
                let body = serde_json::to_vec(&response).unwrap();
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .get_mut()
                    .write_all(headers.as_bytes())
                    .await
                    .unwrap();
                stream.get_mut().write_all(&body).await.unwrap();
                requests.push(request);
            }
            requests
        });
        (endpoint, task)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn model_commands_keep_provider_and_api_kind_fixed() {
        let mut driver = driver_fixture("http://unused", "openai", "openai:responses", "gpt-sol");
        for model in ["gpt-astra", "gpt-sol"] {
            let events = driver
                .handle_command(ChatCommand::SetDraftModel {
                    model: model.into(),
                })
                .await
                .unwrap();
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, ChatEvent::Error(_)))
            );
            assert_eq!(model_config(&driver.settings).unwrap().model, model);
        }
        let before = driver.settings.clone();
        for command in [
            ChatCommand::SetDraftProvider {
                provider: "other-openai-endpoint".into(),
            },
            ChatCommand::SetDraftRoute {
                provider: "other-openai-endpoint".into(),
                api_kind: "openai:responses".into(),
                model: "gpt-sol".into(),
            },
            ChatCommand::SetDraftRoute {
                provider: "openai".into(),
                api_kind: "openai:completions".into(),
                model: "gpt-sol".into(),
            },
        ] {
            assert!(matches!(
                driver.handle_command(command).await.unwrap().as_slice(),
                [ChatEvent::Error(_)]
            ));
            assert_eq!(driver.settings, before);
        }
        let mut aggregator = driver_fixture(
            "http://unused",
            "openrouter",
            "openai:completions",
            "deepseek/model",
        );
        let events = aggregator.set_model("glm/model".into()).await.unwrap();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ChatEvent::Error(_)))
        );
        assert_eq!(
            model_config(&aggregator.settings).unwrap().provider_id,
            "openrouter"
        );
        let mut direct = driver_fixture(
            "http://unused",
            "deepseek",
            "openai:completions",
            "deepseek-model",
        );
        assert!(matches!(
            direct
                .set_route(
                    "glm".into(),
                    "openai:completions".into(),
                    "glm-model".into()
                )
                .await
                .unwrap()
                .as_slice(),
            [ChatEvent::Error(_)]
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn model_changes_wait_for_pending_submissions_and_active_runs() {
        let mut driver = driver_fixture("http://unused", "openai", "openai:responses", "gpt-sol");
        driver.pending_run = Some(tokio::spawn(std::future::pending()));
        assert!(matches!(
            driver
                .set_model("gpt-astra".into())
                .await
                .unwrap()
                .as_slice(),
            [ChatEvent::Error(_)]
        ));
        driver.pending_run.take().unwrap().abort();
        for status in [
            api::RunStatus::Queued,
            api::RunStatus::Running,
            api::RunStatus::Parked,
        ] {
            driver.run_states.insert(
                1,
                TrackedRun {
                    id: "run_1".into(),
                    status,
                },
            );
            assert!(matches!(
                driver
                    .set_model("gpt-astra".into())
                    .await
                    .unwrap()
                    .as_slice(),
                [ChatEvent::Error(_)]
            ));
        }
        assert_eq!(driver.settings.model, "gpt-sol");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opening_session_validates_explicit_route_flags() {
        use serde_json::json;
        for (provider, kind, accepted) in [
            ("openai", "openai:responses", true),
            ("other", "openai:responses", false),
            ("openai", "openai:completions", false),
        ] {
            let session = session_fixture("openai", "openai:responses", "gpt-sol");
            let (endpoint, server) = mock_api(vec![
                ("session/start", json!({"error":{"code": -32000,"message":"exists","data":{"kind":"conflict","message":"exists"}}})),
                ("session/read", json!({"result":{"result":{"session":session,"hasOlderRuns":false}}})),
                ("session/events/read", json!({"result":{"result":{"events":[],"complete":true}}})),
                ("session/read", json!({"result":{"result":{"session":session,"hasOlderRuns":false}}})),
            ]).await;
            let result = ChatSessionDriver::open(ChatSessionDriverOptions {
                session_id: "session_target".into(),
                api_url: endpoint,
                profile: None,
                draft_settings: ChatDraftSettings {
                    provider: provider.into(),
                    api_kind: kind.into(),
                    model: "gpt-astra".into(),
                    route_requested: true,
                    bare: true,
                    ..Default::default()
                },
            })
            .await;
            assert_eq!(result.is_ok(), accepted, "{provider} / {kind}");
            server.await.unwrap();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn switching_session_discards_previous_model_override() {
        use serde_json::json;
        let session = session_fixture("anthropic", "anthropic:messages", "claude-model");
        let (endpoint, server) = mock_api(vec![
            (
                "session/read",
                json!({"result":{"result":{"session":session,"hasOlderRuns":false}}}),
            ),
            (
                "session/events/read",
                json!({"result":{"result":{"events":[],"complete":true}}}),
            ),
            (
                "session/read",
                json!({"result":{"result":{"session":session,"hasOlderRuns":false}}}),
            ),
        ])
        .await;
        let mut driver = driver_fixture(&endpoint, "openai", "openai:responses", "gpt-sol");
        driver.set_model("gpt-astra".into()).await.unwrap();
        driver
            .switch_session("session_target".into())
            .await
            .unwrap();
        assert_eq!(driver.settings.provider, "anthropic");
        assert_eq!(driver.settings.api_kind, "anthropic:messages");
        assert_eq!(driver.settings.model, "claude-model");
        assert!(model_config(&driver.settings).is_none());
        assert!(run_start_config(&driver.settings).model.is_none());
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_transcript_reads_are_reported_retried_and_then_cached() {
        use serde_json::json;
        let error = json!({"error":{"code":-32603,"message":"temporary read failure"}});
        let (endpoint, server) = mock_api(vec![
            ("session/runs/read", error.clone()), ("session/runs/read", error),
            ("session/runs/read", json!({"result":{"result":{"run":{
                "id":"run_1","status":"completed","source":{"type":"input","items":[{"type":"text","text":"hello"}]},
                "entries":[{"id":"item_1","kind":{"type":"message","role":"assistant"},"content":{"contentRef":"sha256:a"},"text":"world"}]
            }}}})),
        ]).await;
        let mut driver = driver_fixture(&endpoint, "openai", "openai:responses", "gpt-sol");
        let mut session = session_fixture("openai", "openai:responses", "gpt-sol");
        session.runs = vec![serde_json::from_value(json!({"id":"run_1","status":"completed","acceptedAtMs":1,"source":{"type":"input","preview":"hello"}})).unwrap()];
        let (turns, events) = driver.project_turns(&session).await;
        assert_eq!(turns.len(), 1);
        assert!(matches!(events.as_slice(), [ChatEvent::Error(_)]));
        assert!(driver.finished_turns.is_empty());
        assert!(driver.ensure_transcript_loaded().is_err());
        assert!(
            driver.project_turns(&session).await.1.is_empty(),
            "same error is not repeated"
        );
        let (turns, events) = driver.project_turns(&session).await;
        assert!(events.is_empty());
        assert_eq!(turns[0].assistant.as_ref().unwrap().content, "world");
        driver.ensure_transcript_loaded().unwrap();
        assert_eq!(driver.project_turns(&session).await.0, turns);
        assert_eq!(server.await.unwrap().len(), 3);
    }

    #[test]
    fn cancelled_tool_status_is_rendered_neutrally() {
        assert_eq!(
            tool_status(ToolItemStatus::Cancelled),
            ChatProgressStatus::Cancelled
        );
    }

    #[test]
    fn finished_run_turn_shows_full_input_and_last_assistant_message() {
        let summary: api::RunSummaryView = serde_json::from_value(serde_json::json!({
            "id": "run_2",
            "status": "completed",
            "acceptedAtMs": 1,
            "completedAtMs": 2,
            "source": { "type": "input", "preview": "Say hel…", "previewTruncated": true },
        }))
        .expect("run summary");
        let run: api::RunView = serde_json::from_value(serde_json::json!({
            "id": "run_2",
            "status": "completed",
            "source": { "type": "input", "items": [{ "type": "text", "text": "Say hello world" }] },
            "entries": [
                {
                    "id": "item_3",
                    "kind": { "type": "message", "role": "assistant" },
                    "content": { "contentRef": "sha256:a" },
                    "source": { "type": "assistantOutput", "runId": "run_2", "turnId": "turn_1" },
                    "text": "draft",
                },
                {
                    "id": "item_5",
                    "kind": { "type": "message", "role": "assistant" },
                    "content": { "contentRef": "sha256:b" },
                    "source": { "type": "assistantOutput", "runId": "run_2", "turnId": "turn_2" },
                    "text": "Hello, world!",
                },
            ],
        }))
        .expect("run view");

        let mut turn = turn_from_summary(&summary, &ChatDraftSettings::default());
        assert_eq!(
            turn.user.as_ref().map(|user| user.content.as_str()),
            Some("Say hel…")
        );
        assert!(turn.assistant.is_none());

        apply_run_detail(&mut turn, &run);

        assert_eq!(turn.turn_id, "run_2");
        assert_eq!(
            turn.user.as_ref().map(|user| user.content.as_str()),
            Some("Say hello world")
        );
        let assistant = turn.assistant.expect("assistant message");
        assert_eq!(assistant.id, "item_5");
        assert_eq!(assistant.content, "Hello, world!");
    }

    #[test]
    fn finished_run_turn_collects_usage_duration_and_tool_count() {
        let summary: api::RunSummaryView = serde_json::from_value(serde_json::json!({
            "id": "run_3",
            "status": "completed",
            "acceptedAtMs": 1,
            "startedAtMs": 1_000,
            "completedAtMs": 13_345,
            "source": { "type": "input", "preview": "hi" },
            "usage": { "inputTokens": 900, "outputTokens": 40, "cachedInputTokens": 800 },
        }))
        .expect("run summary");
        let run: api::RunView = serde_json::from_value(serde_json::json!({
            "id": "run_3",
            "status": "completed",
            "startedAtMs": 1_000,
            "completedAtMs": 13_345,
            "source": { "type": "input", "items": [{ "type": "text", "text": "hi" }] },
            "usage": { "inputTokens": 1_000, "outputTokens": 50, "cachedInputTokens": 800 },
        }))
        .expect("run view");

        let mut turn = turn_from_summary(&summary, &ChatDraftSettings::default());
        let stats = &turn.run.as_ref().expect("run").stats;
        assert_eq!(stats.duration_ms, Some(12_345));
        assert_eq!(
            stats.usage.as_ref().and_then(|usage| usage.input_tokens),
            Some(900)
        );
        assert_eq!(stats.tool_calls, None);

        apply_run_detail(&mut turn, &run);

        let stats = &turn.run.as_ref().expect("run").stats;
        assert_eq!(
            stats.usage.as_ref().and_then(|usage| usage.input_tokens),
            Some(1_000)
        );
        assert_eq!(stats.duration_ms, Some(12_345));
        assert_eq!(stats.tool_calls, Some(0));
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
                    media: Vec::new(),
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
                    media_handle: None,
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

        assert!(rendered.contains("catalog sha256:catalog"));
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
        assert!(vfs.workspaces.is_empty());
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

    #[test]
    fn omitted_route_leaves_model_to_deployment_default() {
        let args = ChatArgs {
            provider: None,
            api_kind: None,
            model: None,
            ..chat_args_with_effort(Some("high"))
        };
        let settings = draft_settings(&args).expect("draft settings");
        assert!(!settings.route_requested);

        let session = session_start_config(&settings);
        assert!(session.model.is_none());
        // Effort depends on the api kind, which is unknown until the session
        // resolves the default; runs send it once the session is read.
        assert_eq!(
            session.generation.expect("generation").reasoning_effort,
            None
        );
        assert!(
            session
                .features
                .and_then(|features| features.web)
                .is_some_and(|web| web.search.is_some())
        );
        assert!(run_start_config(&settings).model.is_none());
    }

    #[test]
    fn route_flags_must_be_given_together() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            chat: ChatArgs,
        }
        use clap::Parser;

        let partial = Cli::try_parse_from(["chat", "--model", "gpt-5.4"]);
        assert!(partial.is_err());
        let full = Cli::try_parse_from([
            "chat",
            "--provider",
            "openai",
            "--api-kind",
            "openai:responses",
            "--model",
            "gpt-5.4",
        ]);
        assert!(full.is_ok());
    }

    fn chat_args_with_effort(effort: Option<&str>) -> ChatArgs {
        ChatArgs {
            session: None,
            list: false,
            resume: false,
            new: true,
            provider: Some("openai".into()),
            api_kind: Some("openai:responses".into()),
            model: Some("gpt-5.5".into()),
            effort: effort.map(str::to_string),
            max_tokens: None,
            no_web_search: false,
            no_web_fetch: false,
            bare: false,
            profile: None,
            profile_json: None,
            upload: None,
            workspace: None,
            workspace_path: "/workspace".into(),
            workspace_access: crate::vfs_cli::WorkspaceAccessArg::Edit,
            api_url: "http://127.0.0.1:18080/rpc".into(),
            show_tool_details: false,
            show_stats: false,
            json: false,
            message: Vec::new(),
        }
    }

    #[test]
    fn live_tool_calls_update_individually_and_keep_failed_and_cancelled_results() {
        let mut driver = driver_fixture("http://unused", "openai", "openai:responses", "gpt-sol");
        let started = tool_event(SessionEventKindView::ToolBatchStarted {
            run_id: "run_1".into(),
            turn_id: "turn_1".into(),
            batch_id: "batch_1".into(),
            calls: vec![live_call("a"), live_call("b")],
        });
        driver.chat_events_from_session_event(&started);
        for (id, status, expected) in [
            ("a", ToolItemStatus::Succeeded, ChatProgressStatus::Running),
            ("b", ToolItemStatus::Failed, ChatProgressStatus::Failed),
        ] {
            let events = driver.chat_events_from_session_event(&tool_event(
                SessionEventKindView::ToolCallCompleted {
                    run_id: "run_1".into(),
                    turn_id: "turn_1".into(),
                    batch_id: "batch_1".into(),
                    call_id: id.into(),
                    status,
                    effects: vec![],
                    output_bytes: None,
                    truncated: false,
                },
            ));
            assert!(matches!(
                events.first(),
                Some(ChatEvent::ToolChainsChanged { .. })
            ));
            assert_eq!(driver.active_tool_chains[0].status, expected);
            assert_eq!(
                driver.active_tool_chains[0]
                    .calls
                    .iter()
                    .find(|call| call.id == id)
                    .unwrap()
                    .status,
                tool_status(status)
            );
        }
        let cancelled =
            driver.update_live_tool("run_1", "batch_1", "b", ChatProgressStatus::Cancelled);
        assert!(!cancelled.is_empty());
        assert_eq!(
            driver.active_tool_chains[0].status,
            ChatProgressStatus::Cancelled
        );
        assert_eq!(
            driver.observed_tool_chains["run_1"],
            driver.active_tool_chains
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tui_follows_an_existing_run_and_handles_interrupt_between_event_pages() {
        use crate::chat::tui::{
            app::spawn_driver_task, app_event::UiEvent, app_event_sender::AppEventSender,
        };
        use serde_json::json;
        let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let send_command = commands.clone();
        let sent_interrupt = std::sync::atomic::AtomicBool::new(false);
        let (event_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        let first_event = tool_event(SessionEventKindView::ToolBatchStarted {
            run_id: "run_1".into(),
            turn_id: "turn_1".into(),
            batch_id: "batch_1".into(),
            calls: vec![live_call("a")],
        });
        let (endpoint, server) = mock_api_with_hook(
            vec![
                (
                    "session/events/read",
                    json!({"result":{"result":{"events":[first_event],"complete":true}}}),
                ),
                (
                    "session/runs/cancel",
                    json!({"error":{"code":-32603,"message":"fixture cancellation response"}}),
                ),
                (
                    "session/events/read",
                    json!({"error":{"code":-32603,"message":"end fixture"}}),
                ),
            ],
            move |request| {
                if request["method"] == "session/events/read"
                    && !sent_interrupt.swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    let _ = send_command.send(ChatCommand::InterruptRun { reason: None });
                }
            },
        )
        .await;
        let mut driver = driver_fixture(&endpoint, "openai", "openai:responses", "gpt-sol");
        driver.run_view_from_status("run_1", api::RunStatus::Running, 1);
        spawn_driver_task(driver, command_rx, AppEventSender::new(event_tx));
        let mut saw_tools = false;
        let mut saw_interrupt_response = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = events.recv().await {
                match event {
                    UiEvent::Chat(ChatEvent::ToolChainsChanged { chains, .. }) => {
                        saw_tools |= !chains.is_empty()
                    }
                    UiEvent::Chat(ChatEvent::Error(error))
                        if error.message.contains("fixture cancellation response") =>
                    {
                        saw_interrupt_response = true;
                    }
                    UiEvent::Chat(ChatEvent::Error(error))
                        if error.message.contains("end fixture") =>
                    {
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("live tool updates and interrupt response before run completion");
        assert!(saw_tools);
        assert!(saw_interrupt_response);
        let requests = server.await.unwrap();
        assert_eq!(requests[1]["params"]["runId"], "run_1");
        drop(commands);
    }

    fn live_call(id: &str) -> ToolCallEventView {
        ToolCallEventView {
            tool_id: None,
            call_id: id.into(),
            tool_name: "read_file".into(),
            arguments_ref: "sha256:fixture".into(),
            arguments: Some(r#"{"path":"src/lib.rs"}"#.into()),
            display: None,
        }
    }

    fn tool_event(kind: SessionEventKindView) -> SessionEventView {
        SessionEventView {
            cursor: EventCursor { seq: 1 },
            session_id: "session_original".into(),
            observed_at_ms: 1,
            joins: Default::default(),
            kind,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_events_are_emitted_before_snapshot_reads_and_survive_active_run_projection() {
        use serde_json::json;
        let started = tool_event(SessionEventKindView::ToolBatchStarted {
            run_id: "run_1".into(),
            turn_id: "turn_1".into(),
            batch_id: "batch_1".into(),
            calls: vec![live_call("a")],
        });
        let completed = tool_event(SessionEventKindView::ToolCallCompleted {
            run_id: "run_1".into(),
            turn_id: "turn_1".into(),
            batch_id: "batch_1".into(),
            call_id: "a".into(),
            status: ToolItemStatus::Succeeded,
            effects: vec![],
            output_bytes: Some(5),
            truncated: false,
        });
        let batch_done = tool_event(SessionEventKindView::ToolBatchCompleted {
            run_id: "run_1".into(),
            turn_id: "turn_1".into(),
            batch_id: "batch_1".into(),
        });
        let (endpoint, server) = mock_api(vec![
            ("session/events/read", json!({"result":{"result":{"events":[started, completed, batch_done],"complete":true}}})),
            ("session/read", json!({"error":{"code":-32603,"message":"snapshot unavailable"}})),
        ]).await;
        let mut driver = driver_fixture(&endpoint, "openai", "openai:responses", "gpt-sol");
        let mut emitted = Vec::new();
        assert!(
            driver
                .stream_event_log(None, &mut |event| emitted.push(event))
                .await
                .is_err()
        );
        let states: Vec<_> = emitted
            .iter()
            .filter_map(|event| match event {
                ChatEvent::ToolChainsChanged { chains, .. } => Some(chains[0].calls[0].status),
                _ => None,
            })
            .collect();
        assert_eq!(
            states,
            vec![ChatProgressStatus::Running, ChatProgressStatus::Succeeded]
        );
        let mut session = session_fixture("openai", "openai:responses", "gpt-sol");
        session.active_run = Some(serde_json::from_value(json!({"id":"run_1","status":"running","acceptedAtMs":1,"source":{"type":"input","preview":"read files"}})).unwrap());
        let (turns, errors) = driver.project_turns(&session).await;
        assert!(errors.is_empty());
        assert_eq!(
            turns[0].tool_chains[0].calls[0].status,
            ChatProgressStatus::Succeeded
        );
        assert_eq!(server.await.unwrap().len(), 2);
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
            requested_by: None,
        }));
        assert!(!event_needs_snapshot(&SessionEventKindView::RunStarted {
            run_id: "run_1".into(),
        }));
    }
}

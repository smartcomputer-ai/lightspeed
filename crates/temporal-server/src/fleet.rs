//! Hosted Fleet subagent control-plane service.

use std::sync::Arc;

use api::{
    AgentApiError, AgentApiService, AgentProfile, AgentProfileSummary, EventCursor, InputItem,
    MediaKind, ProfileId, ProfileListParams, ProfileReadParams, ProfileSource, RunCancelParams,
    RunStartParams, RunStartSource, RunStatus as ApiRunStatus, SessionCloseParams,
    SessionEnvironmentListParams, SessionEnvironmentListResponse, SessionEventsReadParams,
    SessionEventsReadResponse, SessionReadParams, SessionView,
};
use api_projection::{MAX_EVENT_PAGE_LIMIT, read_all_session_entries, replay_core_agent_state};
use async_trait::async_trait;
use engine::{
    BlobRef, ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentIoError, EventSeq,
    RunId, SessionId, SubmissionId, ToolBatchId, ToolBatchOutcome, ToolBatchResumeDirective,
    ToolCallId, ToolCallStatus, ToolInvocationBatchResult, ToolInvocationRequest,
    ToolInvocationResult, TurnId, core_agent_clone_opening_events,
    storage::{
        BlobStore, BlobStoreError, CreateClonedSession, CreateForkedSession, ListSessionLinks,
        SessionLinkDirection, SessionRecord, SessionStore, SessionStoreError, UpsertSessionLink,
    },
};
use serde::Serialize;
use serde_json::{Value, json};
use temporal_workflow::{
    AgentWaitDirective, AgentWaitHandle, AgentWaitHandleResult, AgentWaitHandleStatus,
    AgentWaitMode as WorkflowAgentWaitMode, AgentWaitOutcome, AgentWaitOutput, AgentWaitRunResult,
    FLEET_AGENT_WAIT_DIRECTIVE_KIND,
};
use tools::fleet::{
    AGENT_CANCEL_TOOL_NAME, AGENT_LIST_TOOL_NAME, AGENT_READ_TOOL_NAME, AGENT_SEND_TOOL_NAME,
    AGENT_SPAWN_TOOL_NAME, AGENT_WAIT_TOOL_NAME, AgentCancelArgs, AgentCancelOutput,
    AgentCancelScope, AgentLineageView, AgentLinkView, AgentListArgs, AgentListDirection,
    AgentListItem, AgentListOutput, AgentReadArgs, AgentReadOutput, AgentReportBack, AgentSendArgs,
    AgentSendInputItem, AgentSendMediaKind, AgentSendOutput, AgentSendStatus, AgentSendTarget,
    AgentSpawnArgs, AgentSpawnBase, AgentSpawnFork, AgentSpawnOutput, AgentWaitArgs,
    AgentWaitMode as ToolAgentWaitMode, EnvironmentPolicy, PROFILE_LIST_TOOL_NAME,
    PROFILE_READ_TOOL_NAME, ProfileListArgs, ProfileListOutput, ProfileReadArgs, ProfileReadOutput,
    VfsPolicy,
};
use vfs::{
    CreateVfsWorkspaceRecord, VfsCatalogError, VfsMountSource, VfsMountStore, VfsPath,
    VfsWorkspaceId, VfsWorkspaceStore,
};

use crate::gateway::GatewayAgentApi;

pub const FLEET_CHILD_RELATIONSHIP: &str = "fleet_child";
const DEFAULT_AGENT_LIST_LIMIT: usize = 20;
const MAX_AGENT_LIST_LIMIT: usize = 100;
const DEFAULT_RECENT_EVENT_LIMIT: u32 = 20;
const DEFAULT_RECENT_TRANSCRIPT_EVENT_LIMIT: u32 = 20;
const MAX_RECENT_EVENT_LIMIT: u32 = 100;
const MAX_DIRECT_LINKS: usize = 100;
const MAX_AGENT_WAIT_HANDLES: usize = 32;
const MAX_AGENT_READ_VISIBLE_CHARS: usize = 20_000;
const MAX_AGENT_READ_VISIBLE_RUNS: usize = 2;

#[derive(Clone)]
pub struct FleetService {
    sessions: Arc<dyn SessionStore>,
    runtime: Arc<dyn FleetChildRuntime>,
    workspace_store: Option<Arc<dyn VfsWorkspaceStore>>,
    mount_store: Option<Arc<dyn VfsMountStore>>,
}

impl FleetService {
    pub fn new(sessions: Arc<dyn SessionStore>, runtime: Arc<dyn FleetChildRuntime>) -> Self {
        Self {
            sessions,
            runtime,
            workspace_store: None,
            mount_store: None,
        }
    }

    pub fn with_vfs_stores(
        mut self,
        workspace_store: Arc<dyn VfsWorkspaceStore>,
        mount_store: Arc<dyn VfsMountStore>,
    ) -> Self {
        self.workspace_store = Some(workspace_store);
        self.mount_store = Some(mount_store);
        self
    }

    pub async fn spawn(
        &self,
        context: FleetInvocationContext,
        args: AgentSpawnArgs,
    ) -> Result<AgentSpawnOutput, AgentApiError> {
        validate_spawn_args(&args)?;
        let child_id_was_derived = args.child_session_id.is_none();
        let child_session_id = match args.child_session_id.as_deref() {
            Some(session_id) => parse_session_id(session_id, "child_session_id")?,
            None => derived_child_session_id(&context),
        };
        let spawn_request_id = spawn_request_id(&context);
        let child_run_submission_id = child_run_submission_id(&context);

        let (outcome, source_session_id, source_seq) = if let Some(profile) = args.base.profile() {
            let existed = self
                .sessions
                .load_session(&child_session_id)
                .await
                .map_err(map_session_store_error)?
                .is_some();
            self.runtime
                .start_session(
                    &child_session_id,
                    args.lifecycle.close_on_terminal,
                    Some(profile.clone()),
                )
                .await?;
            if !existed {
                (ChildCreateOutcome::Created, None, None)
            } else {
                (
                    ChildCreateOutcome::Reused {
                        matching_spawn_link: false,
                    },
                    None,
                    None,
                )
            }
        } else {
            let source_session_id = self.resolve_base_source(&context, &args.base)?;
            let source_record = self.load_session_required(&source_session_id).await?;
            let source_seq = if let Some(fork) = args.base.fork() {
                Some(match fork {
                    AgentSpawnFork::Safe => self
                        .sessions
                        .safe_fork_seq(&source_session_id)
                        .await
                        .map_err(map_session_store_error)?,
                    AgentSpawnFork::AtSeq { seq } => EventSeq::new(*seq),
                })
            } else {
                None
            };

            let outcome = self
                .create_or_reuse_child(
                    &context,
                    &source_record,
                    &child_session_id,
                    source_seq,
                    &spawn_request_id,
                    child_id_was_derived,
                )
                .await?;
            (outcome, Some(source_session_id), source_seq)
        };
        let skip_pre_run_setup = outcome.has_matching_spawn_link();
        if args.base.profile().is_none() && !skip_pre_run_setup {
            self.apply_resource_policies(&child_session_id, context.observed_at_ms, &args)
                .await?;
        }

        if args.base.profile().is_none() {
            self.runtime
                .start_session(&child_session_id, args.lifecycle.close_on_terminal, None)
                .await?;
        }
        self.upsert_spawn_link(
            &context,
            source_session_id.as_ref(),
            &child_session_id,
            source_seq,
            &spawn_request_id,
            &args,
        )
        .await?;
        let child_run_id = if args.lifecycle.run_immediately {
            Some(
                self.runtime
                    .start_run(
                        &child_session_id,
                        vec![InputItem::Text {
                            text: spawn_run_text(&context, &args),
                        }],
                        child_run_submission_id,
                    )
                    .await?,
            )
        } else {
            None
        };

        Ok(AgentSpawnOutput {
            child_session_id: child_session_id.as_str().to_owned(),
            child_run_id,
            status: if matches!(outcome, ChildCreateOutcome::Created) {
                "created".to_owned()
            } else {
                "reused".to_owned()
            },
        })
    }

    pub async fn list_profiles(
        &self,
        _args: ProfileListArgs,
    ) -> Result<ProfileListOutput, AgentApiError> {
        Ok(ProfileListOutput {
            profiles: self.runtime.list_profiles().await?,
        })
    }

    pub async fn read_profile(
        &self,
        args: ProfileReadArgs,
    ) -> Result<ProfileReadOutput, AgentApiError> {
        let profile_id = ProfileId::try_new(args.profile_id).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid profile_id: {error}"))
        })?;
        Ok(ProfileReadOutput {
            profile: self.runtime.read_profile(profile_id).await?,
        })
    }

    pub async fn send(
        &self,
        context: FleetInvocationContext,
        args: AgentSendArgs,
    ) -> Result<AgentSendOutput, AgentApiError> {
        validate_send_args(&args)?;
        let Some(target_session_id) = self.resolve_send_target(&context, &args.to).await? else {
            return Ok(AgentSendOutput {
                target_session_id: None,
                run_id: None,
                submission_id: None,
                status: AgentSendStatus::NotReachable,
            });
        };
        if !self
            .has_session_link_edge(&context.parent_session_id, &target_session_id)
            .await?
        {
            return Ok(AgentSendOutput {
                target_session_id: Some(target_session_id.as_str().to_owned()),
                run_id: None,
                submission_id: None,
                status: AgentSendStatus::NotReachable,
            });
        }
        self.load_session_required(&target_session_id).await?;
        self.runtime
            .start_session(&target_session_id, false, None)
            .await?;
        let input = send_run_input(&context, &args)?;
        let submission_id = send_submission_id(&context, &target_session_id);
        let run_id = self
            .runtime
            .enqueue_run(&target_session_id, input, submission_id.clone())
            .await?;
        Ok(AgentSendOutput {
            target_session_id: Some(target_session_id.as_str().to_owned()),
            run_id: Some(run_id),
            submission_id: Some(submission_id.as_str().to_owned()),
            status: AgentSendStatus::Delivered,
        })
    }

    pub async fn wait(
        &self,
        context: FleetInvocationContext,
        call_id: ToolCallId,
        args: AgentWaitArgs,
    ) -> Result<FleetWaitPreflight, AgentApiError> {
        let wait_args = validate_wait_args(args)?;
        let mut handles = Vec::with_capacity(wait_args.handles.len());
        let mut results = Vec::with_capacity(wait_args.handles.len());

        for handle in wait_args.handles {
            let target_session_id = handle.target_session_id;
            let run_id = handle.run_id;
            let run_id_text = api_run_id(run_id);
            handles.push(AgentWaitHandle {
                target_session_id: target_session_id.clone(),
                run_id,
            });

            if target_session_id == context.parent_session_id && run_id == context.parent_run_id {
                results.push(wait_error_result(
                    &target_session_id,
                    run_id,
                    "agent_wait cannot wait on the current run",
                ));
                continue;
            }
            if target_session_id != context.parent_session_id
                && !self
                    .has_session_link_edge(&context.parent_session_id, &target_session_id)
                    .await?
            {
                results.push(wait_error_result(
                    &target_session_id,
                    run_id,
                    "target session is not reachable",
                ));
                continue;
            }
            if let Err(error) = self.load_session_required(&target_session_id).await {
                results.push(wait_error_result(
                    &target_session_id,
                    run_id,
                    error.to_string(),
                ));
                continue;
            }
            if let Some(result) = self
                .wait_result_from_store(&target_session_id, run_id)
                .await?
                && result.status == AgentWaitHandleStatus::Terminal
            {
                results.push(result);
                continue;
            }
            if let Err(error) = self
                .runtime
                .start_session(&target_session_id, false, None)
                .await
            {
                results.push(wait_error_result(
                    &target_session_id,
                    run_id,
                    error.to_string(),
                ));
                continue;
            }
            if let Some(result) = self
                .wait_result_from_store(&target_session_id, run_id)
                .await?
            {
                results.push(result);
                continue;
            }
            let session = match self.runtime.read_session(&target_session_id).await {
                Ok(session) => session,
                Err(error) => {
                    results.push(wait_error_result(
                        &target_session_id,
                        run_id,
                        error.to_string(),
                    ));
                    continue;
                }
            };
            let Some(run) = session.runs.iter().find(|run| run.id == run_id_text) else {
                results.push(wait_error_result(
                    &target_session_id,
                    run_id,
                    "run not found in target session",
                ));
                continue;
            };
            if api_run_status_is_terminal(run.status)
                || matches!(
                    session.status,
                    api::SessionStatus::Closed | api::SessionStatus::Error
                )
            {
                results.push(wait_terminal_result(&target_session_id, run_id, run.status));
            } else if target_session_id == context.parent_session_id {
                results.push(wait_error_result(
                    &target_session_id,
                    run_id,
                    "agent_wait cannot park on a non-terminal run in the calling session",
                ));
            } else {
                results.push(wait_pending_result(&target_session_id, run_id));
            }
        }

        if let Some(outcome) = wait_preflight_outcome(wait_args.mode, &results) {
            return Ok(FleetWaitPreflight::Completed(AgentWaitOutput {
                outcome,
                results,
            }));
        }

        Ok(FleetWaitPreflight::Deferred(AgentWaitDirective {
            call_id,
            mode: wait_args.mode,
            timeout_ms: wait_args.timeout_ms,
            handles,
            results,
        }))
    }

    async fn wait_result_from_store(
        &self,
        target_session_id: &SessionId,
        run_id: RunId,
    ) -> Result<Option<AgentWaitHandleResult>, AgentApiError> {
        let entries = read_all_session_entries(
            self.sessions.as_ref(),
            target_session_id,
            MAX_EVENT_PAGE_LIMIT as usize,
        )
        .await?;
        let state = replay_core_agent_state(&entries)?;
        if let Some(record) = state
            .runs
            .completed
            .iter()
            .find(|record| record.run_id == run_id)
        {
            return Ok(Some(wait_terminal_result_core(
                target_session_id,
                run_id,
                record.status,
                record.output_ref.clone(),
                record
                    .failure
                    .as_ref()
                    .and_then(|failure| failure.message_ref.clone()),
            )));
        }
        if let Some(active) = state.runs.active.as_ref()
            && active.run_id == run_id
        {
            if core_run_status_is_terminal(active.status) {
                return Ok(Some(wait_terminal_result_core(
                    target_session_id,
                    run_id,
                    active.status,
                    active.output_ref.clone(),
                    active
                        .failure
                        .as_ref()
                        .and_then(|failure| failure.message_ref.clone()),
                )));
            }
            return Ok(Some(wait_pending_result(target_session_id, run_id)));
        }
        if state
            .runs
            .queued
            .iter()
            .any(|queued| queued.run_id == run_id)
        {
            return Ok(Some(wait_pending_result(target_session_id, run_id)));
        }
        Ok(None)
    }

    pub async fn list(
        &self,
        context: FleetInvocationContext,
        args: AgentListArgs,
    ) -> Result<AgentListOutput, AgentApiError> {
        let target_session_id = match args.target_session_id.as_deref() {
            Some(session_id) => parse_session_id(session_id, "target_session_id")?,
            None => context.parent_session_id,
        };
        self.load_session_required(&target_session_id).await?;
        let limit = bounded_list_limit(args.limit)?;
        let link_direction = match args.direction {
            AgentListDirection::Children => SessionLinkDirection::Outgoing,
            AgentListDirection::Parents => SessionLinkDirection::Incoming,
        };
        let links = self
            .sessions
            .list_links(ListSessionLinks {
                session_id: target_session_id.clone(),
                direction: link_direction,
                relationship: Some(FLEET_CHILD_RELATIONSHIP.to_owned()),
                limit,
            })
            .await
            .map_err(map_session_store_error)?;

        let mut agents = Vec::with_capacity(links.len());
        for link in links {
            let session_id = match args.direction {
                AgentListDirection::Children => link.to_session_id.clone(),
                AgentListDirection::Parents => link.from_session_id.clone(),
            };
            let record = self.load_session_required(&session_id).await?;
            let session = self.runtime.read_session(&session_id).await?;
            agents.push(AgentListItem {
                session_id: session_id.as_str().to_owned(),
                relationship: link.relationship,
                created_at_ms: link.created_at_ms,
                status: Some(api_status_name(&session.status)),
                active_run_id: active_run_id(&session),
                updated_at_ms: Some(record.updated_at_ms),
                lineage: lineage_view(&record),
            });
        }

        Ok(AgentListOutput {
            target_session_id: target_session_id.as_str().to_owned(),
            direction: args.direction,
            agents,
        })
    }

    pub async fn read(&self, args: AgentReadArgs) -> Result<AgentReadOutput, AgentApiError> {
        let target_session_id = parse_session_id(&args.target_session_id, "target_session_id")?;
        let record = self.load_session_required(&target_session_id).await?;
        let session = self.runtime.read_session(&target_session_id).await?;
        let environments = self
            .runtime
            .list_session_environments(&target_session_id)
            .await?;
        let links = self.direct_links(&target_session_id).await?;
        let recent_event_limit = recent_event_limit(args.recent_events.as_ref())?;
        let recent_transcript_limit = recent_transcript_limit(args.recent_transcript.as_ref())?;
        let recent_event_after = recent_after(&record, recent_event_limit);
        let recent_transcript_after = recent_after(&record, recent_transcript_limit);
        let recent_events = self
            .runtime
            .read_session_events(&target_session_id, recent_event_after, recent_event_limit)
            .await?;
        let recent_transcript = self
            .runtime
            .read_session_events(
                &target_session_id,
                recent_transcript_after,
                recent_transcript_limit,
            )
            .await?;

        Ok(AgentReadOutput {
            session_id: target_session_id.as_str().to_owned(),
            session: to_json_value(session)?,
            lineage: lineage_view(&record),
            links,
            environments: to_json_value(environments)?,
            recent_events: to_json_values(recent_events.events)?,
            recent_transcript: to_json_values(transcript_events(
                recent_transcript.events,
                args.recent_transcript.as_ref(),
            ))?,
        })
    }

    pub async fn cancel(&self, args: AgentCancelArgs) -> Result<AgentCancelOutput, AgentApiError> {
        let target_session_id = parse_session_id(&args.target_session_id, "target_session_id")?;
        self.load_session_required(&target_session_id).await?;

        match args.scope {
            AgentCancelScope::ActiveRun => {
                let session = self.runtime.read_session(&target_session_id).await?;
                let run_id = active_run_id(&session).ok_or_else(|| {
                    AgentApiError::rejected(format!(
                        "agent {} has no active run to cancel",
                        target_session_id
                    ))
                })?;
                let run = self.runtime.cancel_run(&target_session_id, &run_id).await?;
                Ok(AgentCancelOutput {
                    target_session_id: target_session_id.as_str().to_owned(),
                    scope: args.scope,
                    status: "cancelled".to_owned(),
                    run: Some(to_json_value(run)?),
                    session: None,
                })
            }
            AgentCancelScope::Session => {
                let session = self.runtime.close_session(&target_session_id).await?;
                Ok(AgentCancelOutput {
                    target_session_id: target_session_id.as_str().to_owned(),
                    scope: args.scope,
                    status: "closed".to_owned(),
                    run: None,
                    session: Some(to_json_value(session)?),
                })
            }
        }
    }

    async fn direct_links(
        &self,
        target_session_id: &SessionId,
    ) -> Result<Vec<AgentLinkView>, AgentApiError> {
        let mut links = Vec::new();
        for direction in [
            SessionLinkDirection::Outgoing,
            SessionLinkDirection::Incoming,
        ] {
            let records = self
                .sessions
                .list_links(ListSessionLinks {
                    session_id: target_session_id.clone(),
                    direction,
                    relationship: None,
                    limit: MAX_DIRECT_LINKS,
                })
                .await
                .map_err(map_session_store_error)?;
            links.extend(records.into_iter().map(link_view));
        }
        links.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.from_session_id.cmp(&right.from_session_id))
                .then_with(|| left.to_session_id.cmp(&right.to_session_id))
                .then_with(|| left.relationship.cmp(&right.relationship))
        });
        Ok(links)
    }

    async fn resolve_send_target(
        &self,
        context: &FleetInvocationContext,
        to: &AgentSendTarget,
    ) -> Result<Option<SessionId>, AgentApiError> {
        match to {
            AgentSendTarget::Session { target_session_id } => Ok(Some(parse_session_id(
                target_session_id,
                "target_session_id",
            )?)),
            AgentSendTarget::Parent => self.parent_session_id(&context.parent_session_id).await,
        }
    }

    async fn parent_session_id(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionId>, AgentApiError> {
        let mut links = self
            .sessions
            .list_links(ListSessionLinks {
                session_id: session_id.clone(),
                direction: SessionLinkDirection::Incoming,
                relationship: Some(FLEET_CHILD_RELATIONSHIP.to_owned()),
                limit: MAX_DIRECT_LINKS,
            })
            .await
            .map_err(map_session_store_error)?;
        links.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.from_session_id.cmp(&right.from_session_id))
                .then_with(|| left.to_session_id.cmp(&right.to_session_id))
        });
        Ok(links.into_iter().next().map(|link| link.from_session_id))
    }

    async fn has_session_link_edge(
        &self,
        left: &SessionId,
        right: &SessionId,
    ) -> Result<bool, AgentApiError> {
        if left == right {
            return Ok(false);
        }
        for direction in [
            SessionLinkDirection::Outgoing,
            SessionLinkDirection::Incoming,
        ] {
            let links = self
                .sessions
                .list_links(ListSessionLinks {
                    session_id: left.clone(),
                    direction,
                    relationship: None,
                    limit: MAX_DIRECT_LINKS,
                })
                .await
                .map_err(map_session_store_error)?;
            if links.into_iter().any(|link| {
                (link.from_session_id == *left && link.to_session_id == *right)
                    || (link.from_session_id == *right && link.to_session_id == *left)
            }) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn resolve_base_source(
        &self,
        context: &FleetInvocationContext,
        base: &AgentSpawnBase,
    ) -> Result<SessionId, AgentApiError> {
        match base {
            AgentSpawnBase::Self_ { .. } => Ok(context.parent_session_id.clone()),
            AgentSpawnBase::Session { session_id, .. } => {
                parse_session_id(session_id, "base.session_id")
            }
            AgentSpawnBase::Profile { .. } => Err(AgentApiError::invalid_request(
                "profile base does not resolve to a source session",
            )),
        }
    }

    async fn load_session_required(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionRecord, AgentApiError> {
        self.sessions
            .load_session(session_id)
            .await
            .map_err(map_session_store_error)?
            .ok_or_else(|| AgentApiError::not_found(format!("session not found: {session_id}")))
    }

    async fn create_or_reuse_child(
        &self,
        context: &FleetInvocationContext,
        source_record: &SessionRecord,
        child_session_id: &SessionId,
        source_seq: Option<EventSeq>,
        spawn_request_id: &str,
        child_id_was_derived: bool,
    ) -> Result<ChildCreateOutcome, AgentApiError> {
        let result = if let Some(source_seq) = source_seq {
            self.sessions
                .create_forked_session(CreateForkedSession {
                    source_session_id: source_record.session_id.clone(),
                    session_id: child_session_id.clone(),
                    source_seq,
                    created_at_ms: context.observed_at_ms,
                })
                .await
        } else {
            let entries = read_all_session_entries(
                self.sessions.as_ref(),
                &source_record.session_id,
                MAX_EVENT_PAGE_LIMIT as usize,
            )
            .await?;
            let state = replay_core_agent_state(&entries)?;
            let opening_events = core_agent_clone_opening_events(&state, context.observed_at_ms)
                .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
            self.sessions
                .create_cloned_session(CreateClonedSession {
                    source_session_id: source_record.session_id.clone(),
                    session_id: child_session_id.clone(),
                    created_at_ms: context.observed_at_ms,
                    opening_events,
                })
                .await
        };

        match result {
            Ok(_) => Ok(ChildCreateOutcome::Created),
            Err(SessionStoreError::SessionAlreadyExists { .. }) => {
                let existing = self
                    .validate_existing_child(
                        child_session_id,
                        &source_record.session_id,
                        source_seq,
                        spawn_request_id,
                        child_id_was_derived,
                    )
                    .await?;
                Ok(ChildCreateOutcome::Reused {
                    matching_spawn_link: existing.matching_spawn_link,
                })
            }
            Err(error) => Err(map_session_store_error(error)),
        }
    }

    async fn validate_existing_child(
        &self,
        child_session_id: &SessionId,
        source_session_id: &SessionId,
        source_seq: Option<EventSeq>,
        spawn_request_id: &str,
        child_id_was_derived: bool,
    ) -> Result<ExistingChildValidation, AgentApiError> {
        let existing = self.load_session_required(child_session_id).await?;
        if existing.source_session_id.as_ref() != Some(source_session_id) {
            return Err(AgentApiError::conflict(format!(
                "child session id {child_session_id} already exists with a different source"
            )));
        }
        if existing.source_seq != source_seq {
            return Err(AgentApiError::conflict(format!(
                "child session id {child_session_id} already exists with a different fork point"
            )));
        }
        let links = self
            .sessions
            .list_links(ListSessionLinks {
                session_id: child_session_id.clone(),
                direction: SessionLinkDirection::Incoming,
                relationship: Some(FLEET_CHILD_RELATIONSHIP.to_owned()),
                limit: 100,
            })
            .await
            .map_err(map_session_store_error)?;
        if links.is_empty() {
            if child_id_was_derived {
                return Ok(ExistingChildValidation {
                    matching_spawn_link: false,
                });
            }
            return Err(AgentApiError::conflict(format!(
                "child session id {child_session_id} already exists without matching fleet spawn metadata"
            )));
        }
        if links.iter().any(|link| {
            link.metadata
                .get("spawn_request_id")
                .and_then(Value::as_str)
                == Some(spawn_request_id)
        }) {
            return Ok(ExistingChildValidation {
                matching_spawn_link: true,
            });
        }
        Err(AgentApiError::conflict(format!(
            "child session id {child_session_id} is already linked to a different spawn request"
        )))
    }

    async fn upsert_spawn_link(
        &self,
        context: &FleetInvocationContext,
        source_session_id: Option<&SessionId>,
        child_session_id: &SessionId,
        source_seq: Option<EventSeq>,
        spawn_request_id: &str,
        args: &AgentSpawnArgs,
    ) -> Result<(), AgentApiError> {
        self.sessions
            .upsert_link(UpsertSessionLink {
                from_session_id: context.parent_session_id.clone(),
                to_session_id: child_session_id.clone(),
                relationship: FLEET_CHILD_RELATIONSHIP.to_owned(),
                created_at_ms: context.observed_at_ms,
                metadata: spawn_link_metadata(
                    context,
                    source_session_id,
                    source_seq,
                    spawn_request_id,
                    args,
                ),
            })
            .await
            .map_err(map_session_store_error)?;
        Ok(())
    }

    async fn apply_resource_policies(
        &self,
        child_session_id: &SessionId,
        observed_at_ms: u64,
        args: &AgentSpawnArgs,
    ) -> Result<(), AgentApiError> {
        if args.environment != EnvironmentPolicy::Share {
            return Err(AgentApiError::invalid_request(
                "agent_spawn environment policy must be share",
            ));
        }
        match args.vfs {
            VfsPolicy::Share => Ok(()),
            VfsPolicy::Isolate => {
                self.isolate_vfs_mounts(child_session_id, observed_at_ms)
                    .await
            }
        }
    }

    async fn isolate_vfs_mounts(
        &self,
        child_session_id: &SessionId,
        observed_at_ms: u64,
    ) -> Result<(), AgentApiError> {
        let workspace_store = self.workspace_store.as_ref().ok_or_else(|| {
            AgentApiError::internal("agent_spawn vfs isolate requires a workspace store")
        })?;
        let mount_store = self.mount_store.as_ref().ok_or_else(|| {
            AgentApiError::internal("agent_spawn vfs isolate requires a mount store")
        })?;
        let created_at_ms = nonnegative_i64(observed_at_ms, "observed_at_ms")?;
        let mounts = mount_store
            .list_mounts(child_session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        for mount in mounts {
            let VfsMountSource::Workspace { workspace_id } = mount.source.clone() else {
                continue;
            };
            let child_workspace_id = isolated_workspace_id(child_session_id, &mount.mount_path);
            if workspace_id == child_workspace_id {
                continue;
            }
            let source_workspace = workspace_store
                .read_workspace(&workspace_id)
                .await
                .map_err(map_vfs_catalog_error)?;
            match workspace_store
                .create_workspace(CreateVfsWorkspaceRecord {
                    workspace_id: child_workspace_id.clone(),
                    display_name: None,
                    base_snapshot_ref: Some(source_workspace.head_snapshot_ref.clone()),
                    head_snapshot_ref: source_workspace.head_snapshot_ref,
                    head_totals: source_workspace.head_totals,
                    created_at_ms,
                })
                .await
            {
                Ok(_) | Err(VfsCatalogError::AlreadyExists { .. }) => {}
                Err(error) => return Err(map_vfs_catalog_error(error)),
            }
            let mut isolated_mount = mount;
            isolated_mount.source = VfsMountSource::Workspace {
                workspace_id: child_workspace_id,
            };
            mount_store
                .put_mount(isolated_mount)
                .await
                .map_err(map_vfs_catalog_error)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChildCreateOutcome {
    Created,
    Reused { matching_spawn_link: bool },
}

impl ChildCreateOutcome {
    const fn has_matching_spawn_link(self) -> bool {
        matches!(
            self,
            ChildCreateOutcome::Reused {
                matching_spawn_link: true
            }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExistingChildValidation {
    matching_spawn_link: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FleetWaitPreflight {
    Completed(AgentWaitOutput),
    Deferred(AgentWaitDirective),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ValidatedWaitArgs {
    handles: Vec<AgentWaitHandle>,
    mode: WorkflowAgentWaitMode,
    timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetInvocationContext {
    pub parent_session_id: SessionId,
    pub parent_run_id: RunId,
    pub turn_id: TurnId,
    pub batch_id: ToolBatchId,
    pub call_id: ToolCallId,
    pub observed_at_ms: u64,
}

#[async_trait]
pub trait FleetChildRuntime: Send + Sync {
    async fn start_session(
        &self,
        session_id: &SessionId,
        close_on_terminal: bool,
        profile: Option<ProfileSource>,
    ) -> Result<(), AgentApiError>;

    async fn list_profiles(&self) -> Result<Vec<AgentProfileSummary>, AgentApiError>;

    async fn read_profile(&self, profile_id: ProfileId) -> Result<AgentProfile, AgentApiError>;

    async fn start_run(
        &self,
        session_id: &SessionId,
        input: Vec<InputItem>,
        submission_id: SubmissionId,
    ) -> Result<String, AgentApiError>;

    async fn enqueue_run(
        &self,
        session_id: &SessionId,
        input: Vec<InputItem>,
        submission_id: SubmissionId,
    ) -> Result<String, AgentApiError>;

    async fn read_session(&self, session_id: &SessionId) -> Result<SessionView, AgentApiError>;

    async fn read_session_events(
        &self,
        session_id: &SessionId,
        after: Option<u64>,
        limit: u32,
    ) -> Result<SessionEventsReadResponse, AgentApiError>;

    async fn list_session_environments(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionEnvironmentListResponse, AgentApiError>;

    async fn cancel_run(
        &self,
        session_id: &SessionId,
        run_id: &str,
    ) -> Result<api::RunView, AgentApiError>;

    async fn close_session(&self, session_id: &SessionId) -> Result<SessionView, AgentApiError>;
}

#[derive(Clone)]
pub struct AgentApiFleetRuntime {
    api: Arc<GatewayAgentApi>,
}

impl AgentApiFleetRuntime {
    pub fn new(api: Arc<GatewayAgentApi>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl FleetChildRuntime for AgentApiFleetRuntime {
    async fn start_session(
        &self,
        session_id: &SessionId,
        close_on_terminal: bool,
        profile: Option<ProfileSource>,
    ) -> Result<(), AgentApiError> {
        self.api
            .start_session_for_fleet_with_profile(session_id, close_on_terminal, profile)
            .await?;
        Ok(())
    }

    async fn list_profiles(&self) -> Result<Vec<AgentProfileSummary>, AgentApiError> {
        let response = self.api.list_profiles(ProfileListParams {}).await?;
        Ok(response.result.profiles)
    }

    async fn read_profile(&self, profile_id: ProfileId) -> Result<AgentProfile, AgentApiError> {
        let response = self
            .api
            .read_profile(ProfileReadParams { profile_id })
            .await?;
        Ok(response.result.profile)
    }

    async fn start_run(
        &self,
        session_id: &SessionId,
        input: Vec<InputItem>,
        submission_id: SubmissionId,
    ) -> Result<String, AgentApiError> {
        let response = self
            .api
            .start_run(RunStartParams {
                session_id: session_id.as_str().to_owned(),
                source: RunStartSource::Input { items: input },
                submission_id: Some(submission_id.as_str().to_owned()),
                config: None,
            })
            .await?;
        Ok(response.result.run.id)
    }

    async fn enqueue_run(
        &self,
        session_id: &SessionId,
        input: Vec<InputItem>,
        submission_id: SubmissionId,
    ) -> Result<String, AgentApiError> {
        self.api
            .enqueue_run_for_fleet(session_id, input, submission_id)
            .await
    }

    async fn read_session(&self, session_id: &SessionId) -> Result<SessionView, AgentApiError> {
        let response = self
            .api
            .read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
            })
            .await?;
        Ok(response.result.session)
    }

    async fn read_session_events(
        &self,
        session_id: &SessionId,
        after: Option<u64>,
        limit: u32,
    ) -> Result<SessionEventsReadResponse, AgentApiError> {
        let response = self
            .api
            .read_session_events(SessionEventsReadParams {
                session_id: session_id.as_str().to_owned(),
                after: after.map(|seq| EventCursor { seq }),
                limit: Some(limit),
                wait_ms: Some(0),
            })
            .await?;
        Ok(response.result)
    }

    async fn list_session_environments(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionEnvironmentListResponse, AgentApiError> {
        let response = self
            .api
            .list_session_environments(SessionEnvironmentListParams {
                session_id: session_id.as_str().to_owned(),
            })
            .await?;
        Ok(response.result)
    }

    async fn cancel_run(
        &self,
        session_id: &SessionId,
        run_id: &str,
    ) -> Result<api::RunView, AgentApiError> {
        let response = self
            .api
            .cancel_run(RunCancelParams {
                session_id: session_id.as_str().to_owned(),
                run_id: run_id.to_owned(),
            })
            .await?;
        Ok(response.result.run)
    }

    async fn close_session(&self, session_id: &SessionId) -> Result<SessionView, AgentApiError> {
        let response = self
            .api
            .close_session(SessionCloseParams {
                session_id: session_id.as_str().to_owned(),
            })
            .await?;
        Ok(response.result.session)
    }
}

#[derive(Clone)]
pub struct FleetToolExecutor {
    blobs: Arc<dyn BlobStore>,
    service: FleetService,
}

impl FleetToolExecutor {
    pub fn new(blobs: Arc<dyn BlobStore>, service: FleetService) -> Self {
        Self { blobs, service }
    }

    pub async fn invoke(
        &self,
        context: FleetInvocationContext,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        match call.tool_name.as_str() {
            AGENT_SPAWN_TOOL_NAME => self.invoke_spawn(context, call).await,
            AGENT_SEND_TOOL_NAME => self.invoke_send(context, call).await,
            AGENT_WAIT_TOOL_NAME => {
                fleet_failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    "agent_wait must be the only call in its tool batch",
                )
                .await
            }
            AGENT_LIST_TOOL_NAME => self.invoke_list(context, call).await,
            AGENT_READ_TOOL_NAME => self.invoke_read(call).await,
            AGENT_CANCEL_TOOL_NAME => self.invoke_cancel(call).await,
            PROFILE_LIST_TOOL_NAME => self.invoke_profile_list(call).await,
            PROFILE_READ_TOOL_NAME => self.invoke_profile_read(call).await,
            other => {
                fleet_failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    format!("unknown Fleet tool: {other}"),
                )
                .await
            }
        }
    }

    pub async fn invoke_wait_batch(
        &self,
        context: FleetInvocationContext,
        call: &ToolInvocationRequest,
    ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
        let args: AgentWaitArgs = self.decode_args(call).await?;
        match self
            .service
            .wait(context.clone(), call.call_id.clone(), args)
            .await
        {
            Ok(FleetWaitPreflight::Completed(output)) => {
                let output_ref = self
                    .blobs
                    .put_bytes(serde_json::to_vec(&output).map_err(io_error)?)
                    .await
                    .map_err(map_blob_error)?;
                let result = ToolInvocationResult {
                    call_id: call.call_id.clone(),
                    status: ToolCallStatus::Succeeded,
                    output_ref: Some(output_ref),
                    model_visible_context_entries: wait_model_visible_context_entries(
                        self.blobs.as_ref(),
                        &call.call_id,
                        &output,
                    )
                    .await?,
                    error_ref: None,
                    effects: Vec::new(),
                };
                Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                    run_id: context.parent_run_id,
                    turn_id: context.turn_id,
                    batch_id: context.batch_id,
                    results: vec![result],
                }))
            }
            Ok(FleetWaitPreflight::Deferred(directive)) => {
                let body = serde_json::to_value(directive)
                    .map_err(|error| io_error(format!("encode agent_wait directive: {error}")))?;
                Ok(ToolBatchOutcome::Deferred {
                    batch_id: context.batch_id,
                    resume_directive: ToolBatchResumeDirective::new(
                        FLEET_AGENT_WAIT_DIRECTIVE_KIND,
                        body,
                    ),
                })
            }
            Err(error) => {
                let result = fleet_failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    error.to_string(),
                )
                .await?;
                Ok(ToolBatchOutcome::completed(ToolInvocationBatchResult {
                    run_id: context.parent_run_id,
                    turn_id: context.turn_id,
                    batch_id: context.batch_id,
                    results: vec![result],
                }))
            }
        }
    }

    async fn invoke_spawn(
        &self,
        context: FleetInvocationContext,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let args: AgentSpawnArgs = self.decode_args(call).await?;
        match self.service.spawn(context, args).await {
            Ok(output) => {
                self.succeeded(
                    call.call_id.clone(),
                    &output,
                    spawn_model_visible_text(&output),
                )
                .await
            }
            Err(error) => {
                fleet_failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await
            }
        }
    }

    async fn invoke_send(
        &self,
        context: FleetInvocationContext,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let args: AgentSendArgs = self.decode_args(call).await?;
        match self.service.send(context, args).await {
            Ok(output) => {
                let visible = send_model_visible_text(&output);
                self.succeeded(call.call_id.clone(), &output, visible).await
            }
            Err(error) => {
                fleet_failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await
            }
        }
    }

    async fn invoke_list(
        &self,
        context: FleetInvocationContext,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let args: AgentListArgs = self.decode_args(call).await?;
        match self.service.list(context, args).await {
            Ok(output) => {
                let visible = list_model_visible_text(&output);
                self.succeeded(call.call_id.clone(), &output, visible).await
            }
            Err(error) => {
                fleet_failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await
            }
        }
    }

    async fn invoke_read(
        &self,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let args: AgentReadArgs = self.decode_args(call).await?;
        match self.service.read(args).await {
            Ok(output) => {
                let visible =
                    read_model_visible_context_entries(self.blobs.as_ref(), &call.call_id, &output)
                        .await?;
                self.succeeded_with_entries(call.call_id.clone(), &output, visible)
                    .await
            }
            Err(error) => {
                fleet_failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await
            }
        }
    }

    async fn invoke_cancel(
        &self,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let args: AgentCancelArgs = self.decode_args(call).await?;
        match self.service.cancel(args).await {
            Ok(output) => {
                let visible = cancel_model_visible_text(&output);
                self.succeeded(call.call_id.clone(), &output, visible).await
            }
            Err(error) => {
                fleet_failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await
            }
        }
    }

    async fn invoke_profile_list(
        &self,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let args: ProfileListArgs = self.decode_args(call).await?;
        match self.service.list_profiles(args).await {
            Ok(output) => {
                let visible = profile_list_model_visible_text(&output);
                self.succeeded(call.call_id.clone(), &output, visible).await
            }
            Err(error) => {
                fleet_failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await
            }
        }
    }

    async fn invoke_profile_read(
        &self,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let args: ProfileReadArgs = self.decode_args(call).await?;
        match self.service.read_profile(args).await {
            Ok(output) => {
                let visible = profile_read_model_visible_text(&output);
                self.succeeded(call.call_id.clone(), &output, visible).await
            }
            Err(error) => {
                fleet_failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await
            }
        }
    }

    async fn succeeded<T>(
        &self,
        call_id: ToolCallId,
        output: &T,
        visible: String,
    ) -> Result<ToolInvocationResult, CoreAgentIoError>
    where
        T: Serialize,
    {
        let output_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(output).map_err(io_error)?)
            .await
            .map_err(map_blob_error)?;
        let visible_ref = self
            .blobs
            .put_bytes(visible.into_bytes())
            .await
            .map_err(map_blob_error)?;
        let model_visible_context_entries = vec![ToolInvocationResult::tool_result_context_entry(
            &call_id,
            ToolCallStatus::Succeeded,
            visible_ref,
        )];
        Ok(ToolInvocationResult {
            call_id,
            status: ToolCallStatus::Succeeded,
            output_ref: Some(output_ref),
            model_visible_context_entries,
            error_ref: None,
            effects: Vec::new(),
        })
    }

    async fn succeeded_with_entries<T>(
        &self,
        call_id: ToolCallId,
        output: &T,
        model_visible_context_entries: Vec<ContextEntryInput>,
    ) -> Result<ToolInvocationResult, CoreAgentIoError>
    where
        T: Serialize,
    {
        let output_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(output).map_err(io_error)?)
            .await
            .map_err(map_blob_error)?;
        Ok(ToolInvocationResult {
            call_id,
            status: ToolCallStatus::Succeeded,
            output_ref: Some(output_ref),
            model_visible_context_entries,
            error_ref: None,
            effects: Vec::new(),
        })
    }

    async fn decode_args<T>(&self, call: &ToolInvocationRequest) -> Result<T, CoreAgentIoError>
    where
        T: serde::de::DeserializeOwned,
    {
        let bytes = self
            .blobs
            .read_bytes(&call.arguments_ref)
            .await
            .map_err(map_blob_error)?;
        serde_json::from_slice(&bytes)
            .map_err(|error| io_error(format!("invalid JSON tool arguments: {error}")))
    }
}

fn validate_spawn_args(args: &AgentSpawnArgs) -> Result<(), AgentApiError> {
    if args.input.trim().is_empty() {
        return Err(AgentApiError::invalid_request(
            "agent_spawn input must not be empty",
        ));
    }
    if args.base.profile().is_some() && args.vfs != VfsPolicy::Share {
        return Err(AgentApiError::invalid_request(
            "agent_spawn profile requires vfs=share",
        ));
    }
    if args.environment != EnvironmentPolicy::Share {
        return Err(AgentApiError::invalid_request(
            "agent_spawn environment policy must be share",
        ));
    }
    if args.lifecycle.close_on_terminal && !args.lifecycle.run_immediately {
        return Err(AgentApiError::invalid_request(
            "agent_spawn lifecycle.close_on_terminal requires lifecycle.run_immediately",
        ));
    }
    Ok(())
}

fn validate_send_args(args: &AgentSendArgs) -> Result<(), AgentApiError> {
    if args.text.trim().is_empty() {
        return Err(AgentApiError::invalid_request(
            "agent_send text must not be empty",
        ));
    }
    Ok(())
}

fn validate_wait_args(args: AgentWaitArgs) -> Result<ValidatedWaitArgs, AgentApiError> {
    if args.waits.is_empty() {
        return Err(AgentApiError::invalid_request(
            "agent_wait waits must contain at least one handle",
        ));
    }
    if args.waits.len() > MAX_AGENT_WAIT_HANDLES {
        return Err(AgentApiError::invalid_request(format!(
            "agent_wait waits must contain at most {MAX_AGENT_WAIT_HANDLES} handles"
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut handles = Vec::with_capacity(args.waits.len());
    for wait in args.waits {
        let target_session_id = parse_session_id(&wait.target_session_id, "target_session_id")?;
        let run_id = parse_api_run_id(&wait.run_id)?;
        let key = (target_session_id.clone(), run_id);
        if !seen.insert(key) {
            return Err(AgentApiError::invalid_request(format!(
                "duplicate agent_wait handle: target_session_id={} run_id={}",
                wait.target_session_id, wait.run_id
            )));
        }
        handles.push(AgentWaitHandle {
            target_session_id,
            run_id,
        });
    }
    Ok(ValidatedWaitArgs {
        handles,
        mode: match args.mode {
            ToolAgentWaitMode::All => WorkflowAgentWaitMode::All,
            ToolAgentWaitMode::Any => WorkflowAgentWaitMode::Any,
        },
        timeout_ms: args.timeout_ms,
    })
}

fn wait_preflight_outcome(
    mode: WorkflowAgentWaitMode,
    results: &[AgentWaitHandleResult],
) -> Option<AgentWaitOutcome> {
    match mode {
        WorkflowAgentWaitMode::All => {
            if results
                .iter()
                .any(|result| result.status == AgentWaitHandleStatus::Error)
            {
                Some(AgentWaitOutcome::Error)
            } else if results
                .iter()
                .all(|result| result.status == AgentWaitHandleStatus::Terminal)
            {
                Some(AgentWaitOutcome::Terminal)
            } else {
                None
            }
        }
        WorkflowAgentWaitMode::Any => {
            if results
                .iter()
                .any(|result| result.status == AgentWaitHandleStatus::Terminal)
            {
                Some(AgentWaitOutcome::Terminal)
            } else if results
                .iter()
                .all(|result| result.status == AgentWaitHandleStatus::Error)
            {
                Some(AgentWaitOutcome::Error)
            } else {
                None
            }
        }
    }
}

fn wait_pending_result(target_session_id: &SessionId, run_id: RunId) -> AgentWaitHandleResult {
    AgentWaitHandleResult {
        target_session_id: target_session_id.as_str().to_owned(),
        run_id: api_run_id(run_id),
        status: AgentWaitHandleStatus::Pending,
        run: None,
        error: None,
    }
}

fn wait_terminal_result(
    target_session_id: &SessionId,
    run_id: RunId,
    status: ApiRunStatus,
) -> AgentWaitHandleResult {
    AgentWaitHandleResult {
        target_session_id: target_session_id.as_str().to_owned(),
        run_id: api_run_id(run_id),
        status: AgentWaitHandleStatus::Terminal,
        run: Some(AgentWaitRunResult {
            status: api_status_name(&status),
            output_ref: None,
            failure_message_ref: None,
        }),
        error: None,
    }
}

fn wait_terminal_result_core(
    target_session_id: &SessionId,
    run_id: RunId,
    status: engine::RunStatus,
    output_ref: Option<BlobRef>,
    failure_message_ref: Option<BlobRef>,
) -> AgentWaitHandleResult {
    AgentWaitHandleResult {
        target_session_id: target_session_id.as_str().to_owned(),
        run_id: api_run_id(run_id),
        status: AgentWaitHandleStatus::Terminal,
        run: Some(AgentWaitRunResult {
            status: core_run_status_name(status).to_owned(),
            output_ref,
            failure_message_ref,
        }),
        error: None,
    }
}

fn wait_error_result(
    target_session_id: &SessionId,
    run_id: RunId,
    error: impl Into<String>,
) -> AgentWaitHandleResult {
    AgentWaitHandleResult {
        target_session_id: target_session_id.as_str().to_owned(),
        run_id: api_run_id(run_id),
        status: AgentWaitHandleStatus::Error,
        run: None,
        error: Some(error.into()),
    }
}

fn bounded_list_limit(limit: Option<u32>) -> Result<usize, AgentApiError> {
    let limit = limit.unwrap_or(DEFAULT_AGENT_LIST_LIMIT as u32);
    if limit == 0 {
        return Err(AgentApiError::invalid_request(
            "agent_list limit must be at least 1",
        ));
    }
    Ok((limit as usize).min(MAX_AGENT_LIST_LIMIT))
}

fn recent_event_limit(
    selector: Option<&tools::fleet::RecentEventsSelector>,
) -> Result<u32, AgentApiError> {
    let limit = selector.map_or(DEFAULT_RECENT_EVENT_LIMIT, |selector| selector.limit);
    bounded_recent_limit(limit, "recent_events.limit")
}

fn recent_transcript_limit(
    selector: Option<&tools::fleet::RecentTranscriptSelector>,
) -> Result<u32, AgentApiError> {
    let Some(selector) = selector else {
        return Ok(DEFAULT_RECENT_TRANSCRIPT_EVENT_LIMIT);
    };
    let limit = match (selector.events, selector.turns) {
        (Some(events), _) => events,
        (None, Some(_)) => MAX_RECENT_EVENT_LIMIT,
        (None, None) => DEFAULT_RECENT_TRANSCRIPT_EVENT_LIMIT,
    };
    bounded_recent_limit(limit, "recent_transcript")
}

fn bounded_recent_limit(limit: u32, field: &str) -> Result<u32, AgentApiError> {
    if limit == 0 {
        return Err(AgentApiError::invalid_request(format!(
            "{field} must be at least 1"
        )));
    }
    Ok(limit.min(MAX_RECENT_EVENT_LIMIT))
}

fn recent_after(record: &SessionRecord, limit: u32) -> Option<u64> {
    let head_seq = record.head.as_ref()?.seq.as_u64();
    let after = head_seq.saturating_sub(limit as u64);
    (after > 0).then_some(after)
}

fn active_run_id(session: &SessionView) -> Option<String> {
    session
        .runs
        .iter()
        .find(|run| matches!(run.status, ApiRunStatus::Running))
        .map(|run| run.id.clone())
}

fn lineage_view(record: &SessionRecord) -> AgentLineageView {
    AgentLineageView {
        source_session_id: record
            .source_session_id
            .as_ref()
            .map(|session_id| session_id.as_str().to_owned()),
        source_seq: record.source_seq.map(EventSeq::as_u64),
    }
}

fn link_view(record: engine::storage::SessionLinkRecord) -> AgentLinkView {
    AgentLinkView {
        from_session_id: record.from_session_id.as_str().to_owned(),
        to_session_id: record.to_session_id.as_str().to_owned(),
        relationship: record.relationship,
        created_at_ms: record.created_at_ms,
        metadata: record.metadata,
    }
}

fn api_status_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn to_json_value<T: Serialize>(value: T) -> Result<Value, AgentApiError> {
    serde_json::to_value(value)
        .map_err(|error| AgentApiError::internal(format!("failed to encode Fleet output: {error}")))
}

fn to_json_values<T: Serialize>(values: Vec<T>) -> Result<Vec<Value>, AgentApiError> {
    values.into_iter().map(to_json_value).collect()
}

fn transcript_events(
    events: Vec<api::SessionEventView>,
    selector: Option<&tools::fleet::RecentTranscriptSelector>,
) -> Vec<api::SessionEventView> {
    let Some(turns) = selector.and_then(|selector| selector.turns) else {
        return events;
    };
    if selector.and_then(|selector| selector.events).is_some() {
        return events;
    }
    let mut selected_turn_ids = Vec::new();
    for event in events.iter().rev() {
        let Some(turn_id) = event.joins.turn_id.as_deref() else {
            continue;
        };
        if selected_turn_ids.iter().any(|selected| selected == turn_id) {
            continue;
        }
        selected_turn_ids.push(turn_id.to_owned());
        if selected_turn_ids.len() >= turns as usize {
            break;
        }
    }
    if selected_turn_ids.is_empty() {
        return events;
    }
    events
        .into_iter()
        .filter(|event| {
            event
                .joins
                .turn_id
                .as_ref()
                .is_some_and(|turn_id| selected_turn_ids.contains(turn_id))
        })
        .collect()
}

fn parse_session_id(value: &str, field: &str) -> Result<SessionId, AgentApiError> {
    SessionId::try_new(value.to_owned())
        .map_err(|error| AgentApiError::invalid_request(format!("invalid {field}: {error}")))
}

fn parse_api_run_id(value: &str) -> Result<RunId, AgentApiError> {
    let raw = value.strip_prefix("run_").unwrap_or(value);
    let numeric = raw.parse::<u64>().map_err(|_| {
        AgentApiError::invalid_request(format!(
            "invalid run_id: expected run_<number>, got {value}"
        ))
    })?;
    Ok(RunId::new(numeric))
}

fn api_run_id(run_id: RunId) -> String {
    format!("run_{}", run_id.as_u64())
}

fn api_run_status_is_terminal(status: ApiRunStatus) -> bool {
    matches!(
        status,
        ApiRunStatus::Completed | ApiRunStatus::Failed | ApiRunStatus::Cancelled
    )
}

fn core_run_status_is_terminal(status: engine::RunStatus) -> bool {
    matches!(
        status,
        engine::RunStatus::Completed | engine::RunStatus::Failed | engine::RunStatus::Cancelled
    )
}

fn core_run_status_name(status: engine::RunStatus) -> &'static str {
    match status {
        engine::RunStatus::Active => "running",
        engine::RunStatus::Cancelling => "cancelling",
        engine::RunStatus::Completed => "completed",
        engine::RunStatus::Failed => "failed",
        engine::RunStatus::Cancelled => "cancelled",
    }
}

fn derived_child_session_id(context: &FleetInvocationContext) -> SessionId {
    let digest = digest_suffix(&spawn_request_material(context));
    SessionId::new(format!("agent_{digest}"))
}

fn spawn_request_id(context: &FleetInvocationContext) -> String {
    format!(
        "fleet_spawn_{}",
        digest_suffix(&spawn_request_material(context))
    )
}

fn child_run_submission_id(context: &FleetInvocationContext) -> SubmissionId {
    SubmissionId::new(format!(
        "fleet_run_{}",
        digest_suffix(&spawn_request_material(context))
    ))
}

fn send_submission_id(
    context: &FleetInvocationContext,
    target_session_id: &SessionId,
) -> SubmissionId {
    SubmissionId::new(format!(
        "fleet_send_{}",
        digest_suffix(&format!(
            "{}:{}",
            spawn_request_material(context),
            target_session_id
        ))
    ))
}

fn spawn_request_material(context: &FleetInvocationContext) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        context.parent_session_id,
        context.parent_run_id,
        context.turn_id,
        context.batch_id,
        context.call_id
    )
}

fn digest_suffix(value: &str) -> String {
    let digest = BlobRef::from_bytes(value.as_bytes());
    digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(digest.as_str())
        .chars()
        .take(32)
        .collect()
}

fn isolated_workspace_id(child_session_id: &SessionId, mount_path: &VfsPath) -> VfsWorkspaceId {
    let digest = digest_suffix(&format!("{child_session_id}:{}", mount_path.as_str()));
    VfsWorkspaceId::new(format!("workspace_{digest}"))
}

fn spawn_run_text(context: &FleetInvocationContext, args: &AgentSpawnArgs) -> String {
    match args.report_back.as_ref() {
        Some(report_back) => format!(
            "{}\n\n{}",
            args.input.trim(),
            parent_report_back_instruction(&context.parent_session_id, report_back)
        ),
        None => args.input.clone(),
    }
}

fn send_run_input(
    context: &FleetInvocationContext,
    args: &AgentSendArgs,
) -> Result<Vec<InputItem>, AgentApiError> {
    let envelope = fleet_send_envelope_text(context, args)?;
    let mut input = Vec::with_capacity(args.input.len() + 1);
    input.push(InputItem::Text { text: envelope });
    input.extend(
        args.input
            .iter()
            .map(send_input_item_to_api)
            .collect::<Vec<_>>(),
    );
    Ok(input)
}

fn send_input_item_to_api(item: &AgentSendInputItem) -> InputItem {
    match item {
        AgentSendInputItem::Text { text } => InputItem::Text { text: text.clone() },
        AgentSendInputItem::TextRef { blob_ref } => InputItem::TextRef {
            blob_ref: blob_ref.clone(),
        },
        AgentSendInputItem::Media {
            blob_ref,
            mime,
            kind,
            name,
        } => InputItem::Media {
            blob_ref: blob_ref.clone(),
            mime: mime.clone(),
            kind: send_media_kind_to_api(*kind),
            name: name.clone(),
        },
    }
}

fn send_media_kind_to_api(kind: AgentSendMediaKind) -> MediaKind {
    match kind {
        AgentSendMediaKind::Image => MediaKind::Image,
        AgentSendMediaKind::Audio => MediaKind::Audio,
        AgentSendMediaKind::Document => MediaKind::Document,
    }
}

fn fleet_send_envelope_text(
    context: &FleetInvocationContext,
    args: &AgentSendArgs,
) -> Result<String, AgentApiError> {
    let text = match args.report_back.as_ref() {
        Some(report_back) => format!(
            "{}\n\n{}",
            args.text.trim(),
            session_report_back_instruction(&context.parent_session_id, report_back)
        ),
        None => args.text.clone(),
    };
    let mut fleet_send = serde_json::Map::new();
    fleet_send.insert(
        "from_session_id".to_owned(),
        Value::String(context.parent_session_id.as_str().to_owned()),
    );
    if let Some(payload) = args.payload.clone() {
        fleet_send.insert("payload".to_owned(), payload);
    }
    serde_json::to_string(&json!({
        "fleet_send": Value::Object(fleet_send),
        "text": text,
    }))
    .map_err(|error| {
        AgentApiError::internal(format!("failed to encode Fleet send envelope: {error}"))
    })
}

fn parent_report_back_instruction(
    parent_session_id: &SessionId,
    report_back: &AgentReportBack,
) -> String {
    format!(
        "Report back to parent session {parent_session_id} with agent_send {{ \"to\": {{ \"kind\": \"parent\" }}, \"text\": \"<concise result>\" }} when you finish, and earlier if you have meaningful progress or hit a blocking question.{}",
        extra_report_back_instructions(report_back)
    )
}

fn session_report_back_instruction(
    sender_session_id: &SessionId,
    report_back: &AgentReportBack,
) -> String {
    format!(
        "Report back to session {sender_session_id} with agent_send {{ \"to\": {{ \"kind\": \"session\", \"target_session_id\": \"{sender_session_id}\" }}, \"text\": \"<concise result>\" }} when you finish, and earlier if you have meaningful progress or hit a blocking question.{}",
        extra_report_back_instructions(report_back)
    )
}

fn extra_report_back_instructions(report_back: &AgentReportBack) -> String {
    report_back
        .instructions
        .as_deref()
        .map(str::trim)
        .filter(|instructions| !instructions.is_empty())
        .map(|instructions| format!(" Extra instructions: {instructions}"))
        .unwrap_or_default()
}

fn spawn_link_metadata(
    context: &FleetInvocationContext,
    source_session_id: Option<&SessionId>,
    source_seq: Option<EventSeq>,
    spawn_request_id: &str,
    args: &AgentSpawnArgs,
) -> Value {
    json!({
        "kind": "fleet_spawn",
        "spawn_request_id": spawn_request_id,
        "parent_run_id": context.parent_run_id.as_u64(),
        "turn_id": context.turn_id.as_u64(),
        "tool_batch_id": context.batch_id.as_u64(),
        "tool_call_id": context.call_id.as_str(),
        "source_session_id": source_session_id.map(SessionId::as_str),
        "source_seq": source_seq.map(EventSeq::as_u64),
        "base": &args.base,
        "profile": args.base.profile(),
        "fork": args.base.fork().is_some(),
        "fork_at_seq": args.base.fork().and_then(|fork| match fork {
            AgentSpawnFork::Safe => None,
            AgentSpawnFork::AtSeq { seq } => Some(*seq),
        }),
        "vfs": match args.vfs {
            VfsPolicy::Share => "share",
            VfsPolicy::Isolate => "isolate",
        },
        "environment": "share",
    })
}

fn map_session_store_error(error: SessionStoreError) -> AgentApiError {
    match error {
        SessionStoreError::SessionAlreadyExists { session_id } => {
            AgentApiError::conflict(format!("session already exists: {session_id}"))
        }
        SessionStoreError::SessionNotFound { session_id } => {
            AgentApiError::not_found(format!("session not found: {session_id}"))
        }
        SessionStoreError::ExpectedHeadMismatch { .. } => {
            AgentApiError::conflict(error.to_string())
        }
        SessionStoreError::InvalidForkPoint { .. }
        | SessionStoreError::InvalidRelationship { .. }
        | SessionStoreError::InvalidLimit { .. } => {
            AgentApiError::invalid_request(error.to_string())
        }
        SessionStoreError::Store { .. } => AgentApiError::internal(error.to_string()),
    }
}

fn map_vfs_catalog_error(error: VfsCatalogError) -> AgentApiError {
    match error {
        VfsCatalogError::AlreadyExists { .. } | VfsCatalogError::RevisionConflict { .. } => {
            AgentApiError::conflict(error.to_string())
        }
        VfsCatalogError::NotFound { .. } => AgentApiError::not_found(error.to_string()),
        VfsCatalogError::InvalidInput { .. } => AgentApiError::invalid_request(error.to_string()),
        VfsCatalogError::Store { .. } => AgentApiError::internal(error.to_string()),
    }
}

fn nonnegative_i64(value: u64, name: &str) -> Result<i64, AgentApiError> {
    i64::try_from(value)
        .map_err(|_| AgentApiError::invalid_request(format!("{name} is too large: {value}")))
}

fn spawn_model_visible_text(output: &AgentSpawnOutput) -> String {
    match output.child_run_id.as_deref() {
        Some(run_id) => format!(
            "Agent {} {} and started run {}.",
            output.child_session_id, output.status, run_id
        ),
        None => format!(
            "Agent {} {} without starting a run.",
            output.child_session_id, output.status
        ),
    }
}

fn send_model_visible_text(output: &AgentSendOutput) -> String {
    match (
        output.status,
        output.target_session_id.as_deref(),
        output.run_id.as_deref(),
    ) {
        (AgentSendStatus::Delivered, Some(target_session_id), Some(run_id)) => {
            match output.submission_id.as_deref() {
                Some(submission_id) => format!(
                    "Delivered message to session {target_session_id} as queued run {run_id} (submission {submission_id})."
                ),
                None => {
                    format!(
                        "Delivered message to session {target_session_id} as queued run {run_id}."
                    )
                }
            }
        }
        (AgentSendStatus::NotReachable, Some(target_session_id), _) => {
            format!("Session {target_session_id} is not reachable.")
        }
        (AgentSendStatus::NotReachable, None, _) => "No reachable target session found.".to_owned(),
        _ => "Fleet send did not produce a run.".to_owned(),
    }
}

async fn wait_model_visible_context_entries(
    blobs: &dyn BlobStore,
    call_id: &ToolCallId,
    output: &AgentWaitOutput,
) -> Result<Vec<ContextEntryInput>, CoreAgentIoError> {
    let summary_ref = blobs
        .put_bytes(wait_model_visible_summary(output).into_bytes())
        .await
        .map_err(map_blob_error)?;
    let mut entries = vec![ToolInvocationResult::tool_result_context_entry(
        call_id,
        ToolCallStatus::Succeeded,
        summary_ref,
    )];
    for result in output
        .results
        .iter()
        .filter(|result| result.status == AgentWaitHandleStatus::Terminal)
    {
        append_wait_terminal_visible_context_entries(blobs, &mut entries, result).await?;
    }
    Ok(entries)
}

fn wait_model_visible_summary(output: &AgentWaitOutput) -> String {
    let terminal = output
        .results
        .iter()
        .filter(|result| result.status == AgentWaitHandleStatus::Terminal)
        .count();
    let pending = output
        .results
        .iter()
        .filter(|result| result.status == AgentWaitHandleStatus::Pending)
        .count();
    let errors = output
        .results
        .iter()
        .filter(|result| result.status == AgentWaitHandleStatus::Error)
        .count();
    format!(
        "agent_wait resolved with outcome {} (terminal: {terminal}, pending: {pending}, errors: {errors}).",
        wait_outcome_name(output.outcome)
    )
}

async fn append_wait_terminal_visible_context_entries(
    blobs: &dyn BlobStore,
    entries: &mut Vec<ContextEntryInput>,
    result: &AgentWaitHandleResult,
) -> Result<(), CoreAgentIoError> {
    let Some(run) = result.run.as_ref() else {
        let mut text = wait_result_header(result, "terminal", "none", 0);
        text.push_str("\n\nNo run details were recorded.");
        append_wait_text_message(
            blobs,
            entries,
            text,
            Some(wait_result_bundle_preview(result, "no run details")),
        )
        .await?;
        return Ok(());
    };

    if let Some(output_ref) = run.output_ref.as_ref() {
        append_wait_result_content(
            blobs,
            entries,
            result,
            run,
            "final_output",
            "Final output",
            output_ref,
        )
        .await?;
    } else if let Some(failure_ref) = run.failure_message_ref.as_ref() {
        append_wait_result_content(
            blobs,
            entries,
            result,
            run,
            "failure_message",
            "Failure message",
            failure_ref,
        )
        .await?;
    } else {
        let mut text = wait_result_header(result, &run.status, "none", 0);
        text.push_str("\n\nNo final output was recorded.");
        append_wait_text_message(
            blobs,
            entries,
            text,
            Some(wait_result_bundle_preview(result, "empty result")),
        )
        .await?;
    }
    Ok(())
}

async fn append_wait_result_content(
    blobs: &dyn BlobStore,
    entries: &mut Vec<ContextEntryInput>,
    result: &AgentWaitHandleResult,
    run: &AgentWaitRunResult,
    item_kind: &str,
    section_label: &str,
    content_ref: &BlobRef,
) -> Result<(), CoreAgentIoError> {
    match blobs.read_text(content_ref).await {
        Ok(content) => {
            let mut text = wait_result_header(result, &run.status, item_kind, 1);
            text.push_str("\n\n");
            text.push_str(section_label);
            text.push_str(":\n");
            text.push_str(&content);
            append_wait_text_message(
                blobs,
                entries,
                text,
                Some(wait_result_bundle_preview(result, item_kind)),
            )
            .await?;
        }
        Err(_) => {
            let mut text = wait_result_header(result, &run.status, item_kind, 1);
            text.push_str("\n\nThe next context item belongs to this subagent result. ");
            text.push_str(
                "It is preserved as its original blob because it could not be copied into a text wrapper.",
            );
            append_wait_text_message(
                blobs,
                entries,
                text,
                Some(wait_result_bundle_preview(result, item_kind)),
            )
            .await?;
            entries.push(wait_user_message(
                content_ref.clone(),
                Some(&wait_result_item_preview(result, item_kind, 1, 1)),
            ));
        }
    }
    Ok(())
}

fn wait_result_header(
    result: &AgentWaitHandleResult,
    status: &str,
    item_kind: &str,
    item_count: usize,
) -> String {
    let mut text = format!(
        "Subagent result\ntarget_session_id: {}\nrun_id: {}\nstatus: {}\nresult_item_count: {}",
        result.target_session_id, result.run_id, status, item_count
    );
    if item_count > 0 {
        text.push_str("\nresult_item_1_kind: ");
        text.push_str(item_kind);
    }
    text
}

fn wait_result_bundle_preview(result: &AgentWaitHandleResult, item_kind: &str) -> String {
    format!(
        "Subagent result: {item_kind} from {} {}",
        result.target_session_id, result.run_id
    )
}

fn wait_result_item_preview(
    result: &AgentWaitHandleResult,
    item_kind: &str,
    index: usize,
    count: usize,
) -> String {
    format!(
        "Subagent result item {index}/{count}: {item_kind} from {} {}",
        result.target_session_id, result.run_id
    )
}

async fn append_wait_text_message(
    blobs: &dyn BlobStore,
    entries: &mut Vec<ContextEntryInput>,
    text: impl Into<String>,
    preview: Option<String>,
) -> Result<(), CoreAgentIoError> {
    let text = text.into();
    let content_ref = blobs
        .put_bytes(text.into_bytes())
        .await
        .map_err(map_blob_error)?;
    entries.push(wait_user_message(content_ref, preview.as_deref()));
    Ok(())
}

fn wait_user_message(content_ref: BlobRef, preview: Option<&str>) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content_ref,
        media_type: None,
        preview: preview.map(|value| value.chars().take(160).collect()),
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }
}

fn wait_outcome_name(outcome: AgentWaitOutcome) -> &'static str {
    match outcome {
        AgentWaitOutcome::Terminal => "terminal",
        AgentWaitOutcome::Timeout => "timeout",
        AgentWaitOutcome::Error => "error",
    }
}

fn list_model_visible_text(output: &AgentListOutput) -> String {
    format!(
        "Found {} {} agent(s) for {}.",
        output.agents.len(),
        match output.direction {
            AgentListDirection::Children => "child",
            AgentListDirection::Parents => "parent",
        },
        output.target_session_id
    )
}

fn read_model_visible_text(output: &AgentReadOutput) -> String {
    let status = output
        .session
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!(
        "Read agent {}: status {}, {} link(s), {} recent event(s).",
        output.session_id,
        status,
        output.links.len(),
        output.recent_events.len()
    )
}

async fn read_model_visible_context_entries(
    blobs: &dyn BlobStore,
    call_id: &ToolCallId,
    output: &AgentReadOutput,
) -> Result<Vec<ContextEntryInput>, CoreAgentIoError> {
    let summary_ref = blobs
        .put_bytes(read_model_visible_text(output).into_bytes())
        .await
        .map_err(map_blob_error)?;
    let mut entries = vec![ToolInvocationResult::tool_result_context_entry(
        call_id,
        ToolCallStatus::Succeeded,
        summary_ref,
    )];
    if let Some(transcript) = agent_read_visible_run_transcripts(output) {
        append_wait_text_message(
            blobs,
            &mut entries,
            transcript,
            Some(format!("Agent run transcript from {}", output.session_id)),
        )
        .await?;
    }
    Ok(entries)
}

fn agent_read_visible_run_transcripts(output: &AgentReadOutput) -> Option<String> {
    let runs = output.session.get("runs")?.as_array()?;
    let mut sections = Vec::new();
    let mut remaining = MAX_AGENT_READ_VISIBLE_CHARS;
    for run in runs.iter().rev() {
        if sections.len() >= MAX_AGENT_READ_VISIBLE_RUNS || remaining == 0 {
            break;
        }
        let Some(section) = agent_read_visible_run_section(&output.session_id, run) else {
            continue;
        };
        let section = truncate_chars(&section, remaining);
        remaining = remaining.saturating_sub(section.chars().count());
        sections.push(section);
    }
    (!sections.is_empty()).then(|| sections.join("\n\n---\n\n"))
}

fn agent_read_visible_run_section(session_id: &str, run: &Value) -> Option<String> {
    let run_id = run.get("id").and_then(Value::as_str).unwrap_or("unknown");
    let status = run
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let items = run.get("items").and_then(Value::as_array)?;
    let mut lines = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("assistantMessage") => {
                if let Some(text) = item.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    lines.push(format!("Assistant message:\n{}", text.trim()));
                }
            }
            Some("toolResult") => {
                if let Some(output) = item.get("output").and_then(Value::as_str)
                    && !output.trim().is_empty()
                {
                    lines.push(format!("Tool result:\n{}", output.trim()));
                }
            }
            _ => {}
        }
    }
    if lines.is_empty() {
        return None;
    }
    let mut text = format!(
        "Agent run transcript\ntarget_session_id: {session_id}\nrun_id: {run_id}\nstatus: {status}"
    );
    text.push_str("\n\n");
    text.push_str(&lines.join("\n\n"));
    Some(text)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    const TRUNCATED: &str = "\n[truncated]";
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    if max_chars <= TRUNCATED.chars().count() {
        return value.chars().take(max_chars).collect();
    }
    let keep = max_chars - TRUNCATED.chars().count();
    let mut truncated = value.chars().take(keep).collect::<String>();
    truncated.push_str(TRUNCATED);
    truncated
}

fn cancel_model_visible_text(output: &AgentCancelOutput) -> String {
    match output.scope {
        AgentCancelScope::ActiveRun => format!(
            "Agent {} active run cancellation status: {}.",
            output.target_session_id, output.status
        ),
        AgentCancelScope::Session => {
            format!(
                "Agent {} session status: {}.",
                output.target_session_id, output.status
            )
        }
    }
}

fn profile_list_model_visible_text(output: &ProfileListOutput) -> String {
    if output.profiles.is_empty() {
        return "No agent profiles are available.".to_owned();
    }
    let mut lines = Vec::with_capacity(output.profiles.len() + 1);
    lines.push(format!("Found {} agent profile(s).", output.profiles.len()));
    for profile in &output.profiles {
        let mut line = format!(
            "- {} (revision {}, updated_at_ms {})",
            profile.profile_id, profile.revision, profile.updated_at_ms
        );
        if let Some(display_name) = profile
            .display_name
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            line.push_str(": ");
            line.push_str(display_name);
        }
        if let Some(description) = profile
            .description
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            line.push_str(" - ");
            line.push_str(description);
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn profile_read_model_visible_text(output: &ProfileReadOutput) -> String {
    let profile = &output.profile;
    format!(
        "Read profile {} revision {}: config {}, instructions {}, {} mount(s), {} MCP link(s), {} environment(s).",
        profile.profile_id,
        profile.revision,
        yes_no(profile.document.config.is_some()),
        yes_no(profile.document.instructions.is_some()),
        profile.document.mounts.len(),
        profile.document.mcp.len(),
        profile.document.environments.len()
    )
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

async fn fleet_failed_result(
    blobs: &dyn BlobStore,
    call_id: ToolCallId,
    message: impl Into<String>,
) -> Result<ToolInvocationResult, CoreAgentIoError> {
    let error_ref = blobs
        .put_bytes(message.into().into_bytes())
        .await
        .map_err(map_blob_error)?;
    let model_visible_context_entries = vec![ToolInvocationResult::tool_result_context_entry(
        &call_id,
        ToolCallStatus::Failed,
        error_ref.clone(),
    )];
    Ok(ToolInvocationResult {
        call_id,
        status: ToolCallStatus::Failed,
        output_ref: None,
        model_visible_context_entries,
        error_ref: Some(error_ref),
        effects: Vec::new(),
    })
}

fn map_blob_error(error: BlobStoreError) -> CoreAgentIoError {
    io_error(format!("Fleet tool blob operation failed: {error}"))
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use async_trait::async_trait;
    use engine::{
        ContextConfig, ModelSelection, ProviderApiKind, RunConfig, SessionConfig, ToolBatchOutcome,
        ToolCallId, ToolConfig, ToolInvocationRequest, ToolName, TurnConfig,
        storage::{CreateSession, InMemorySessionStore, SessionStore},
    };
    use vfs::{CompareAndSetVfsWorkspaceHead, VfsMountAccess, VfsMountRecord, VfsWorkspaceRecord};

    use super::*;

    fn visible_tool_result_ref(result: &ToolInvocationResult) -> BlobRef {
        result
            .model_visible_context_entries
            .iter()
            .find_map(|entry| {
                matches!(entry.kind, ContextEntryKind::ToolResult { .. })
                    .then(|| entry.content_ref.clone())
            })
            .expect("visible tool result")
    }

    #[derive(Default)]
    struct FakeRuntime {
        session_store: Option<Arc<InMemorySessionStore>>,
        started_sessions: Mutex<Vec<(SessionId, bool, Option<ProfileSource>)>>,
        started_runs: Mutex<Vec<(SessionId, Vec<InputItem>, SubmissionId)>>,
        sessions: Mutex<BTreeMap<SessionId, SessionView>>,
        events: Mutex<BTreeMap<SessionId, Vec<api::SessionEventView>>>,
        environments: Mutex<BTreeMap<SessionId, SessionEnvironmentListResponse>>,
        profiles: Mutex<BTreeMap<ProfileId, AgentProfile>>,
        cancelled_runs: Mutex<Vec<(SessionId, String)>>,
        closed_sessions: Mutex<Vec<SessionId>>,
    }

    #[async_trait]
    impl FleetChildRuntime for FakeRuntime {
        async fn start_session(
            &self,
            session_id: &SessionId,
            close_on_terminal: bool,
            profile: Option<ProfileSource>,
        ) -> Result<(), AgentApiError> {
            if let Some(store) = &self.session_store
                && store
                    .load_session(session_id)
                    .await
                    .map_err(map_session_store_error)?
                    .is_none()
            {
                store
                    .create_session(CreateSession {
                        session_id: session_id.clone(),
                        created_at_ms: 1,
                    })
                    .await
                    .map_err(map_session_store_error)?;
            }
            self.started_sessions.lock().expect("lock").push((
                session_id.clone(),
                close_on_terminal,
                profile,
            ));
            Ok(())
        }

        async fn list_profiles(&self) -> Result<Vec<AgentProfileSummary>, AgentApiError> {
            Ok(self
                .profiles
                .lock()
                .expect("lock")
                .values()
                .map(AgentProfile::summary)
                .collect())
        }

        async fn read_profile(&self, profile_id: ProfileId) -> Result<AgentProfile, AgentApiError> {
            self.profiles
                .lock()
                .expect("lock")
                .get(&profile_id)
                .cloned()
                .ok_or_else(|| {
                    AgentApiError::not_found(format!("agent profile not found: {profile_id}"))
                })
        }

        async fn start_run(
            &self,
            session_id: &SessionId,
            input: Vec<InputItem>,
            submission_id: SubmissionId,
        ) -> Result<String, AgentApiError> {
            self.started_runs.lock().expect("lock").push((
                session_id.clone(),
                input,
                submission_id,
            ));
            Ok("run_1".to_owned())
        }

        async fn enqueue_run(
            &self,
            session_id: &SessionId,
            input: Vec<InputItem>,
            submission_id: SubmissionId,
        ) -> Result<String, AgentApiError> {
            self.started_runs.lock().expect("lock").push((
                session_id.clone(),
                input,
                submission_id,
            ));
            Ok("run_1".to_owned())
        }

        async fn read_session(&self, session_id: &SessionId) -> Result<SessionView, AgentApiError> {
            Ok(self
                .sessions
                .lock()
                .expect("lock")
                .get(session_id)
                .cloned()
                .unwrap_or_else(|| {
                    api_session_view(session_id, api::SessionStatus::Idle, Vec::new())
                }))
        }

        async fn read_session_events(
            &self,
            session_id: &SessionId,
            after: Option<u64>,
            limit: u32,
        ) -> Result<SessionEventsReadResponse, AgentApiError> {
            let all_events = self
                .events
                .lock()
                .expect("lock")
                .get(session_id)
                .cloned()
                .unwrap_or_default();
            let events: Vec<_> = all_events
                .iter()
                .filter(|event| after.is_none_or(|after| event.cursor.seq > after))
                .take(limit as usize)
                .cloned()
                .collect();
            Ok(SessionEventsReadResponse {
                next_cursor: events.last().map(|event| event.cursor),
                head_cursor: all_events.last().map(|event| event.cursor),
                events,
                complete: true,
                gap: None,
            })
        }

        async fn list_session_environments(
            &self,
            session_id: &SessionId,
        ) -> Result<SessionEnvironmentListResponse, AgentApiError> {
            Ok(self
                .environments
                .lock()
                .expect("lock")
                .get(session_id)
                .cloned()
                .unwrap_or_else(|| SessionEnvironmentListResponse {
                    active_env_id: None,
                    environments: Vec::new(),
                }))
        }

        async fn cancel_run(
            &self,
            session_id: &SessionId,
            run_id: &str,
        ) -> Result<api::RunView, AgentApiError> {
            self.cancelled_runs
                .lock()
                .expect("lock")
                .push((session_id.clone(), run_id.to_owned()));
            Ok(api_run_view(run_id, ApiRunStatus::Cancelled))
        }

        async fn close_session(
            &self,
            session_id: &SessionId,
        ) -> Result<SessionView, AgentApiError> {
            self.closed_sessions
                .lock()
                .expect("lock")
                .push(session_id.clone());
            Ok(api_session_view(
                session_id,
                api::SessionStatus::Closed,
                Vec::new(),
            ))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_clone_self_creates_child_link_and_starts_run() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let source = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                session_id: source.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create source");
        let opening_events =
            core_agent_clone_opening_events(&open_state(), 2).expect("opening events");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: source.clone(),
                expected_head: None,
                events: opening_events,
            })
            .await
            .expect("append open");

        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions.clone(), runtime.clone());
        let output = service
            .spawn(context(source.clone()), spawn_args("summarize"))
            .await
            .expect("spawn");

        let child = SessionId::new(output.child_session_id);
        let child_record = sessions
            .load_session(&child)
            .await
            .expect("load")
            .expect("child");
        assert_eq!(child_record.source_session_id, Some(source.clone()));
        assert_eq!(child_record.source_seq, None);

        let links = sessions
            .list_links(ListSessionLinks {
                session_id: source,
                direction: SessionLinkDirection::Outgoing,
                relationship: Some(FLEET_CHILD_RELATIONSHIP.to_owned()),
                limit: 10,
            })
            .await
            .expect("links");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].to_session_id, child);

        assert_eq!(
            runtime.started_sessions.lock().expect("lock").as_slice(),
            &[(links[0].to_session_id.clone(), false, None)]
        );
        assert_eq!(output.child_run_id.as_deref(), Some("run_1"));
        assert_eq!(output.status, "created");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_close_on_terminal_is_passed_to_child_runtime() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let source = open_source_session(&sessions).await;
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime.clone());

        service
            .spawn(
                context(source),
                serde_json::from_value(json!({
                    "input": "one-off task",
                    "lifecycle": {
                        "close_on_terminal": true
                    }
                }))
                .expect("spawn args"),
            )
            .await
            .expect("spawn");

        let started = runtime.started_sessions.lock().expect("lock");
        assert_eq!(started.len(), 1);
        assert!(started[0].1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_safe_fork_records_runtime_source_seq() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let source = open_source_session(sessions.as_ref()).await;
        let expected_seq = sessions
            .safe_fork_seq(&source)
            .await
            .expect("safe fork seq");
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions.clone(), runtime);

        let output = service
            .spawn(
                context(source.clone()),
                serde_json::from_value(json!({
                    "input": "fork work",
                    "base": {
                        "kind": "self",
                        "fork": { "kind": "safe" }
                    }
                }))
                .expect("spawn args"),
            )
            .await
            .expect("spawn");

        let child = SessionId::new(output.child_session_id);
        let child_record = sessions
            .load_session(&child)
            .await
            .expect("load")
            .expect("child");
        assert_eq!(child_record.source_session_id, Some(source));
        assert_eq!(child_record.source_seq, Some(expected_seq));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_profile_only_creates_fresh_child_without_source_lineage() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let profile = ProfileSource::Named {
            profile_id: api::ProfileId::new("support"),
        };
        let runtime = Arc::new(FakeRuntime {
            session_store: Some(sessions.clone()),
            ..FakeRuntime::default()
        });
        let service = FleetService::new(sessions.clone(), runtime.clone());

        let output = service
            .spawn(
                context(parent.clone()),
                serde_json::from_value(json!({
                    "input": "support this customer",
                    "base": {
                        "kind": "profile",
                        "profile": {
                            "kind": "named",
                            "profileId": "support"
                        }
                    }
                }))
                .expect("spawn args"),
            )
            .await
            .expect("spawn");

        let child = SessionId::new(output.child_session_id);
        let child_record = sessions
            .load_session(&child)
            .await
            .expect("load")
            .expect("child");
        assert_eq!(child_record.source_session_id, None);
        assert_eq!(child_record.source_seq, None);

        assert_eq!(
            runtime.started_sessions.lock().expect("lock").as_slice(),
            &[(child.clone(), false, Some(profile.clone()))]
        );
        assert_eq!(output.child_run_id.as_deref(), Some("run_1"));
        assert_eq!(output.status, "created");

        let links = sessions
            .list_links(ListSessionLinks {
                session_id: parent,
                direction: SessionLinkDirection::Outgoing,
                relationship: Some(FLEET_CHILD_RELATIONSHIP.to_owned()),
                limit: 10,
            })
            .await
            .expect("links");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].to_session_id, child);
        assert_eq!(
            links[0].metadata["profile"],
            serde_json::to_value(profile).expect("profile json")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_profile_base_rejects_vfs_isolate() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime);

        let error = service
            .spawn(
                context(parent),
                serde_json::from_value(json!({
                    "input": "bad profile resource policy",
                    "base": {
                        "kind": "profile",
                        "profile": {
                            "kind": "named",
                            "profileId": "support"
                        }
                    },
                    "vfs": "isolate"
                }))
                .expect("spawn args"),
            )
            .await
            .expect_err("profile + vfs isolate must reject");

        assert_eq!(error.kind, api::AgentApiErrorKind::InvalidRequest);
        assert!(error.message.contains("profile requires vfs=share"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_retry_reuses_existing_child() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let source = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                session_id: source.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create source");
        let opening_events =
            core_agent_clone_opening_events(&open_state(), 2).expect("opening events");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: source.clone(),
                expected_head: None,
                events: opening_events,
            })
            .await
            .expect("append open");

        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime);
        let first = service
            .spawn(context(source.clone()), spawn_args("do work"))
            .await
            .expect("first spawn");
        let second = service
            .spawn(context(source), spawn_args("do work"))
            .await
            .expect("retry spawn");

        assert_eq!(first.child_session_id, second.child_session_id);
        assert_eq!(second.status, "reused");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_child_id_collision_without_spawn_metadata_conflicts() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let source = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                session_id: source.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create source");
        let opening_events =
            core_agent_clone_opening_events(&open_state(), 2).expect("opening events");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: source.clone(),
                expected_head: None,
                events: opening_events.clone(),
            })
            .await
            .expect("append open");
        let child = SessionId::new("explicit_child");
        sessions
            .create_cloned_session(CreateClonedSession {
                source_session_id: source.clone(),
                session_id: child,
                created_at_ms: 3,
                opening_events,
            })
            .await
            .expect("preexisting clone");

        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime);
        let error = service
            .spawn(
                context(source),
                spawn_args_with_child("do work", "explicit_child"),
            )
            .await
            .expect_err("explicit collision must be rejected");

        assert_eq!(error.kind, api::AgentApiErrorKind::Conflict);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_isolate_rewrites_workspace_mounts_and_keeps_snapshots() {
        let vfs = Arc::new(TestVfsCatalog::default());
        let child = SessionId::new("child");
        let source_workspace = VfsWorkspaceId::new("workspace_source");
        let head = BlobRef::from_bytes(b"snapshot-head");
        vfs.create_workspace(CreateVfsWorkspaceRecord {
            workspace_id: source_workspace.clone(),
            display_name: None,
            base_snapshot_ref: None,
            head_snapshot_ref: head.clone(),
            head_totals: ::vfs::VfsTotals::default(),
            created_at_ms: 1,
        })
        .await
        .expect("source workspace");
        vfs.put_mount(VfsMountRecord {
            session_id: child.clone(),
            mount_path: VfsPath::parse("/workspace").expect("path"),
            source: VfsMountSource::Workspace {
                workspace_id: source_workspace.clone(),
            },
            access: VfsMountAccess::ReadWrite,
        })
        .await
        .expect("workspace mount");
        let snapshot_ref = BlobRef::from_bytes(b"snapshot-mount");
        vfs.put_mount(VfsMountRecord {
            session_id: child.clone(),
            mount_path: VfsPath::parse("/readonly").expect("path"),
            source: VfsMountSource::Snapshot {
                snapshot_ref: snapshot_ref.clone(),
            },
            access: VfsMountAccess::ReadOnly,
        })
        .await
        .expect("snapshot mount");

        let service = FleetService::new(
            Arc::new(InMemorySessionStore::new()),
            Arc::new(FakeRuntime::default()),
        )
        .with_vfs_stores(vfs.clone(), vfs.clone());
        service
            .apply_resource_policies(
                &child,
                10,
                &serde_json::from_value(json!({
                    "input": "do work",
                    "vfs": "isolate"
                }))
                .expect("args"),
            )
            .await
            .expect("isolate");

        let mounts = vfs.list_mounts(&child).await.expect("mounts");
        let workspace_mount = mounts
            .iter()
            .find(|mount| mount.mount_path.as_str() == "/workspace")
            .expect("workspace mount");
        let VfsMountSource::Workspace { workspace_id } = &workspace_mount.source else {
            panic!("workspace mount source");
        };
        assert_ne!(workspace_id, &source_workspace);
        let child_workspace = vfs
            .read_workspace(workspace_id)
            .await
            .expect("child workspace");
        assert_eq!(child_workspace.base_snapshot_ref, Some(head.clone()));
        assert_eq!(child_workspace.head_snapshot_ref, head);
        let snapshot_mount = mounts
            .iter()
            .find(|mount| mount.mount_path.as_str() == "/readonly")
            .expect("snapshot mount");
        assert_eq!(
            snapshot_mount.source,
            VfsMountSource::Snapshot { snapshot_ref }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_executor_runs_spawn_and_writes_output_blobs() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let source = open_source_session(sessions.as_ref()).await;
        let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime);
        let executor = FleetToolExecutor::new(blobs.clone(), service);
        let arguments_ref = blobs
            .put_bytes(br#"{"input":"do work"}"#.to_vec())
            .await
            .expect("args");
        let result = executor
            .invoke(
                context(source),
                &ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new(AGENT_SPAWN_TOOL_NAME),
                    arguments_ref,
                    execution_target: None,
                },
            )
            .await
            .expect("invoke");

        assert_eq!(result.status, ToolCallStatus::Succeeded);
        let output_ref = result.output_ref.as_ref().expect("output");
        let output: AgentSpawnOutput =
            serde_json::from_slice(&blobs.read_bytes(output_ref).await.expect("read output"))
                .expect("decode output");
        assert!(output.child_session_id.starts_with("agent_"));
        let visible_ref = visible_tool_result_ref(&result);
        let visible = blobs.read_text(&visible_ref).await.expect("read visible");
        assert!(visible.contains("started run"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_to_linked_child_delivers_envelope_with_deterministic_submission_id() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let child = create_linked_child(sessions.as_ref(), &parent).await;
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime.clone());

        let output = service
            .send(
                context(parent.clone()),
                serde_json::from_value(json!({
                    "to": { "kind": "session", "target_session_id": child.as_str() },
                    "text": "do more work",
                    "payload": { "answer": 42 },
                    "input": [
                        { "type": "text", "text": "trailing context" }
                    ]
                }))
                .expect("send args"),
            )
            .await
            .expect("send");

        assert_eq!(output.target_session_id.as_deref(), Some(child.as_str()));
        assert_eq!(output.run_id.as_deref(), Some("run_1"));
        assert!(
            output
                .submission_id
                .as_deref()
                .is_some_and(|submission_id| submission_id.starts_with("fleet_send_"))
        );
        assert_eq!(output.status, AgentSendStatus::Delivered);
        assert_eq!(
            runtime.started_sessions.lock().expect("lock").as_slice(),
            &[(child.clone(), false, None)]
        );
        let started_runs = runtime.started_runs.lock().expect("lock");
        assert_eq!(started_runs.len(), 1);
        assert_eq!(started_runs[0].0, child);
        assert_eq!(started_runs[0].1.len(), 2);
        let envelope = text_item_json(&started_runs[0].1[0]);
        assert_eq!(envelope["fleet_send"]["from_session_id"], "parent");
        assert!(envelope["fleet_send"].get("kind").is_none());
        assert_eq!(envelope["fleet_send"]["payload"], json!({ "answer": 42 }));
        assert_eq!(envelope["text"], "do more work");
        assert_eq!(
            started_runs[0].1[1],
            InputItem::Text {
                text: "trailing context".to_owned()
            }
        );
        assert!(
            started_runs[0].2.as_str().starts_with("fleet_send_"),
            "submission id should be Fleet-derived, got {}",
            started_runs[0].2
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_to_parent_resolves_incoming_spawn_link() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let child = create_linked_child(sessions.as_ref(), &parent).await;
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime.clone());

        let output = service
            .send(
                context(child),
                serde_json::from_value(json!({
                    "to": { "kind": "parent" },
                    "text": "done"
                }))
                .expect("send args"),
            )
            .await
            .expect("send");

        assert_eq!(output.target_session_id.as_deref(), Some(parent.as_str()));
        assert_eq!(output.run_id.as_deref(), Some("run_1"));
        assert!(
            output
                .submission_id
                .as_deref()
                .is_some_and(|submission_id| submission_id.starts_with("fleet_send_"))
        );
        let started_runs = runtime.started_runs.lock().expect("lock");
        assert_eq!(started_runs[0].0, parent);
        let envelope = text_item_json(&started_runs[0].1[0]);
        assert!(envelope["fleet_send"].get("kind").is_none());
        assert_eq!(envelope["text"], "done");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_without_link_returns_not_reachable() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime.clone());

        let output = service
            .send(
                context(parent),
                serde_json::from_value(json!({
                    "to": { "kind": "session", "target_session_id": "other" },
                    "text": "hello"
                }))
                .expect("send args"),
            )
            .await
            .expect("send");

        assert_eq!(output.target_session_id.as_deref(), Some("other"));
        assert_eq!(output.run_id, None);
        assert_eq!(output.status, AgentSendStatus::NotReachable);
        assert!(runtime.started_runs.lock().expect("lock").is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_to_parent_from_root_returns_not_reachable() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime.clone());

        let output = service
            .send(
                context(parent),
                serde_json::from_value(json!({
                    "to": { "kind": "parent" },
                    "text": "hello"
                }))
                .expect("send args"),
            )
            .await
            .expect("send");

        assert_eq!(output.target_session_id, None);
        assert_eq!(output.status, AgentSendStatus::NotReachable);
        assert!(runtime.started_runs.lock().expect("lock").is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_report_back_injects_instruction_without_config_patch() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions.clone(), runtime.clone());

        let output = service
            .spawn(
                context(parent.clone()),
                serde_json::from_value(json!({
                    "input": "do work",
                    "report_back": {
                        "instructions": "Include the final count."
                    }
                }))
                .expect("spawn args"),
            )
            .await
            .expect("spawn");

        let child = SessionId::new(output.child_session_id);
        let child_record = sessions
            .load_session(&child)
            .await
            .expect("load")
            .expect("child");
        assert_eq!(child_record.source_session_id, Some(parent));
        let started_runs = runtime.started_runs.lock().expect("lock");
        let text = text_item(&started_runs[0].1[0]);
        assert!(text.contains("do work"));
        assert!(text.contains("agent_send"));
        assert!(text.contains("\"kind\": \"parent\""));
        assert!(!text.contains("\"kind\": \"result\""));
        assert!(text.contains("Include the final count."));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_returns_linked_children_with_compact_status() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let child = SessionId::new("child");
        sessions
            .create_cloned_session(CreateClonedSession {
                source_session_id: parent.clone(),
                session_id: child.clone(),
                created_at_ms: 20,
                opening_events: Vec::new(),
            })
            .await
            .expect("child");
        sessions
            .upsert_link(UpsertSessionLink {
                from_session_id: parent.clone(),
                to_session_id: child.clone(),
                relationship: FLEET_CHILD_RELATIONSHIP.to_owned(),
                created_at_ms: 21,
                metadata: json!({ "kind": "fleet_spawn" }),
            })
            .await
            .expect("link");
        let runtime = Arc::new(FakeRuntime::default());
        runtime.sessions.lock().expect("lock").insert(
            child.clone(),
            api_session_view(
                &child,
                api::SessionStatus::Active,
                vec![api_run_view("run_7", ApiRunStatus::Running)],
            ),
        );
        let service = FleetService::new(sessions, runtime);

        let output = service
            .list(
                context(parent.clone()),
                AgentListArgs {
                    target_session_id: None,
                    direction: AgentListDirection::Children,
                    limit: Some(10),
                },
            )
            .await
            .expect("list");

        assert_eq!(output.target_session_id, "parent");
        assert_eq!(output.agents.len(), 1);
        let agent = &output.agents[0];
        assert_eq!(agent.session_id, "child");
        assert_eq!(agent.relationship, FLEET_CHILD_RELATIONSHIP);
        assert_eq!(agent.status.as_deref(), Some("active"));
        assert_eq!(agent.active_run_id.as_deref(), Some("run_7"));
        assert_eq!(
            agent.lineage.source_session_id.as_deref(),
            Some(parent.as_str())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_returns_session_lineage_resources_links_and_recent_activity() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let child = SessionId::new("child");
        sessions
            .create_cloned_session(CreateClonedSession {
                source_session_id: parent.clone(),
                session_id: child.clone(),
                created_at_ms: 20,
                opening_events: Vec::new(),
            })
            .await
            .expect("child");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: child.clone(),
                expected_head: None,
                events: vec![
                    stored_test_event(30, "lightspeed.test.1"),
                    stored_test_event(31, "lightspeed.test.2"),
                    stored_test_event(32, "lightspeed.test.3"),
                ],
            })
            .await
            .expect("append child events");
        sessions
            .upsert_link(UpsertSessionLink {
                from_session_id: parent,
                to_session_id: child.clone(),
                relationship: FLEET_CHILD_RELATIONSHIP.to_owned(),
                created_at_ms: 21,
                metadata: json!({ "kind": "fleet_spawn" }),
            })
            .await
            .expect("link");
        let runtime = Arc::new(FakeRuntime::default());
        runtime.sessions.lock().expect("lock").insert(
            child.clone(),
            api_session_view(&child, api::SessionStatus::Idle, Vec::new()),
        );
        runtime.events.lock().expect("lock").insert(
            child.clone(),
            vec![
                api_event(&child, 1),
                api_event(&child, 2),
                api_event(&child, 3),
            ],
        );
        runtime.environments.lock().expect("lock").insert(
            child.clone(),
            SessionEnvironmentListResponse {
                active_env_id: Some("env_1".to_owned()),
                environments: Vec::new(),
            },
        );
        let service = FleetService::new(sessions, runtime);

        let output = service
            .read(AgentReadArgs {
                target_session_id: child.as_str().to_owned(),
                recent_transcript: Some(tools::fleet::RecentTranscriptSelector {
                    turns: Some(1),
                    events: None,
                }),
                recent_events: Some(tools::fleet::RecentEventsSelector { limit: 2 }),
            })
            .await
            .expect("read");

        assert_eq!(output.session_id, "child");
        assert_eq!(output.session["id"], "child");
        assert_eq!(output.session["config"]["tools"]["fleet"], true);
        assert_eq!(output.lineage.source_session_id.as_deref(), Some("parent"));
        assert_eq!(output.links.len(), 1);
        assert_eq!(output.environments["activeEnvId"], "env_1");
        assert_eq!(output.recent_events.len(), 2);
        assert_eq!(output.recent_events[0]["cursor"]["seq"], 2);
        assert_eq!(output.recent_transcript.len(), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_active_run_uses_runtime_active_run() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let child = open_source_session(sessions.as_ref()).await;
        let runtime = Arc::new(FakeRuntime::default());
        runtime.sessions.lock().expect("lock").insert(
            child.clone(),
            api_session_view(
                &child,
                api::SessionStatus::Active,
                vec![api_run_view("run_3", ApiRunStatus::Running)],
            ),
        );
        let service = FleetService::new(sessions, runtime.clone());

        let output = service
            .cancel(AgentCancelArgs {
                target_session_id: child.as_str().to_owned(),
                scope: AgentCancelScope::ActiveRun,
                reason: Some("test".to_owned()),
            })
            .await
            .expect("cancel");

        assert_eq!(output.status, "cancelled");
        assert_eq!(output.run.as_ref().expect("run")["id"], "run_3");
        assert_eq!(
            runtime.cancelled_runs.lock().expect("lock").as_slice(),
            &[(child, "run_3".to_owned())]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_session_uses_runtime_close() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let child = open_source_session(sessions.as_ref()).await;
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime.clone());

        let output = service
            .cancel(AgentCancelArgs {
                target_session_id: child.as_str().to_owned(),
                scope: AgentCancelScope::Session,
                reason: None,
            })
            .await
            .expect("close");

        assert_eq!(output.status, "closed");
        assert_eq!(
            output.session.as_ref().expect("session")["status"],
            "closed"
        );
        assert_eq!(
            runtime.closed_sessions.lock().expect("lock").as_slice(),
            &[child]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_preflight_completed_run_returns_inline_output() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let child = create_linked_child(sessions.as_ref(), &parent).await;
        let runtime = Arc::new(FakeRuntime::default());
        runtime.sessions.lock().expect("lock").insert(
            child.clone(),
            api_session_view(
                &child,
                api::SessionStatus::Idle,
                vec![api_run_view("run_2", ApiRunStatus::Completed)],
            ),
        );
        let service = FleetService::new(sessions, runtime);

        let output = match service
            .wait(
                context(parent),
                ToolCallId::new("call_wait"),
                serde_json::from_value(json!({
                    "waits": [{ "target_session_id": child.as_str(), "run_id": "run_2" }],
                    "mode": "all"
                }))
                .expect("wait args"),
            )
            .await
            .expect("wait")
        {
            FleetWaitPreflight::Completed(output) => output,
            FleetWaitPreflight::Deferred(_) => panic!("completed run should not defer"),
        };

        assert_eq!(output.outcome, AgentWaitOutcome::Terminal);
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].status, AgentWaitHandleStatus::Terminal);
        assert_eq!(
            output.results[0]
                .run
                .as_ref()
                .map(|run| run.status.as_str()),
            Some("completed")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_preflight_running_run_returns_deferred_directive() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let child = create_linked_child(sessions.as_ref(), &parent).await;
        let runtime = Arc::new(FakeRuntime::default());
        runtime.sessions.lock().expect("lock").insert(
            child.clone(),
            api_session_view(
                &child,
                api::SessionStatus::Active,
                vec![api_run_view("run_2", ApiRunStatus::Running)],
            ),
        );
        let service = FleetService::new(sessions, runtime);

        let directive = match service
            .wait(
                context(parent),
                ToolCallId::new("call_wait"),
                serde_json::from_value(json!({
                    "waits": [{ "target_session_id": child.as_str(), "run_id": "run_2" }],
                    "mode": "all",
                    "timeout_ms": 5000
                }))
                .expect("wait args"),
            )
            .await
            .expect("wait")
        {
            FleetWaitPreflight::Completed(_) => panic!("running run should defer"),
            FleetWaitPreflight::Deferred(directive) => directive,
        };

        assert_eq!(directive.call_id, ToolCallId::new("call_wait"));
        assert_eq!(directive.mode, WorkflowAgentWaitMode::All);
        assert_eq!(directive.timeout_ms, Some(5000));
        assert_eq!(directive.handles.len(), 1);
        assert_eq!(directive.handles[0].target_session_id, child);
        assert_eq!(directive.handles[0].run_id, RunId::new(2));
        assert_eq!(directive.results[0].status, AgentWaitHandleStatus::Pending);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_executor_lone_wait_can_return_deferred_batch() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let child = create_linked_child(sessions.as_ref(), &parent).await;
        let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
        let runtime = Arc::new(FakeRuntime::default());
        runtime.sessions.lock().expect("lock").insert(
            child.clone(),
            api_session_view(
                &child,
                api::SessionStatus::Active,
                vec![api_run_view("run_2", ApiRunStatus::Running)],
            ),
        );
        let service = FleetService::new(sessions, runtime);
        let executor = FleetToolExecutor::new(blobs.clone(), service);
        let arguments_ref = blobs
            .put_bytes(
                format!(
                    r#"{{"waits":[{{"target_session_id":"{}","run_id":"run_2"}}]}}"#,
                    child.as_str()
                )
                .into_bytes(),
            )
            .await
            .expect("args");

        let outcome = executor
            .invoke_wait_batch(
                context(parent),
                &ToolInvocationRequest {
                    call_id: ToolCallId::new("call_wait"),
                    tool_name: ToolName::new(AGENT_WAIT_TOOL_NAME),
                    arguments_ref,
                    execution_target: None,
                },
            )
            .await
            .expect("invoke");

        let ToolBatchOutcome::Deferred {
            batch_id,
            resume_directive,
        } = outcome
        else {
            panic!("wait should defer");
        };
        assert_eq!(batch_id, ToolBatchId::new(1));
        assert_eq!(resume_directive.api_kind, FLEET_AGENT_WAIT_DIRECTIVE_KIND);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_executor_lone_wait_includes_child_final_output() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let child = create_linked_child(sessions.as_ref(), &parent).await;
        let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
        append_completed_run_with_output(
            sessions.as_ref(),
            blobs.as_ref(),
            &child,
            RunId::new(1),
            "full child answer\nwith every line",
        )
        .await;
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime.clone());
        let executor = FleetToolExecutor::new(blobs.clone(), service);
        let arguments_ref = blobs
            .put_bytes(
                format!(
                    r#"{{"waits":[{{"target_session_id":"{}","run_id":"run_1"}}]}}"#,
                    child.as_str()
                )
                .into_bytes(),
            )
            .await
            .expect("args");

        let outcome = executor
            .invoke_wait_batch(
                context(parent),
                &ToolInvocationRequest {
                    call_id: ToolCallId::new("call_wait"),
                    tool_name: ToolName::new(AGENT_WAIT_TOOL_NAME),
                    arguments_ref,
                    execution_target: None,
                },
            )
            .await
            .expect("invoke");

        let ToolBatchOutcome::Completed { result } = outcome else {
            panic!("wait should complete from stored terminal run");
        };
        if result.results[0].status != ToolCallStatus::Succeeded {
            let error = match result.results[0].error_ref.as_ref() {
                Some(error_ref) => Some(blobs.read_text(error_ref).await.expect("read error")),
                None => None,
            };
            panic!("wait failed: {error:?}");
        }
        let output_ref = result.results[0].output_ref.as_ref().unwrap_or_else(|| {
            panic!(
                "missing output: error_ref={:?}",
                result.results[0].error_ref.as_ref()
            )
        });
        let output: AgentWaitOutput =
            serde_json::from_slice(&blobs.read_bytes(output_ref).await.expect("read output"))
                .expect("decode output");
        if output.outcome != AgentWaitOutcome::Terminal {
            panic!("unexpected wait output: {output:?}");
        }
        assert_eq!(
            output.results[0]
                .run
                .as_ref()
                .and_then(|run| run.output_ref.as_ref())
                .map(BlobRef::as_str),
            Some(BlobRef::from_bytes(b"full child answer\nwith every line").as_str())
        );
        let wait_result = &result.results[0];
        let summary_ref = visible_tool_result_ref(wait_result);
        let summary = blobs.read_text(&summary_ref).await.expect("read summary");
        assert!(summary.contains("agent_wait resolved with outcome terminal"));
        assert!(!summary.contains("full child answer"));
        let visible_messages = wait_result
            .model_visible_context_entries
            .iter()
            .filter(|entry| matches!(entry.kind, ContextEntryKind::Message { .. }))
            .collect::<Vec<_>>();
        assert_eq!(visible_messages.len(), 1);
        let wrapped_output = visible_messages[0];
        assert!(
            wrapped_output
                .preview
                .as_deref()
                .is_some_and(|text| text.contains("Subagent result: final_output from child run_1"))
        );
        let wrapped_output = blobs
            .read_text(&wrapped_output.content_ref)
            .await
            .expect("read wrapped output");
        assert!(wrapped_output.contains("Subagent result"));
        assert!(wrapped_output.contains("target_session_id: child"));
        assert!(wrapped_output.contains("run_id: run_1"));
        assert!(wrapped_output.contains("status: completed"));
        assert!(wrapped_output.contains("result_item_count: 1"));
        assert!(wrapped_output.contains("result_item_1_kind: final_output"));
        assert!(wrapped_output.contains("Final output:\nfull child answer\nwith every line"));
        let child_output_ref = BlobRef::from_bytes(b"full child answer\nwith every line");
        assert!(
            !visible_messages
                .iter()
                .any(|entry| entry.content_ref == child_output_ref),
            "text child output should be wrapped with subagent identity, not injected raw"
        );
        assert!(runtime.started_sessions.lock().expect("lock").is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_context_preserves_non_text_result_as_bundle_item() {
        let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
        let binary_ref = blobs
            .put_bytes(vec![0xff, 0xfe, 0xfd])
            .await
            .expect("binary output");
        let output = AgentWaitOutput {
            outcome: AgentWaitOutcome::Terminal,
            results: vec![AgentWaitHandleResult {
                target_session_id: "child".to_owned(),
                run_id: "run_1".to_owned(),
                status: AgentWaitHandleStatus::Terminal,
                run: Some(AgentWaitRunResult {
                    status: "completed".to_owned(),
                    output_ref: Some(binary_ref.clone()),
                    failure_message_ref: None,
                }),
                error: None,
            }],
        };

        let entries = wait_model_visible_context_entries(
            blobs.as_ref(),
            &ToolCallId::new("call_wait"),
            &output,
        )
        .await
        .expect("visible entries");
        let visible_messages = entries
            .iter()
            .filter(|entry| matches!(entry.kind, ContextEntryKind::Message { .. }))
            .collect::<Vec<_>>();

        assert_eq!(visible_messages.len(), 2);
        let header = blobs
            .read_text(&visible_messages[0].content_ref)
            .await
            .expect("read header");
        assert!(header.contains("Subagent result"));
        assert!(header.contains("target_session_id: child"));
        assert!(header.contains("run_id: run_1"));
        assert!(header.contains("status: completed"));
        assert!(header.contains("result_item_count: 1"));
        assert!(header.contains("result_item_1_kind: final_output"));
        assert!(header.contains("The next context item belongs to this subagent result."));
        assert_eq!(visible_messages[1].content_ref, binary_ref);
        assert!(visible_messages[1].preview.as_deref().is_some_and(|text| {
            text.contains("Subagent result item 1/1: final_output from child run_1")
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_executor_runs_read_and_writes_output_blobs() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let child = open_source_session(sessions.as_ref()).await;
        let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
        let runtime = Arc::new(FakeRuntime::default());
        let mut run = api_run_view("run_1", ApiRunStatus::Completed);
        run.items.push(api::SessionItemView::ToolResult {
            id: "item_1".to_owned(),
            call_id: "call_shell".to_owned(),
            output: Some("/opt\n## main...origin/main".to_owned()),
            is_error: false,
            status: api::ToolItemStatus::Succeeded,
        });
        run.items.push(api::SessionItemView::AssistantMessage {
            id: "item_2".to_owned(),
            text: "Command completed.".to_owned(),
        });
        runtime.sessions.lock().expect("lock").insert(
            child.clone(),
            api_session_view(&child, api::SessionStatus::Idle, vec![run]),
        );
        let service = FleetService::new(sessions, runtime);
        let executor = FleetToolExecutor::new(blobs.clone(), service);
        let arguments_ref = blobs
            .put_bytes(br#"{"target_session_id":"parent"}"#.to_vec())
            .await
            .expect("args");

        let result = executor
            .invoke(
                context(child),
                &ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new(AGENT_READ_TOOL_NAME),
                    arguments_ref,
                    execution_target: None,
                },
            )
            .await
            .expect("invoke");

        assert_eq!(result.status, ToolCallStatus::Succeeded);
        let output_ref = result.output_ref.as_ref().expect("output");
        let output: AgentReadOutput =
            serde_json::from_slice(&blobs.read_bytes(output_ref).await.expect("read output"))
                .expect("decode output");
        assert_eq!(output.session_id, "parent");
        let visible_ref = visible_tool_result_ref(&result);
        let visible = blobs.read_text(&visible_ref).await.expect("read visible");
        assert!(visible.contains("Read agent parent"));
        let visible_messages = result
            .model_visible_context_entries
            .iter()
            .filter(|entry| matches!(entry.kind, ContextEntryKind::Message { .. }))
            .collect::<Vec<_>>();
        assert_eq!(visible_messages.len(), 1);
        let transcript = blobs
            .read_text(&visible_messages[0].content_ref)
            .await
            .expect("read transcript");
        assert!(transcript.contains("Agent run transcript"));
        assert!(transcript.contains("target_session_id: parent"));
        assert!(transcript.contains("run_id: run_1"));
        assert!(transcript.contains("status: completed"));
        assert!(transcript.contains("Tool result:\n/opt"));
        assert!(transcript.contains("Assistant message:\nCommand completed."));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_executor_runs_profile_tools_and_writes_output_blobs() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
        let profile = test_profile("support");
        let runtime = Arc::new(FakeRuntime::default());
        runtime
            .profiles
            .lock()
            .expect("lock")
            .insert(profile.profile_id.clone(), profile.clone());
        let service = FleetService::new(sessions, runtime);
        let executor = FleetToolExecutor::new(blobs.clone(), service);

        let list_arguments_ref = blobs.put_bytes(br#"{}"#.to_vec()).await.expect("args");
        let list_result = executor
            .invoke(
                context(parent.clone()),
                &ToolInvocationRequest {
                    call_id: ToolCallId::new("call_profile_list"),
                    tool_name: ToolName::new(PROFILE_LIST_TOOL_NAME),
                    arguments_ref: list_arguments_ref,
                    execution_target: None,
                },
            )
            .await
            .expect("invoke list");

        assert_eq!(list_result.status, ToolCallStatus::Succeeded);
        let list_output_ref = list_result.output_ref.as_ref().expect("list output");
        let list_output: ProfileListOutput = serde_json::from_slice(
            &blobs
                .read_bytes(list_output_ref)
                .await
                .expect("read list output"),
        )
        .expect("decode list output");
        assert_eq!(list_output.profiles, vec![profile.summary()]);
        let list_visible_ref = visible_tool_result_ref(&list_result);
        let list_visible = blobs
            .read_text(&list_visible_ref)
            .await
            .expect("read list visible");
        assert!(list_visible.contains("Found 1 agent profile"));

        let read_arguments_ref = blobs
            .put_bytes(br#"{"profile_id":"support"}"#.to_vec())
            .await
            .expect("args");
        let read_result = executor
            .invoke(
                context(parent),
                &ToolInvocationRequest {
                    call_id: ToolCallId::new("call_profile_read"),
                    tool_name: ToolName::new(PROFILE_READ_TOOL_NAME),
                    arguments_ref: read_arguments_ref,
                    execution_target: None,
                },
            )
            .await
            .expect("invoke read");

        assert_eq!(read_result.status, ToolCallStatus::Succeeded);
        let read_output_ref = read_result.output_ref.as_ref().expect("read output");
        let read_output: ProfileReadOutput = serde_json::from_slice(
            &blobs
                .read_bytes(read_output_ref)
                .await
                .expect("read profile output"),
        )
        .expect("decode read output");
        assert_eq!(read_output.profile, profile);
        let read_visible_ref = visible_tool_result_ref(&read_result);
        let read_visible = blobs
            .read_text(&read_visible_ref)
            .await
            .expect("read visible");
        assert!(read_visible.contains("Read profile support revision 1"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_executor_runs_send_and_writes_output_blobs() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let parent = open_source_session(sessions.as_ref()).await;
        let child = create_linked_child(sessions.as_ref(), &parent).await;
        let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime);
        let executor = FleetToolExecutor::new(blobs.clone(), service);
        let arguments_ref = blobs
            .put_bytes(
                format!(
                    r#"{{"to":{{"kind":"session","target_session_id":"{}"}},"text":"do more work"}}"#,
                    child.as_str()
                )
                .into_bytes(),
            )
            .await
            .expect("args");

        let result = executor
            .invoke(
                context(parent),
                &ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new(AGENT_SEND_TOOL_NAME),
                    arguments_ref,
                    execution_target: None,
                },
            )
            .await
            .expect("invoke");

        assert_eq!(result.status, ToolCallStatus::Succeeded);
        let output_ref = result.output_ref.as_ref().expect("output");
        let output: AgentSendOutput =
            serde_json::from_slice(&blobs.read_bytes(output_ref).await.expect("read output"))
                .expect("decode output");
        assert_eq!(output.target_session_id.as_deref(), Some(child.as_str()));
        assert_eq!(output.run_id.as_deref(), Some("run_1"));
        let visible_ref = visible_tool_result_ref(&result);
        let visible = blobs.read_text(&visible_ref).await.expect("read visible");
        assert!(visible.contains("Delivered message"));
    }

    fn spawn_args(input: &str) -> AgentSpawnArgs {
        serde_json::from_value(json!({ "input": input })).expect("args")
    }

    fn spawn_args_with_child(input: &str, child_session_id: &str) -> AgentSpawnArgs {
        serde_json::from_value(json!({
            "input": input,
            "child_session_id": child_session_id
        }))
        .expect("args")
    }

    fn test_profile(profile_id: &str) -> AgentProfile {
        AgentProfile {
            profile_id: ProfileId::new(profile_id),
            display_name: Some("Support".to_owned()),
            description: Some("Ticket support profile".to_owned()),
            revision: 1,
            document: api::ProfileDocument {
                instructions: Some(api::ProfileInstructions::Text {
                    text: "Be concise.".to_owned(),
                }),
                ..api::ProfileDocument::default()
            },
            created_at_ms: 1,
            updated_at_ms: 2,
        }
    }

    async fn open_source_session(sessions: &InMemorySessionStore) -> SessionId {
        let source = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                session_id: source.clone(),
                created_at_ms: 1,
            })
            .await
            .expect("create source");
        let opening_events =
            core_agent_clone_opening_events(&open_state(), 2).expect("opening events");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: source.clone(),
                expected_head: None,
                events: opening_events,
            })
            .await
            .expect("append open");
        source
    }

    async fn create_linked_child(sessions: &InMemorySessionStore, parent: &SessionId) -> SessionId {
        let child = SessionId::new("child");
        sessions
            .create_cloned_session(CreateClonedSession {
                source_session_id: parent.clone(),
                session_id: child.clone(),
                created_at_ms: 20,
                opening_events: Vec::new(),
            })
            .await
            .expect("child");
        sessions
            .upsert_link(UpsertSessionLink {
                from_session_id: parent.clone(),
                to_session_id: child.clone(),
                relationship: FLEET_CHILD_RELATIONSHIP.to_owned(),
                created_at_ms: 21,
                metadata: json!({ "kind": "fleet_spawn" }),
            })
            .await
            .expect("link");
        child
    }

    async fn append_completed_run_with_output(
        sessions: &InMemorySessionStore,
        blobs: &engine::storage::InMemoryBlobStore,
        session_id: &SessionId,
        run_id: RunId,
        output: &str,
    ) {
        let output_ref = blobs
            .put_bytes(output.as_bytes().to_vec())
            .await
            .expect("put output");
        let mut events =
            core_agent_clone_opening_events(&open_state(), 22).expect("opening events");
        events.extend([
            core_uncommitted_event(
                30,
                engine::CoreAgentEvent::Run(engine::RunEvent::Accepted(engine::AcceptedRunEvent {
                    run_id,
                    submission_id: None,
                    source: engine::RunSource::Input { input: Vec::new() },
                    run_config: RunConfig::default(),
                    config_revision: 0,
                })),
            ),
            core_uncommitted_event(
                31,
                engine::CoreAgentEvent::Run(engine::RunEvent::Started { run_id }),
            ),
            core_uncommitted_event(
                32,
                engine::CoreAgentEvent::Run(engine::RunEvent::Completed {
                    run_id,
                    output_ref: Some(output_ref),
                }),
            ),
        ]);
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events,
            })
            .await
            .expect("append completed run");
    }

    fn core_uncommitted_event(
        observed_at_ms: u64,
        event: engine::CoreAgentEvent,
    ) -> engine::storage::UncommittedStoredEvent {
        engine::CoreAgentCodec
            .encode_uncommitted(&engine::UncommittedCoreAgentEvent {
                observed_at_ms,
                joins: Default::default(),
                event,
            })
            .expect("encode core event")
    }

    fn text_item(item: &InputItem) -> &str {
        let InputItem::Text { text } = item else {
            panic!("expected text input item");
        };
        text
    }

    fn text_item_json(item: &InputItem) -> Value {
        serde_json::from_str(text_item(item)).expect("input text should be JSON")
    }

    fn context(parent_session_id: SessionId) -> FleetInvocationContext {
        FleetInvocationContext {
            parent_session_id,
            parent_run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            call_id: ToolCallId::new("call_1"),
            observed_at_ms: 10,
        }
    }

    fn open_state() -> engine::CoreAgentState {
        let mut state = engine::CoreAgentState::new();
        state.lifecycle.config = Some(SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "test".to_owned(),
                model: "test-model".to_owned(),
            },
            run: RunConfig::default(),
            turn: TurnConfig {
                max_output_tokens: None,
                tool_choice: None,
                provider_params: None,
            },
            context: ContextConfig { compaction: None },
            tools: ToolConfig::default(),
        });
        state
    }

    fn api_session_view(
        session_id: &SessionId,
        status: api::SessionStatus,
        runs: Vec<api::RunView>,
    ) -> SessionView {
        SessionView {
            id: session_id.as_str().to_owned(),
            status,
            cwd: Some("/workspace".to_owned()),
            config_revision: 1,
            config: Some(api::SessionConfigView {
                model: api::ModelConfig {
                    provider_id: "test".to_owned(),
                    api_kind: "openaiResponses".to_owned(),
                    model: "test-model".to_owned(),
                },
                generation: api::GenerationConfig::default(),
                context: api::ContextConfigInput::default(),
                run_defaults: api::RunDefaultsConfig::default(),
                tools: api::ToolConfigView {
                    web_search: false,
                    web_fetch: false,
                    filesystem: api::FilesystemToolMode::Edit,
                    fleet: true,
                },
            }),
            created_at_ms: 1,
            updated_at_ms: 2,
            runs,
            active_context: api::ContextView::default(),
            active_tools: api::ActiveToolsView::default(),
            vfs_mounts: Vec::new(),
        }
    }

    fn api_run_view(run_id: &str, status: ApiRunStatus) -> api::RunView {
        api::RunView {
            id: run_id.to_owned(),
            status,
            source: api::RunViewSource::Input { items: Vec::new() },
            items: Vec::new(),
            tool_batches: Vec::new(),
        }
    }

    fn api_event(session_id: &SessionId, seq: u64) -> api::SessionEventView {
        api::SessionEventView {
            cursor: api::EventCursor { seq },
            session_id: session_id.as_str().to_owned(),
            observed_at_ms: seq,
            joins: api::EventJoinsView::default(),
            kind: api::SessionEventKindView::SessionConfigChanged {
                model: None,
                revision: seq,
            },
        }
    }

    fn stored_test_event(
        at_ms: u64,
        kind: &'static str,
    ) -> engine::storage::UncommittedStoredEvent {
        engine::storage::UncommittedStoredEvent {
            observed_at_ms: at_ms,
            joins: Default::default(),
            event: engine::StoredEvent::new(kind, 1, Value::Object(Default::default())),
        }
    }

    #[derive(Default)]
    struct TestVfsCatalog {
        workspaces: Mutex<BTreeMap<VfsWorkspaceId, VfsWorkspaceRecord>>,
        mounts: Mutex<BTreeMap<(SessionId, VfsPath), VfsMountRecord>>,
    }

    #[async_trait]
    impl VfsWorkspaceStore for TestVfsCatalog {
        async fn create_workspace(
            &self,
            record: CreateVfsWorkspaceRecord,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            let mut workspaces = self.workspaces.lock().expect("workspace lock");
            if workspaces.contains_key(&record.workspace_id) {
                return Err(VfsCatalogError::AlreadyExists {
                    kind: "workspace",
                    id: record.workspace_id.to_string(),
                });
            }
            let workspace = VfsWorkspaceRecord {
                workspace_id: record.workspace_id,
                display_name: record.display_name,
                base_snapshot_ref: record.base_snapshot_ref,
                head_snapshot_ref: record.head_snapshot_ref,
                head_totals: record.head_totals,
                revision: 0,
                created_at_ms: record.created_at_ms,
                updated_at_ms: record.created_at_ms,
            };
            workspaces.insert(workspace.workspace_id.clone(), workspace.clone());
            Ok(workspace)
        }

        async fn read_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            self.workspaces
                .lock()
                .expect("workspace lock")
                .get(workspace_id)
                .cloned()
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
        }

        async fn list_workspaces(&self) -> Result<Vec<VfsWorkspaceRecord>, VfsCatalogError> {
            Ok(self
                .workspaces
                .lock()
                .expect("workspace lock")
                .values()
                .cloned()
                .collect())
        }

        async fn compare_and_set_head(
            &self,
            request: CompareAndSetVfsWorkspaceHead,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            let mut workspaces = self.workspaces.lock().expect("workspace lock");
            let workspace = workspaces.get_mut(&request.workspace_id).ok_or_else(|| {
                VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: request.workspace_id.to_string(),
                }
            })?;
            if let Some(display_name) = request.display_name {
                workspace.display_name = Some(display_name);
            }
            workspace.head_snapshot_ref = request.new_head_snapshot_ref;
            workspace.head_totals = request.new_head_totals;
            workspace.revision += 1;
            workspace.updated_at_ms = request.updated_at_ms;
            Ok(workspace.clone())
        }

        async fn delete_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            self.workspaces
                .lock()
                .expect("workspace lock")
                .remove(workspace_id)
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
        }
    }

    #[async_trait]
    impl VfsMountStore for TestVfsCatalog {
        async fn put_mount(&self, record: VfsMountRecord) -> Result<(), VfsCatalogError> {
            self.mounts.lock().expect("mount lock").insert(
                (record.session_id.clone(), record.mount_path.clone()),
                record,
            );
            Ok(())
        }

        async fn list_mounts(
            &self,
            session_id: &SessionId,
        ) -> Result<Vec<VfsMountRecord>, VfsCatalogError> {
            let mut mounts: Vec<_> = self
                .mounts
                .lock()
                .expect("mount lock")
                .values()
                .filter(|mount| &mount.session_id == session_id)
                .cloned()
                .collect();
            mounts.sort_by(|left, right| left.mount_path.as_str().cmp(right.mount_path.as_str()));
            Ok(mounts)
        }

        async fn remove_mount(
            &self,
            session_id: &SessionId,
            mount_path: &VfsPath,
        ) -> Result<(), VfsCatalogError> {
            self.mounts
                .lock()
                .expect("mount lock")
                .remove(&(session_id.clone(), mount_path.clone()))
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "mount",
                    id: format!("{session_id}:{mount_path}"),
                })?;
            Ok(())
        }
    }
}

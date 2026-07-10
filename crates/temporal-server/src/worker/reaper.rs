//! Deployment-level promise/source repair loop.
//!
//! Workflow-local machinery handles the normal path: terminal runs notify
//! holder workflows, and holder-side promise cancellation flushes source
//! cancellation. This reaper is the backstop for the cases that no single
//! workflow can repair by itself: missed signals, terminated workflows, or
//! promise/source state that is only visible by scanning session logs.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use api_projection::{MAX_EVENT_PAGE_LIMIT, read_all_session_entries, replay_core_agent_state};
use async_trait::async_trait;
use engine::{
    CoreAgentAction, CoreAgentCommand, CoreAgentDrive, CoreAgentState, CoreAgentStatus, Promise,
    PromiseId, PromiseResolution, PromiseScope, PromiseSource, PromiseSourceCancelRequest,
    PromiseSourceCheckRequest, PromiseSourceCheckResult, RunId, RunRecord, RunStatus, SessionId,
    storage::{
        AppendSessionEvents, ListSessions, SessionListCursor, SessionRecord, SessionStore,
        SessionStoreError,
    },
};
use temporal_workflow::{AgentAdmission, AgentSessionWorkflow, compose_workflow_id};
use temporalio_client::{
    Client, WorkflowDescribeOptions, WorkflowSignalOptions, errors::WorkflowInteractionError,
};
use temporalio_common::protos::temporal::api::enums::v1::WorkflowExecutionStatus;
use thiserror::Error;
use uuid::Uuid;

use crate::{config::DeploymentStores, worker::SessionTools};

const DEFAULT_REAPER_INTERVAL: Duration = Duration::from_secs(5 * 60);
const SESSION_PAGE_LIMIT: usize = 256;

#[derive(Clone)]
pub struct PromiseReaper {
    client: Client,
    stores: DeploymentStores,
    interval: Duration,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReaperStats {
    pub universes_scanned: usize,
    pub sessions_scanned: usize,
    pub promises_examined: usize,
    pub holder_repairs_signalled: usize,
    pub holder_repairs_appended: usize,
    pub child_cancels_signalled: usize,
    pub child_cancels_appended: usize,
    pub source_cancels: usize,
    pub conflicts: usize,
    pub errors: usize,
}

impl ReaperStats {
    fn merge(&mut self, other: Self) {
        self.universes_scanned += other.universes_scanned;
        self.sessions_scanned += other.sessions_scanned;
        self.promises_examined += other.promises_examined;
        self.holder_repairs_signalled += other.holder_repairs_signalled;
        self.holder_repairs_appended += other.holder_repairs_appended;
        self.child_cancels_signalled += other.child_cancels_signalled;
        self.child_cancels_appended += other.child_cancels_appended;
        self.source_cancels += other.source_cancels;
        self.conflicts += other.conflicts;
        self.errors += other.errors;
    }

    fn repaired_anything(&self) -> bool {
        self.holder_repairs_signalled > 0
            || self.holder_repairs_appended > 0
            || self.child_cancels_signalled > 0
            || self.child_cancels_appended > 0
            || self.source_cancels > 0
    }
}

impl PromiseReaper {
    pub fn new(client: Client, stores: DeploymentStores) -> Self {
        Self {
            client,
            stores,
            interval: DEFAULT_REAPER_INTERVAL,
        }
    }

    pub async fn run_forever(self) {
        loop {
            match self.run_once().await {
                Ok(stats)
                    if stats.repaired_anything() || stats.errors > 0 || stats.conflicts > 0 =>
                {
                    tracing::info!(
                        target: "temporal_server",
                        universes_scanned = stats.universes_scanned,
                        sessions_scanned = stats.sessions_scanned,
                        promises_examined = stats.promises_examined,
                        holder_repairs_signalled = stats.holder_repairs_signalled,
                        holder_repairs_appended = stats.holder_repairs_appended,
                        child_cancels_signalled = stats.child_cancels_signalled,
                        child_cancels_appended = stats.child_cancels_appended,
                        source_cancels = stats.source_cancels,
                        conflicts = stats.conflicts,
                        errors = stats.errors,
                        "promise reaper pass complete"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        target: "temporal_server",
                        %error,
                        "promise reaper pass failed"
                    );
                }
            }
            tokio::time::sleep(self.interval).await;
        }
    }

    pub async fn run_once(&self) -> anyhow::Result<ReaperStats> {
        let workflows: Arc<dyn WorkflowRepairClient> = Arc::new(TemporalWorkflowRepairClient {
            client: self.client.clone(),
        });
        let universes = store_pg::list_universes(self.stores.pool()).await?;
        let mut stats = ReaperStats::default();
        for (universe_id, _) in universes {
            let store = self.stores.store_for(universe_id);
            let source_tools: Arc<dyn engine::CoreAgentTools> = Arc::new(
                SessionTools::from_pg_store(store.clone())
                    .with_environment_job_workflow_client(self.client.clone(), universe_id),
            );
            let sessions: Arc<dyn SessionStore> = store.clone();
            let append_store: Arc<dyn SessionStore> = store;
            let universe_stats = reap_universe_once(
                universe_id,
                sessions,
                append_store,
                source_tools,
                workflows.clone(),
                now_ms(),
            )
            .await?;
            stats.merge(universe_stats);
        }
        Ok(stats)
    }
}

#[derive(Clone)]
struct LoadedSessionSnapshot {
    record: SessionRecord,
    state: CoreAgentState,
}

#[derive(Default)]
struct ReaperPlan {
    holder_commands: BTreeMap<SessionId, Vec<CoreAgentCommand>>,
    child_cancels: BTreeSet<(SessionId, RunId)>,
    source_cancels: Vec<PromiseSource>,
}

pub(super) async fn reap_universe_once(
    universe_id: Uuid,
    sessions: Arc<dyn SessionStore>,
    append_store: Arc<dyn SessionStore>,
    source_tools: Arc<dyn engine::CoreAgentTools>,
    workflows: Arc<dyn WorkflowRepairClient>,
    now_ms: u64,
) -> anyhow::Result<ReaperStats> {
    let snapshots = load_session_snapshots(sessions.as_ref()).await?;
    let mut running_cache = BTreeMap::<SessionId, bool>::new();
    let mut stats = ReaperStats {
        universes_scanned: 1,
        sessions_scanned: snapshots.len(),
        ..ReaperStats::default()
    };
    let plan = plan_repair(
        universe_id,
        &snapshots,
        source_tools.as_ref(),
        workflows.as_ref(),
        &mut running_cache,
        now_ms,
        &mut stats,
    )
    .await;

    apply_holder_repairs(
        universe_id,
        append_store.clone(),
        workflows.as_ref(),
        &snapshots,
        plan.holder_commands,
        now_ms,
        &mut stats,
    )
    .await;
    apply_child_cancels(
        universe_id,
        append_store,
        workflows.as_ref(),
        &snapshots,
        &mut running_cache,
        plan.child_cancels,
        now_ms,
        &mut stats,
    )
    .await;
    for source in plan.source_cancels {
        match source {
            PromiseSource::EnvJob { .. } => {
                match source_tools
                    .cancel_promise_source(PromiseSourceCancelRequest { source })
                    .await
                {
                    Ok(_) => stats.source_cancels += 1,
                    Err(error) => {
                        stats.errors += 1;
                        tracing::warn!(
                            target: "temporal_server",
                            %universe_id,
                            %error,
                            "promise reaper failed to cancel promise source"
                        );
                    }
                }
            }
            PromiseSource::Timer { .. } | PromiseSource::Run { .. } => {}
        }
    }
    Ok(stats)
}

async fn plan_repair(
    universe_id: Uuid,
    snapshots: &BTreeMap<SessionId, LoadedSessionSnapshot>,
    source_tools: &dyn engine::CoreAgentTools,
    workflows: &dyn WorkflowRepairClient,
    running_cache: &mut BTreeMap<SessionId, bool>,
    now_ms: u64,
    stats: &mut ReaperStats,
) -> ReaperPlan {
    let mut plan = ReaperPlan::default();
    for (holder_session_id, holder) in snapshots {
        for promise in holder.state.promises.pending() {
            stats.promises_examined += 1;
            if !promise_owner_live(&holder.state, promise) {
                plan_holder_resolution(
                    &mut plan,
                    holder_session_id,
                    promise.promise_id.clone(),
                    PromiseResolution::Cancelled,
                );
                plan_source_cancel(&mut plan, &promise.source);
                continue;
            }

            match promise_source_resolution(
                universe_id,
                snapshots,
                source_tools,
                workflows,
                running_cache,
                &mut plan,
                &promise.source,
                now_ms,
                stats,
            )
            .await
            {
                Some(resolution) => {
                    plan_holder_resolution(
                        &mut plan,
                        holder_session_id,
                        promise.promise_id.clone(),
                        resolution,
                    );
                }
                None => {}
            }
        }
    }
    plan
}

fn plan_holder_resolution(
    plan: &mut ReaperPlan,
    holder_session_id: &SessionId,
    promise_id: PromiseId,
    resolution: PromiseResolution,
) {
    plan.holder_commands
        .entry(holder_session_id.clone())
        .or_default()
        .push(CoreAgentCommand::ResolvePromise {
            promise_id,
            resolution,
        });
}

fn plan_source_cancel(plan: &mut ReaperPlan, source: &PromiseSource) {
    match source {
        PromiseSource::Run {
            target_session_id,
            target_run_id,
        } => {
            if let Ok(session_id) = SessionId::try_new(target_session_id.clone()) {
                plan.child_cancels
                    .insert((session_id, RunId::new(*target_run_id)));
            }
        }
        PromiseSource::EnvJob { .. } => plan.source_cancels.push(source.clone()),
        PromiseSource::Timer { .. } => {}
    }
}

async fn promise_source_resolution(
    universe_id: Uuid,
    snapshots: &BTreeMap<SessionId, LoadedSessionSnapshot>,
    source_tools: &dyn engine::CoreAgentTools,
    workflows: &dyn WorkflowRepairClient,
    running_cache: &mut BTreeMap<SessionId, bool>,
    plan: &mut ReaperPlan,
    source: &PromiseSource,
    now_ms: u64,
    stats: &mut ReaperStats,
) -> Option<PromiseResolution> {
    match source {
        PromiseSource::Run {
            target_session_id,
            target_run_id,
        } => {
            let Ok(target_session_id) = SessionId::try_new(target_session_id.clone()) else {
                return Some(PromiseResolution::Failed { error_ref: None });
            };
            let target_run_id = RunId::new(*target_run_id);
            let Some(target) = snapshots.get(&target_session_id) else {
                return Some(PromiseResolution::Failed { error_ref: None });
            };
            if let Some(record) = terminal_run_record(&target.state, target_run_id) {
                return Some(run_record_resolution(record));
            }
            if target_run_is_nonterminal(&target.state, target_run_id)
                && !workflow_is_running_cached(
                    workflows,
                    running_cache,
                    universe_id,
                    &target_session_id,
                )
                .await
            {
                plan.child_cancels
                    .insert((target_session_id, target_run_id));
                return Some(PromiseResolution::Failed { error_ref: None });
            }
            None
        }
        PromiseSource::EnvJob { .. } => match source_tools
            .check_promise_source(PromiseSourceCheckRequest {
                source: source.clone(),
            })
            .await
        {
            Ok(PromiseSourceCheckResult::Pending) => None,
            Ok(PromiseSourceCheckResult::Resolved { payload_ref }) => {
                Some(PromiseResolution::Resolved { payload_ref })
            }
            Ok(PromiseSourceCheckResult::Failed { error_ref }) => {
                Some(PromiseResolution::Failed { error_ref })
            }
            Err(error) => {
                stats.errors += 1;
                tracing::warn!(
                    target: "temporal_server",
                    %universe_id,
                    %error,
                    "promise reaper failed to check promise source"
                );
                None
            }
        },
        PromiseSource::Timer { fire_at_ms } => {
            (*fire_at_ms <= now_ms).then_some(PromiseResolution::Resolved { payload_ref: None })
        }
    }
}

fn promise_owner_live(state: &CoreAgentState, promise: &Promise) -> bool {
    match promise.scope {
        PromiseScope::Run { run_id } => state
            .runs
            .active
            .as_ref()
            .is_some_and(|run| run.run_id == run_id),
        PromiseScope::Session => state.lifecycle.status != CoreAgentStatus::Closed,
    }
}

fn terminal_run_record(state: &CoreAgentState, run_id: RunId) -> Option<&RunRecord> {
    state
        .runs
        .completed
        .iter()
        .find(|record| record.run_id == run_id)
}

fn run_record_resolution(record: &RunRecord) -> PromiseResolution {
    match record.status {
        RunStatus::Completed => PromiseResolution::Resolved {
            payload_ref: record.output_ref.clone(),
        },
        RunStatus::Failed | RunStatus::Cancelled => PromiseResolution::Failed {
            error_ref: record
                .failure
                .as_ref()
                .and_then(|failure| failure.message_ref.clone()),
        },
        RunStatus::Active
        | RunStatus::Parked
        | RunStatus::Cancelling
        | RunStatus::CancellingGrace => PromiseResolution::Failed { error_ref: None },
    }
}

fn target_run_is_nonterminal(state: &CoreAgentState, run_id: RunId) -> bool {
    state
        .runs
        .active
        .as_ref()
        .is_some_and(|run| run.run_id == run_id)
        || state.runs.queued.iter().any(|run| run.run_id == run_id)
}

async fn workflow_is_running_cached(
    workflows: &dyn WorkflowRepairClient,
    cache: &mut BTreeMap<SessionId, bool>,
    universe_id: Uuid,
    session_id: &SessionId,
) -> bool {
    if let Some(running) = cache.get(session_id) {
        return *running;
    }
    let running = workflows.workflow_is_running(universe_id, session_id).await;
    cache.insert(session_id.clone(), running);
    running
}

async fn apply_holder_repairs(
    universe_id: Uuid,
    store: Arc<dyn SessionStore>,
    workflows: &dyn WorkflowRepairClient,
    snapshots: &BTreeMap<SessionId, LoadedSessionSnapshot>,
    holder_commands: BTreeMap<SessionId, Vec<CoreAgentCommand>>,
    now_ms: u64,
    stats: &mut ReaperStats,
) {
    for (session_id, commands) in holder_commands {
        match workflows
            .signal_admissions(universe_id, &session_id, admissions(commands.clone()))
            .await
        {
            Ok(()) => {
                stats.holder_repairs_signalled += commands.len();
                continue;
            }
            Err(WorkflowSignalFailure::NotFound) => {}
            Err(WorkflowSignalFailure::Other(error)) => {
                stats.errors += 1;
                tracing::warn!(
                    target: "temporal_server",
                    %universe_id,
                    %session_id,
                    %error,
                    "promise reaper failed to signal holder repair"
                );
                continue;
            }
        }
        let Some(snapshot) = snapshots.get(&session_id) else {
            continue;
        };
        match append_commands_direct(store.as_ref(), &session_id, snapshot, commands, now_ms).await
        {
            Ok(appended) => stats.holder_repairs_appended += appended,
            Err(DirectAppendError::Conflict) => stats.conflicts += 1,
            Err(DirectAppendError::Other(error)) => {
                stats.errors += 1;
                tracing::warn!(
                    target: "temporal_server",
                    %universe_id,
                    %session_id,
                    %error,
                    "promise reaper failed to append holder repair"
                );
            }
        }
    }
}

async fn apply_child_cancels(
    universe_id: Uuid,
    store: Arc<dyn SessionStore>,
    workflows: &dyn WorkflowRepairClient,
    snapshots: &BTreeMap<SessionId, LoadedSessionSnapshot>,
    running_cache: &mut BTreeMap<SessionId, bool>,
    child_cancels: BTreeSet<(SessionId, RunId)>,
    now_ms: u64,
    stats: &mut ReaperStats,
) {
    for (session_id, run_id) in child_cancels {
        if workflow_is_running_cached(workflows, running_cache, universe_id, &session_id).await {
            let command = CoreAgentCommand::CancelRun { run_id };
            match workflows
                .signal_admissions(universe_id, &session_id, admissions(vec![command]))
                .await
            {
                Ok(()) => {
                    stats.child_cancels_signalled += 1;
                    continue;
                }
                Err(WorkflowSignalFailure::NotFound) => {}
                Err(WorkflowSignalFailure::Other(error)) => {
                    stats.errors += 1;
                    tracing::warn!(
                        target: "temporal_server",
                        %universe_id,
                        %session_id,
                        run_id = run_id.as_u64(),
                        %error,
                        "promise reaper failed to signal child cancellation"
                    );
                    continue;
                }
            }
        }
        let Some(snapshot) = snapshots.get(&session_id) else {
            continue;
        };
        let command = direct_child_cancel_command(&snapshot.state, run_id);
        match append_commands_direct(store.as_ref(), &session_id, snapshot, vec![command], now_ms)
            .await
        {
            Ok(appended) => stats.child_cancels_appended += appended,
            Err(DirectAppendError::Conflict) => stats.conflicts += 1,
            Err(DirectAppendError::Other(error)) => {
                stats.errors += 1;
                tracing::warn!(
                    target: "temporal_server",
                    %universe_id,
                    %session_id,
                    run_id = run_id.as_u64(),
                    %error,
                    "promise reaper failed to append child cancellation"
                );
            }
        }
    }
}

fn direct_child_cancel_command(state: &CoreAgentState, run_id: RunId) -> CoreAgentCommand {
    if state
        .runs
        .active
        .as_ref()
        .is_some_and(|active| active.run_id == run_id)
    {
        CoreAgentCommand::ForceCancelRun { run_id }
    } else {
        CoreAgentCommand::CancelRun { run_id }
    }
}

fn admissions(commands: Vec<CoreAgentCommand>) -> Vec<AgentAdmission> {
    commands
        .into_iter()
        .map(|command| AgentAdmission {
            command,
            correlation_token: None,
        })
        .collect()
}

#[derive(Debug, Error)]
enum DirectAppendError {
    #[error("expected head conflict")]
    Conflict,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

async fn append_commands_direct(
    store: &dyn SessionStore,
    session_id: &SessionId,
    snapshot: &LoadedSessionSnapshot,
    commands: Vec<CoreAgentCommand>,
    now_ms: u64,
) -> Result<usize, DirectAppendError> {
    let mut drive = CoreAgentDrive::from_replayed(
        session_id.clone(),
        snapshot.state.clone(),
        snapshot.record.head.clone(),
    );
    let mut appended_count = 0usize;
    for command in commands {
        let action = drive
            .admit_command(command, now_ms)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        match action {
            CoreAgentAction::AppendEvents {
                expected_head,
                events,
            } => {
                let result = store
                    .append(AppendSessionEvents {
                        session_id: session_id.clone(),
                        expected_head,
                        events,
                    })
                    .await
                    .map_err(|error| match error {
                        SessionStoreError::ExpectedHeadMismatch { .. } => {
                            DirectAppendError::Conflict
                        }
                        other => DirectAppendError::Other(anyhow::anyhow!("{other}")),
                    })?;
                if !result.entries.is_empty() {
                    appended_count += 1;
                }
                drive
                    .resume_appended(result.entries)
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
            }
            CoreAgentAction::Idle | CoreAgentAction::Closed => {}
            other => {
                return Err(DirectAppendError::Other(anyhow::anyhow!(
                    "direct repair command produced unexpected action: {other:?}"
                )));
            }
        }
    }
    Ok(appended_count)
}

async fn load_session_snapshots(
    sessions: &dyn SessionStore,
) -> anyhow::Result<BTreeMap<SessionId, LoadedSessionSnapshot>> {
    let mut cursor: Option<SessionListCursor> = None;
    let mut snapshots = BTreeMap::new();
    loop {
        let page = sessions
            .list_sessions(ListSessions {
                cursor,
                limit: SESSION_PAGE_LIMIT,
            })
            .await?;
        for record in page.sessions {
            let entries = read_all_session_entries(
                sessions,
                &record.session_id,
                MAX_EVENT_PAGE_LIMIT as usize,
            )
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
            let state =
                replay_core_agent_state(&entries).map_err(|error| anyhow::anyhow!("{error}"))?;
            snapshots.insert(
                record.session_id.clone(),
                LoadedSessionSnapshot { record, state },
            );
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            return Ok(snapshots);
        }
    }
}

#[derive(Debug, Error)]
pub(super) enum WorkflowSignalFailure {
    #[error("workflow not found")]
    NotFound,
    #[error("{0}")]
    Other(String),
}

#[async_trait]
pub(super) trait WorkflowRepairClient: Send + Sync {
    async fn signal_admissions(
        &self,
        universe_id: Uuid,
        session_id: &SessionId,
        admissions: Vec<AgentAdmission>,
    ) -> Result<(), WorkflowSignalFailure>;

    async fn workflow_is_running(&self, universe_id: Uuid, session_id: &SessionId) -> bool;
}

struct TemporalWorkflowRepairClient {
    client: Client,
}

#[async_trait]
impl WorkflowRepairClient for TemporalWorkflowRepairClient {
    async fn signal_admissions(
        &self,
        universe_id: Uuid,
        session_id: &SessionId,
        admissions: Vec<AgentAdmission>,
    ) -> Result<(), WorkflowSignalFailure> {
        let workflow_id = compose_workflow_id(universe_id, session_id);
        match self
            .client
            .get_workflow_handle::<AgentSessionWorkflow>(workflow_id)
            .signal(
                AgentSessionWorkflow::submit_admissions,
                admissions,
                WorkflowSignalOptions::default(),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(WorkflowInteractionError::NotFound(_)) => Err(WorkflowSignalFailure::NotFound),
            Err(error) => Err(WorkflowSignalFailure::Other(error.to_string())),
        }
    }

    async fn workflow_is_running(&self, universe_id: Uuid, session_id: &SessionId) -> bool {
        let workflow_id = compose_workflow_id(universe_id, session_id);
        match self
            .client
            .get_workflow_handle::<AgentSessionWorkflow>(workflow_id)
            .describe(WorkflowDescribeOptions::default())
            .await
        {
            Ok(description) => {
                matches!(description.status(), WorkflowExecutionStatus::Running)
            }
            Err(_) => false,
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use engine::{
        ActiveRun, BlobRef, CoreAgentCodec, CoreAgentEvent, CoreAgentIoError, CoreAgentTools,
        ModelSelection, PromiseStatus, ProviderApiKind, RunFailure, RunFailureKind, RunOrigin,
        RunSource, ToolBatchOutcome, ToolInvocationBatchRequest,
        storage::{CreateSession, InMemorySessionStore, ReadSessionEvents},
    };
    use temporal_workflow::{DEFAULT_MODEL, default_run_config, default_session_config};

    use super::*;

    #[derive(Default)]
    struct FakeWorkflows {
        running: BTreeSet<SessionId>,
        signals: Mutex<Vec<(SessionId, Vec<AgentAdmission>)>>,
    }

    #[async_trait]
    impl WorkflowRepairClient for FakeWorkflows {
        async fn signal_admissions(
            &self,
            _universe_id: Uuid,
            session_id: &SessionId,
            admissions: Vec<AgentAdmission>,
        ) -> Result<(), WorkflowSignalFailure> {
            if !self.running.contains(session_id) {
                return Err(WorkflowSignalFailure::NotFound);
            }
            self.signals
                .lock()
                .expect("signals lock")
                .push((session_id.clone(), admissions));
            Ok(())
        }

        async fn workflow_is_running(&self, _universe_id: Uuid, session_id: &SessionId) -> bool {
            self.running.contains(session_id)
        }
    }

    struct FakeSourceTools;

    #[async_trait]
    impl CoreAgentTools for FakeSourceTools {
        async fn invoke_batch(
            &self,
            _request: ToolInvocationBatchRequest,
        ) -> Result<ToolBatchOutcome, CoreAgentIoError> {
            Err(CoreAgentIoError::Failed {
                message: "fake source tools do not invoke batches".to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn terminal_owner_cancels_pending_child_promise_and_child_run() {
        let universe_id = Uuid::new_v4();
        let holder_id = SessionId::new("holder");
        let child_id = SessionId::new("child");
        let promise_id = PromiseId::new("promise_child");
        let holder_run_id = RunId::new(1);
        let child_run_id = RunId::new(1);

        let mut holder_state = open_state();
        holder_state
            .runs
            .completed
            .push(run_record(holder_run_id, RunStatus::Cancelled, None));
        holder_state.promises.promises.insert(
            promise_id.clone(),
            Promise {
                promise_id: promise_id.clone(),
                source: PromiseSource::Run {
                    target_session_id: child_id.as_str().to_owned(),
                    target_run_id: child_run_id.as_u64(),
                },
                scope: PromiseScope::Run {
                    run_id: holder_run_id,
                },
                status: PromiseStatus::Pending,
                payload_ref: None,
                error_ref: None,
                deadline_ms: None,
            },
        );
        let mut child_state = open_state();
        child_state.runs.active = Some(active_run(child_run_id));

        let snapshots = snapshots([
            (holder_id.clone(), holder_state),
            (child_id.clone(), child_state),
        ]);
        let workflows = FakeWorkflows::default();
        let tools = FakeSourceTools;
        let mut running_cache = BTreeMap::new();
        let mut stats = ReaperStats::default();
        let plan = plan_repair(
            universe_id,
            &snapshots,
            &tools,
            &workflows,
            &mut running_cache,
            1_000,
            &mut stats,
        )
        .await;

        assert!(matches!(
            &plan.holder_commands[&holder_id][0],
            CoreAgentCommand::ResolvePromise {
                promise_id: planned,
                resolution: PromiseResolution::Cancelled
            } if planned == &promise_id
        ));
        assert!(
            plan.child_cancels
                .contains(&(child_id.clone(), child_run_id))
        );

        let store = Arc::new(InMemorySessionStore::new());
        create_store_session(store.as_ref(), &holder_id).await;
        create_store_session(store.as_ref(), &child_id).await;
        let append_store: Arc<dyn SessionStore> = store.clone();
        let mut apply_stats = ReaperStats::default();
        apply_holder_repairs(
            universe_id,
            append_store.clone(),
            &workflows,
            &snapshots,
            plan.holder_commands,
            2_000,
            &mut apply_stats,
        )
        .await;
        apply_child_cancels(
            universe_id,
            append_store,
            &workflows,
            &snapshots,
            &mut running_cache,
            plan.child_cancels,
            2_000,
            &mut apply_stats,
        )
        .await;

        assert_eq!(apply_stats.holder_repairs_appended, 1);
        assert_eq!(apply_stats.child_cancels_appended, 1);
        assert!(matches!(
            first_event(store.as_ref(), &holder_id).await,
            CoreAgentEvent::Promise(engine::PromiseEvent::Cancelled { promise_id: id })
                if id == promise_id
        ));
        assert!(matches!(
            first_event(store.as_ref(), &child_id).await,
            CoreAgentEvent::Run(engine::RunEvent::ForceCancelled { run_id })
                if run_id == child_run_id
        ));
    }

    #[tokio::test]
    async fn gone_child_workflow_fails_holder_promise_and_force_cancels_child() {
        let universe_id = Uuid::new_v4();
        let holder_id = SessionId::new("holder");
        let child_id = SessionId::new("child");
        let promise_id = PromiseId::new("promise_child");
        let holder_run_id = RunId::new(1);
        let child_run_id = RunId::new(1);

        let mut holder_state = open_state();
        holder_state.runs.active = Some(active_run(holder_run_id));
        holder_state.promises.promises.insert(
            promise_id.clone(),
            Promise {
                promise_id: promise_id.clone(),
                source: PromiseSource::Run {
                    target_session_id: child_id.as_str().to_owned(),
                    target_run_id: child_run_id.as_u64(),
                },
                scope: PromiseScope::Run {
                    run_id: holder_run_id,
                },
                status: PromiseStatus::Pending,
                payload_ref: None,
                error_ref: None,
                deadline_ms: None,
            },
        );
        let mut child_state = open_state();
        child_state.runs.active = Some(active_run(child_run_id));

        let snapshots = snapshots([
            (holder_id.clone(), holder_state),
            (child_id.clone(), child_state),
        ]);
        let workflows = FakeWorkflows::default();
        let tools = FakeSourceTools;
        let mut running_cache = BTreeMap::new();
        let mut stats = ReaperStats::default();
        let plan = plan_repair(
            universe_id,
            &snapshots,
            &tools,
            &workflows,
            &mut running_cache,
            1_000,
            &mut stats,
        )
        .await;

        assert!(matches!(
            &plan.holder_commands[&holder_id][0],
            CoreAgentCommand::ResolvePromise {
                promise_id: planned,
                resolution: PromiseResolution::Failed { error_ref: None }
            } if planned == &promise_id
        ));
        assert!(
            plan.child_cancels
                .contains(&(child_id.clone(), child_run_id))
        );
    }

    fn open_state() -> CoreAgentState {
        let mut state = CoreAgentState::new();
        state.lifecycle.status = CoreAgentStatus::Open;
        state.lifecycle.config = Some(default_session_config(test_model()));
        state
    }

    fn test_model() -> ModelSelection {
        ModelSelection {
            api_kind: ProviderApiKind::OpenAiResponses,
            provider_id: "openai".to_owned(),
            model: DEFAULT_MODEL.to_owned(),
        }
    }

    fn active_run(run_id: RunId) -> ActiveRun {
        ActiveRun {
            run_id,
            status: RunStatus::Active,
            submission_id: None,
            origin: RunOrigin::Requested,
            source: RunSource::Input { input: Vec::new() },
            input_entry_ids: Vec::new(),
            input_consumed_by_turn_id: None,
            run_config: default_run_config(),
            config_revision: 0,
            steering: Vec::new(),
            turns: BTreeMap::new(),
            active_turn_id: None,
            active_tool_batch_id: None,
            cancellation_grace_turn_id: None,
            parked_await: None,
            tool_batches: BTreeMap::new(),
            completed_tool_batches: BTreeMap::new(),
            output_ref: None,
            failure: None,
            notify_on_terminal: Vec::new(),
        }
    }

    fn run_record(run_id: RunId, status: RunStatus, output_ref: Option<BlobRef>) -> RunRecord {
        RunRecord {
            run_id,
            status,
            submission_id: None,
            origin: RunOrigin::Requested,
            submission_digest: None,
            output_ref,
            failure: (status == RunStatus::Failed).then_some(RunFailure {
                kind: RunFailureKind::Internal,
                message_ref: None,
            }),
            notify_on_terminal: Vec::new(),
        }
    }

    fn snapshots(
        states: impl IntoIterator<Item = (SessionId, CoreAgentState)>,
    ) -> BTreeMap<SessionId, LoadedSessionSnapshot> {
        states
            .into_iter()
            .map(|(session_id, state)| {
                (
                    session_id.clone(),
                    LoadedSessionSnapshot {
                        record: SessionRecord {
                            session_id,
                            display_name: None,
                            lifecycle_status: engine::storage::SessionLifecycleStatus::New,
                            closed_at_seq: None,
                            head: None,
                            source_session_id: None,
                            source_seq: None,
                            created_at_ms: 0,
                            updated_at_ms: 0,
                        },
                        state,
                    },
                )
            })
            .collect()
    }

    async fn create_store_session(store: &InMemorySessionStore, session_id: &SessionId) {
        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                display_name: None,
                created_at_ms: 0,
            })
            .await
            .expect("create session");
    }

    async fn first_event(store: &InMemorySessionStore, session_id: &SessionId) -> CoreAgentEvent {
        let page = store
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
                after: None,
                limit: 10,
            })
            .await
            .expect("read session events");
        let entry = page.entries.into_iter().next().expect("event");
        CoreAgentCodec
            .decode_entry(&entry)
            .expect("decode event")
            .event
    }
}

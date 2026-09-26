//! Deployment-level background loops of the sessions role: promise/source
//! repair, session retention, and content-addressed blob collection.
//!
//! Workflow-local machinery handles the normal path: terminal runs notify
//! holder workflows, and holder-side promise cancellation flushes source
//! cancellation. The promise reaper is the backstop for the cases that no
//! single workflow can repair by itself: missed signals, terminated
//! workflows, or promise/source state that is only visible by scanning
//! session logs. The retention reaper deletes closed session trees whose
//! deadline passed, and the blob sweeper frees the blobs nothing references
//! any more.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use api_projection::{MAX_EVENT_PAGE_LIMIT, read_all_session_entries, replay_core_agent_state};
use async_trait::async_trait;
use engine::{
    BlobRef, CoreAgentAction, CoreAgentCommand, CoreAgentDrive, CoreAgentState, CoreAgentStatus,
    Promise, PromiseId, PromiseResolution, PromiseScope, PromiseSource, SessionId,
    storage::{
        AppendSessionEvents, DeleteClosedSessions, ListSessions, SessionListCursor, SessionRecord,
        SessionStore, SessionStoreError, engine_blob_refs,
    },
};
use store_pg::{
    CasObjectDeletion, CasSweepCandidate, CasSweepCursor, CasSweepError, CasSweepLeader,
    CasSweepPage, PgStore,
};
use temporal_workflow::{AgentAdmission, AgentSessionWorkflow, compose_workflow_id};
use temporalio_client::{
    Client, WorkflowDescribeOptions, WorkflowSignalOptions, errors::WorkflowInteractionError,
};
use temporalio_common::protos::temporal::api::enums::v1::WorkflowExecutionStatus;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    config::DeploymentStores,
    session_deletion::{SessionDeletionCause, delete_session_subtree},
};

const DEFAULT_REAPER_INTERVAL: Duration = Duration::from_secs(5 * 60);
const SESSION_PAGE_LIMIT: usize = 256;
const CAS_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Bound each database scan/delete independently of the hourly allowance.
const CAS_SWEEP_PAGE_LIMIT: usize = 1024;
/// Catalog rows examined per universe and pass, including live blobs.
const CAS_SWEEP_ROW_LIMIT: usize = 100_000;
const CAS_SWEEP_PASS_BUDGET: Duration = Duration::from_secs(10 * 60);

/// Periodic collector of content-addressed blobs nothing references.
///
/// One leader scans bounded catalog pages, preserving cursors between passes.
/// FK-backed holders protect concurrent attachments. Physical object keys are
/// unique per catalog incarnation, so delayed object cleanup cannot hurt a put.
#[derive(Clone)]
pub struct CasBlobSweeper {
    progress: Arc<tokio::sync::Mutex<CasSweepProgress>>,
    stores: DeploymentStores,
    grace: Duration,
    interval: Duration,
}

#[derive(Default)]
struct CasSweepProgress {
    cursors: BTreeMap<Uuid, Option<CasSweepCursor>>,
    last_universe: Option<Uuid>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CasSweepStats {
    pub rows_scanned: usize,
    pub leader_busy: bool,
    pub universes_scanned: usize,
    pub candidates: usize,
    pub rows_deleted: usize,
    pub bytes_freed: u64,
    pub objects_deleted: usize,
    pub object_errors: usize,
    pub holder_conflicts: usize,
    pub errors: usize,
}

impl CasSweepStats {
    fn reportable(&self) -> bool {
        self.rows_scanned > 0 || self.holder_conflicts > 0 || self.errors > 0
    }
}

impl CasBlobSweeper {
    pub fn new(stores: DeploymentStores, grace: Duration) -> Self {
        Self {
            progress: Arc::default(),
            stores,
            grace,
            interval: CAS_SWEEP_INTERVAL,
        }
    }

    pub fn grace(&self) -> Duration {
        self.grace
    }

    pub async fn run_forever(self) {
        loop {
            match CasSweepLeader::try_acquire(self.stores.pool()).await {
                Ok(Some(mut leader)) => loop {
                    match self.run_pass(false, Some(&mut leader)).await {
                        Ok(stats) if stats.reportable() => tracing::info!(
                            target: "temporal_server",
                            universes_scanned = stats.universes_scanned,
                            rows_scanned = stats.rows_scanned,
                            candidates = stats.candidates,
                            rows_deleted = stats.rows_deleted,
                            bytes_freed = stats.bytes_freed,
                            objects_deleted = stats.objects_deleted,
                            object_errors = stats.object_errors,
                            holder_conflicts = stats.holder_conflicts,
                            errors = stats.errors,
                            "cas blob sweep pass complete"
                        ),
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(target: "temporal_server", %error, "cas blob sweep leader failed");
                            break;
                        }
                    }
                    tokio::time::sleep(self.interval).await;
                },
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(target: "temporal_server", %error, "could not acquire cas sweep leadership")
                }
            }
            tokio::time::sleep(self.interval).await;
        }
    }

    /// One bounded pass. Manual deletion yields to an active background
    /// leader; dry-run remains available and never advances sweep cursors.
    pub async fn run_once(&self, dry_run: bool) -> anyhow::Result<CasSweepStats> {
        if dry_run {
            return self.run_pass(true, None).await;
        }
        let Some(mut leader) = CasSweepLeader::try_acquire(self.stores.pool()).await? else {
            return Ok(CasSweepStats {
                leader_busy: true,
                ..Default::default()
            });
        };
        self.run_pass(false, Some(&mut leader)).await
    }

    async fn run_pass(
        &self,
        dry_run: bool,
        mut leader: Option<&mut CasSweepLeader>,
    ) -> anyhow::Result<CasSweepStats> {
        let mut universes = store_pg::list_universes(self.stores.pool()).await?;
        universes.sort_by_key(|(id, _)| *id);
        let mut progress = self.progress.lock().await;
        progress
            .cursors
            .retain(|id, _| universes.binary_search_by_key(id, |(id, _)| *id).is_ok());
        if let Some(last) = progress.last_universe {
            let offset = universes.partition_point(|(id, _)| *id <= last);
            universes.rotate_left(offset);
        }
        let deadline = std::time::Instant::now() + CAS_SWEEP_PASS_BUDGET;
        let cutoff_ms = now_ms().saturating_sub(duration_ms(self.grace));
        let pinned = engine_blob_refs();
        let mut stats = CasSweepStats::default();
        for (universe_id, _) in universes {
            if std::time::Instant::now() >= deadline {
                break;
            }
            if let Some(leader) = leader.as_mut() {
                leader.check().await?;
            }
            let store = self.stores.store_for(universe_id);
            let mut cursor = progress.cursors.get(&universe_id).cloned().flatten();
            sweep_universe_pass(
                universe_id,
                store.as_ref(),
                cutoff_ms,
                &pinned,
                CAS_SWEEP_ROW_LIMIT,
                dry_run,
                deadline,
                &mut stats,
                &mut cursor,
            )
            .await;
            if !dry_run {
                progress.cursors.insert(universe_id, cursor);
                progress.last_universe = Some(universe_id);
            }
        }
        Ok(stats)
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The store operations one sweep pass needs, so the pass logic is testable
/// against an in-memory double. The Postgres store is the only production
/// implementation.
#[async_trait]
pub(super) trait CasSweepStore: Send + Sync {
    async fn scan_sweep_candidates(
        &self,
        cutoff_ms: u64,
        pinned: &[BlobRef],
        after: Option<&CasSweepCursor>,
        limit: usize,
    ) -> Result<CasSweepPage, CasSweepError>;

    async fn delete_dead_blobs(
        &self,
        candidates: &[BlobRef],
        cutoff_ms: u64,
        pinned: &[BlobRef],
    ) -> Result<Vec<CasSweepCandidate>, CasSweepError>;

    async fn delete_blob_objects(&self, keys: &[String]) -> CasObjectDeletion;
}

#[async_trait]
impl CasSweepStore for PgStore {
    async fn scan_sweep_candidates(
        &self,
        cutoff_ms: u64,
        pinned: &[BlobRef],
        after: Option<&CasSweepCursor>,
        limit: usize,
    ) -> Result<CasSweepPage, CasSweepError> {
        PgStore::scan_sweep_candidates(self, cutoff_ms, pinned, after, limit).await
    }

    async fn delete_dead_blobs(
        &self,
        candidates: &[BlobRef],
        cutoff_ms: u64,
        pinned: &[BlobRef],
    ) -> Result<Vec<CasSweepCandidate>, CasSweepError> {
        PgStore::delete_dead_blobs(self, candidates, cutoff_ms, pinned).await
    }

    async fn delete_blob_objects(&self, keys: &[String]) -> CasObjectDeletion {
        PgStore::delete_blob_objects(self, keys).await
    }
}

/// Scan multiple small pages without wrapping in the same pass. All universes
/// share the pass deadline; unfinished cursors resume on the next hourly pass.
#[allow(clippy::too_many_arguments)]
async fn sweep_universe_pass(
    universe_id: Uuid,
    store: &dyn CasSweepStore,
    cutoff_ms: u64,
    pinned: &[BlobRef],
    row_limit: usize,
    dry_run: bool,
    deadline: std::time::Instant,
    stats: &mut CasSweepStats,
    cursor: &mut Option<CasSweepCursor>,
) {
    stats.universes_scanned += 1;
    let initial_rows_scanned = stats.rows_scanned;
    loop {
        let remaining = row_limit.saturating_sub(stats.rows_scanned - initial_rows_scanned);
        if remaining == 0 || std::time::Instant::now() >= deadline {
            break;
        }
        if !sweep_universe_page(
            universe_id,
            store,
            cutoff_ms,
            pinned,
            remaining.min(CAS_SWEEP_PAGE_LIMIT),
            dry_run,
            stats,
            cursor,
        )
        .await
        {
            break;
        }
    }
}

/// Returns whether another page may be processed in this pass. Errors stop
/// the universe so a failed dependency cannot consume the whole row allowance.
#[allow(clippy::too_many_arguments)]
async fn sweep_universe_page(
    universe_id: Uuid,
    store: &dyn CasSweepStore,
    cutoff_ms: u64,
    pinned: &[BlobRef],
    limit: usize,
    dry_run: bool,
    stats: &mut CasSweepStats,
    cursor: &mut Option<CasSweepCursor>,
) -> bool {
    let page = match store
        .scan_sweep_candidates(cutoff_ms, pinned, cursor.as_ref(), limit)
        .await
    {
        Ok(page) => page,
        Err(error) => {
            stats.errors += 1;
            tracing::warn!(
                target: "temporal_server",
                %universe_id,
                %error,
                "could not list cas sweep candidates"
            );
            return false;
        }
    };
    stats.rows_scanned += page.scanned;
    *cursor = page.next_cursor;
    let candidates = page.candidates;
    stats.candidates += candidates.len();
    if dry_run {
        stats.bytes_freed += candidates
            .iter()
            .map(|candidate| candidate.byte_len)
            .sum::<u64>();
        return cursor.is_some();
    }
    if candidates.is_empty() {
        return cursor.is_some();
    }
    let candidate_refs = candidates
        .iter()
        .map(|candidate| candidate.blob_ref.clone())
        .collect::<Vec<_>>();
    let deleted = match store
        .delete_dead_blobs(&candidate_refs, cutoff_ms, pinned)
        .await
    {
        Ok(deleted) => deleted,
        Err(CasSweepError::HolderConflict {
            constraint,
            message,
        }) => {
            // A concurrent attachment or an uncovered holder won. Leave
            // this page alone and revisit it after the cursor wraps.
            stats.holder_conflicts += 1;
            tracing::warn!(
                target: "temporal_server",
                %universe_id,
                constraint,
                message,
                "cas sweep skipped a page: blob deletion conflicts with a holder"
            );
            return false;
        }
        Err(error) => {
            stats.errors += 1;
            tracing::warn!(
                target: "temporal_server",
                %universe_id,
                %error,
                "could not delete dead cas blobs"
            );
            return false;
        }
    };
    stats.rows_deleted += deleted.len();
    stats.bytes_freed += deleted
        .iter()
        .map(|candidate| candidate.byte_len)
        .sum::<u64>();
    let object_keys = deleted
        .into_iter()
        .filter_map(|candidate| candidate.object_key)
        .collect::<Vec<_>>();
    if object_keys.is_empty() {
        return cursor.is_some();
    }
    let objects = store.delete_blob_objects(&object_keys).await;
    stats.objects_deleted += objects.deleted;
    stats.object_errors += objects.failures.len();
    let objects_succeeded = objects.failures.is_empty();
    for (key, error) in objects.failures {
        tracing::warn!(
            target: "temporal_server",
            %universe_id,
            key,
            error,
            "could not delete swept blob object; the object is unreachable and leaks"
        );
    }
    objects_succeeded && cursor.is_some()
}

#[cfg(test)]
async fn sweep_universe_once(
    universe_id: Uuid,
    store: &dyn CasSweepStore,
    cutoff_ms: u64,
    pinned: &[BlobRef],
    limit: usize,
    dry_run: bool,
    stats: &mut CasSweepStats,
) {
    sweep_universe_pass(
        universe_id,
        store,
        cutoff_ms,
        pinned,
        limit,
        dry_run,
        std::time::Instant::now() + CAS_SWEEP_PASS_BUDGET,
        stats,
        &mut None,
    )
    .await;
}

#[derive(Clone)]
pub struct SessionRetentionReaper {
    stores: DeploymentStores,
    interval: Duration,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionRetentionReaperStats {
    pub universes_scanned: usize,
    pub due_roots_scanned: usize,
    pub roots_deleted: usize,
    pub sessions_deleted: usize,
    pub open_tree_skips: usize,
    pub conflicts: usize,
    pub errors: usize,
}

impl SessionRetentionReaperStats {
    fn reportable(&self) -> bool {
        self.due_roots_scanned > 0 || self.errors > 0
    }
}

impl SessionRetentionReaper {
    pub fn new(stores: DeploymentStores) -> Self {
        Self {
            stores,
            interval: DEFAULT_REAPER_INTERVAL,
        }
    }

    pub async fn run_forever(self) {
        loop {
            match self.run_once().await {
                Ok(stats) if stats.reportable() => tracing::info!(
                    target: "temporal_server",
                    universes_scanned = stats.universes_scanned,
                    due_roots_scanned = stats.due_roots_scanned,
                    roots_deleted = stats.roots_deleted,
                    sessions_deleted = stats.sessions_deleted,
                    open_tree_skips = stats.open_tree_skips,
                    conflicts = stats.conflicts,
                    errors = stats.errors,
                    "session retention reaper pass complete"
                ),
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    target: "temporal_server",
                    %error,
                    "session retention reaper pass failed"
                ),
            }
            tokio::time::sleep(self.interval).await;
        }
    }

    pub async fn run_once(&self) -> anyhow::Result<SessionRetentionReaperStats> {
        let universes = store_pg::list_universes(self.stores.pool()).await?;
        let mut stats = SessionRetentionReaperStats::default();
        let now_ms = now_ms();
        for (universe_id, _) in universes {
            stats.universes_scanned += 1;
            let store = self.stores.store_for(universe_id);
            let due = match store
                .list_retention_roots_due_for_deletion(now_ms, SESSION_PAGE_LIMIT)
                .await
            {
                Ok(due) => due,
                Err(error) => {
                    stats.errors += 1;
                    tracing::warn!(
                        target: "temporal_server",
                        %universe_id,
                        %error,
                        "could not list due session-retention roots"
                    );
                    continue;
                }
            };
            stats.due_roots_scanned += due.len();
            for root in due {
                let result = delete_session_subtree(
                    store.as_ref(),
                    DeleteClosedSessions {
                        session_id: root.session_id.clone(),
                        cascade: true,
                        due_at_or_before_ms: Some(now_ms),
                    },
                    SessionDeletionCause::Retention,
                )
                .await;
                match result {
                    Ok(deleted) => {
                        stats.roots_deleted += 1;
                        stats.sessions_deleted += deleted.deleted_session_ids.len();
                    }
                    Err(SessionStoreError::SessionTreeNotClosed { .. })
                    | Err(SessionStoreError::SessionNotClosed { .. }) => {
                        stats.open_tree_skips += 1;
                    }
                    Err(SessionStoreError::SessionRetentionNotDue { .. })
                    | Err(SessionStoreError::SessionNotFound { .. }) => {
                        stats.conflicts += 1;
                    }
                    Err(error) => {
                        stats.errors += 1;
                        tracing::warn!(
                            target: "temporal_server",
                            %universe_id,
                            session_id = %root.session_id,
                            %error,
                            "could not delete due session-retention tree"
                        );
                    }
                }
            }
        }
        Ok(stats)
    }
}

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
    pub stale_active_projections: usize,
    pub workflow_status_errors: usize,
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
        self.stale_active_projections += other.stale_active_projections;
        self.workflow_status_errors += other.workflow_status_errors;
        self.conflicts += other.conflicts;
        self.errors += other.errors;
    }

    fn repaired_anything(&self) -> bool {
        self.holder_repairs_signalled > 0 || self.holder_repairs_appended > 0
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
                    if stats.repaired_anything()
                        || stats.stale_active_projections > 0
                        || stats.workflow_status_errors > 0
                        || stats.errors > 0
                        || stats.conflicts > 0 =>
                {
                    tracing::info!(
                        target: "temporal_server",
                        universes_scanned = stats.universes_scanned,
                        sessions_scanned = stats.sessions_scanned,
                        promises_examined = stats.promises_examined,
                        holder_repairs_signalled = stats.holder_repairs_signalled,
                        holder_repairs_appended = stats.holder_repairs_appended,
                        stale_active_projections = stats.stale_active_projections,
                        workflow_status_errors = stats.workflow_status_errors,
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
            let sessions: Arc<dyn SessionStore> = store.clone();
            let append_store: Arc<dyn SessionStore> = store;
            let universe_stats = reap_universe_once(
                universe_id,
                sessions,
                append_store,
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
}

pub(super) async fn reap_universe_once(
    universe_id: Uuid,
    sessions: Arc<dyn SessionStore>,
    append_store: Arc<dyn SessionStore>,
    workflows: Arc<dyn WorkflowRepairClient>,
    now_ms: u64,
) -> anyhow::Result<ReaperStats> {
    let snapshots = load_session_snapshots(sessions.as_ref()).await?;
    let mut workflow_status_cache = BTreeMap::<SessionId, SessionWorkflowStatus>::new();
    let mut stats = ReaperStats {
        universes_scanned: 1,
        sessions_scanned: snapshots.len(),
        ..ReaperStats::default()
    };
    observe_active_projection_statuses(
        universe_id,
        &snapshots,
        workflows.as_ref(),
        &mut workflow_status_cache,
        &mut stats,
    )
    .await;
    let plan = plan_repair(&snapshots, now_ms, &mut stats);

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
    Ok(stats)
}

fn plan_repair(
    snapshots: &BTreeMap<SessionId, LoadedSessionSnapshot>,
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
                continue;
            }

            if let Some(resolution) = promise_source_resolution(&promise.source, now_ms) {
                plan_holder_resolution(
                    &mut plan,
                    holder_session_id,
                    promise.promise_id.clone(),
                    resolution,
                );
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

fn promise_source_resolution(source: &PromiseSource, now_ms: u64) -> Option<PromiseResolution> {
    match source {
        PromiseSource::Timer { fire_at_ms } => {
            (*fire_at_ms <= now_ms).then_some(PromiseResolution::Resolved { payload_ref: None })
        }
        // Only the exact stored producer may resolve a workflow-tool
        // promise; the reaper cannot poll or repair it. Terminal delivery
        // failure and session close remain the unresolvable-promise
        // backstops.
        PromiseSource::Workflow { .. } => None,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SessionWorkflowStatus {
    Running,
    Terminal(WorkflowExecutionStatus),
    NotFound,
    Unavailable(String),
}

async fn workflow_status_cached(
    workflows: &dyn WorkflowRepairClient,
    cache: &mut BTreeMap<SessionId, SessionWorkflowStatus>,
    universe_id: Uuid,
    session_id: &SessionId,
) -> SessionWorkflowStatus {
    if let Some(status) = cache.get(session_id) {
        return status.clone();
    }
    let status = workflows.workflow_status(universe_id, session_id).await;
    cache.insert(session_id.clone(), status.clone());
    status
}

async fn observe_active_projection_statuses(
    universe_id: Uuid,
    snapshots: &BTreeMap<SessionId, LoadedSessionSnapshot>,
    workflows: &dyn WorkflowRepairClient,
    cache: &mut BTreeMap<SessionId, SessionWorkflowStatus>,
    stats: &mut ReaperStats,
) {
    for (session_id, snapshot) in snapshots {
        let active_run_id = snapshot
            .state
            .runs
            .active
            .as_ref()
            .map(|run| run.run_id)
            .or_else(|| snapshot.state.runs.queued.first().map(|run| run.run_id));
        let Some(active_run_id) = active_run_id else {
            continue;
        };

        let workflow_id = compose_workflow_id(universe_id, session_id);
        let session_head_seq = snapshot
            .record
            .head
            .as_ref()
            .map(|head| head.seq.to_string())
            .unwrap_or_default();
        match workflow_status_cached(workflows, cache, universe_id, session_id).await {
            SessionWorkflowStatus::Running => {}
            SessionWorkflowStatus::Terminal(status) => {
                stats.stale_active_projections += 1;
                tracing::error!(
                    target: "temporal_server",
                    event = "stale_active_projection",
                    %universe_id,
                    %session_id,
                    %workflow_id,
                    lightspeed_run_id = active_run_id.as_u64(),
                    session_head_seq,
                    temporal_status = status.as_str_name(),
                    "session projection is active but its workflow is terminal"
                );
            }
            SessionWorkflowStatus::NotFound => {
                stats.stale_active_projections += 1;
                tracing::error!(
                    target: "temporal_server",
                    event = "stale_active_projection",
                    %universe_id,
                    %session_id,
                    %workflow_id,
                    lightspeed_run_id = active_run_id.as_u64(),
                    session_head_seq,
                    temporal_status = "not_found",
                    "session projection is active but its workflow was not found"
                );
            }
            SessionWorkflowStatus::Unavailable(error) => {
                stats.workflow_status_errors += 1;
                tracing::warn!(
                    target: "temporal_server",
                    event = "session_workflow_status_unavailable",
                    %universe_id,
                    %session_id,
                    %workflow_id,
                    lightspeed_run_id = active_run_id.as_u64(),
                    session_head_seq,
                    %error,
                    "could not verify active session projection against Temporal"
                );
            }
        }
    }
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
                ..Default::default()
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

    async fn workflow_status(
        &self,
        universe_id: Uuid,
        session_id: &SessionId,
    ) -> SessionWorkflowStatus;
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

    async fn workflow_status(
        &self,
        universe_id: Uuid,
        session_id: &SessionId,
    ) -> SessionWorkflowStatus {
        let workflow_id = compose_workflow_id(universe_id, session_id);
        match self
            .client
            .get_workflow_handle::<AgentSessionWorkflow>(workflow_id)
            .describe(WorkflowDescribeOptions::default())
            .await
        {
            Ok(description) if description.status() == WorkflowExecutionStatus::Running => {
                SessionWorkflowStatus::Running
            }
            Ok(description) => SessionWorkflowStatus::Terminal(description.status()),
            Err(WorkflowInteractionError::NotFound(_)) => SessionWorkflowStatus::NotFound,
            Err(error) => SessionWorkflowStatus::Unavailable(error.to_string()),
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
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use engine::{
        ActiveRun, ModelSelection, ProviderApiKind, RunId, RunSource, RunStatus,
        storage::{BlobGraphStore as _, BlobStore as _},
    };
    use temporal_workflow::{DEFAULT_MODEL, default_run_config, default_session_config};

    use super::*;

    #[derive(Default)]
    struct FakeWorkflows {
        running: BTreeSet<SessionId>,
        statuses: BTreeMap<SessionId, SessionWorkflowStatus>,
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

        async fn workflow_status(
            &self,
            _universe_id: Uuid,
            session_id: &SessionId,
        ) -> SessionWorkflowStatus {
            if let Some(status) = self.statuses.get(session_id) {
                status.clone()
            } else if self.running.contains(session_id) {
                SessionWorkflowStatus::Running
            } else {
                SessionWorkflowStatus::NotFound
            }
        }
    }

    #[tokio::test]
    async fn terminal_workflow_with_active_run_is_observed_as_stale() {
        let universe_id = Uuid::new_v4();
        let session_id = SessionId::new("stale");
        let mut state = open_state();
        state.runs.active = Some(active_run(RunId::new(7)));
        let snapshots = snapshots([(session_id.clone(), state)]);
        let workflows = FakeWorkflows {
            statuses: BTreeMap::from([(
                session_id,
                SessionWorkflowStatus::Terminal(WorkflowExecutionStatus::Failed),
            )]),
            ..FakeWorkflows::default()
        };
        let mut cache = BTreeMap::new();
        let mut stats = ReaperStats::default();

        observe_active_projection_statuses(
            universe_id,
            &snapshots,
            &workflows,
            &mut cache,
            &mut stats,
        )
        .await;

        assert_eq!(stats.stale_active_projections, 1);
        assert_eq!(stats.workflow_status_errors, 0);
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
            source: RunSource::Input { input: Vec::new() },
            input_entry_ids: Vec::new(),
            input_consumed_by_turn_id: None,
            run_config: default_run_config(),
            config_revision: 0,
            first_seq: engine::EventSeq::new(1),
            accepted_at_ms: 1,
            started_at_ms: Some(1),
            usage: None,
            steering: Vec::new(),
            turns: BTreeMap::new(),
            active_turn_id: None,
            active_tool_batch_id: None,
            approvals: Default::default(),
            parked_tool_batch: None,
            tool_batches: BTreeMap::new(),
            completed_tool_batches: BTreeMap::new(),
            output: None,
            failure: None,
            notify_on_terminal: Vec::new(),
        }
    }

    /// Sweep double over the in-memory blob store: `live` refs stand in for
    /// every holder kind, recorded edges protect children exactly like the
    /// catalog's incoming-edge check, and the object-backed set plus failure
    /// list drive the object phase.
    #[derive(Default)]
    struct FakeSweepStore {
        blobs: engine::storage::InMemoryBlobStore,
        live: Mutex<BTreeSet<BlobRef>>,
        object_backed: BTreeSet<BlobRef>,
        failing_objects: BTreeSet<String>,
        holder_conflict: bool,
        deleted_objects: Mutex<Vec<String>>,
        scan_limits: Mutex<Vec<usize>>,
    }

    impl FakeSweepStore {
        fn dead_candidates(
            &self,
            cutoff_ms: u64,
            pinned: &[BlobRef],
            only: Option<&[BlobRef]>,
        ) -> Vec<CasSweepCandidate> {
            let live = self.live.lock().expect("live lock");
            let children: BTreeSet<BlobRef> = self
                .blobs
                .edges()
                .into_iter()
                .map(|edge| edge.child)
                .collect();
            self.blobs
                .blobs_touched_before(cutoff_ms)
                .into_iter()
                .filter(|info| !pinned.contains(&info.blob_ref))
                .filter(|info| !live.contains(&info.blob_ref))
                .filter(|info| !children.contains(&info.blob_ref))
                .filter(|info| only.is_none_or(|only| only.contains(&info.blob_ref)))
                .map(|info| CasSweepCandidate {
                    object_key: self
                        .object_backed
                        .contains(&info.blob_ref)
                        .then(|| format!("objects/{}", info.blob_ref)),
                    blob_ref: info.blob_ref,
                    byte_len: info.byte_len,
                })
                .collect()
        }
    }

    #[async_trait]
    impl CasSweepStore for FakeSweepStore {
        async fn scan_sweep_candidates(
            &self,
            cutoff_ms: u64,
            pinned: &[BlobRef],
            after: Option<&CasSweepCursor>,
            limit: usize,
        ) -> Result<CasSweepPage, CasSweepError> {
            self.scan_limits
                .lock()
                .expect("scan limits lock")
                .push(limit);
            let scanned: Vec<_> = self
                .blobs
                .blobs_touched_before(cutoff_ms)
                .into_iter()
                .filter(|info| {
                    after.is_none_or(|after| {
                        (
                            self.blobs.touched_at_ms(&info.blob_ref).unwrap(),
                            &info.blob_ref,
                        ) > (after.touched_at_ms, &after.blob_ref)
                    })
                })
                .take(limit)
                .map(|info| info.blob_ref)
                .collect();
            let next_cursor = if scanned.len() == limit {
                scanned.last().map(|blob_ref| CasSweepCursor {
                    touched_at_ms: self.blobs.touched_at_ms(blob_ref).unwrap(),
                    blob_ref: blob_ref.clone(),
                })
            } else {
                None
            };
            Ok(CasSweepPage {
                candidates: self.dead_candidates(cutoff_ms, pinned, Some(&scanned)),
                scanned: scanned.len(),
                next_cursor,
            })
        }

        async fn delete_dead_blobs(
            &self,
            candidates: &[BlobRef],
            cutoff_ms: u64,
            pinned: &[BlobRef],
        ) -> Result<Vec<CasSweepCandidate>, CasSweepError> {
            if self.holder_conflict {
                return Err(CasSweepError::HolderConflict {
                    constraint: "future_holder_digest_fkey".to_owned(),
                    message: "violates foreign key".to_owned(),
                });
            }
            let deleted = self.dead_candidates(cutoff_ms, pinned, Some(candidates));
            let refs = deleted
                .iter()
                .map(|candidate| candidate.blob_ref.clone())
                .collect::<Vec<_>>();
            self.blobs.delete_blobs(&refs);
            Ok(deleted)
        }

        async fn delete_blob_objects(&self, keys: &[String]) -> CasObjectDeletion {
            let mut outcome = CasObjectDeletion::default();
            for key in keys {
                if self.failing_objects.contains(key) {
                    outcome
                        .failures
                        .push((key.clone(), "service unavailable".to_owned()));
                } else {
                    outcome.deleted += 1;
                    self.deleted_objects
                        .lock()
                        .expect("objects lock")
                        .push(key.clone());
                }
            }
            outcome
        }
    }

    async fn aged_blob(store: &FakeSweepStore, content: &[u8]) -> BlobRef {
        store.blobs.put_bytes(content.to_vec()).await.expect("put")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cas_sweep_deletes_only_dead_aged_blobs_and_drains_children_over_passes() {
        let now = Arc::new(std::sync::atomic::AtomicU64::new(1_000));
        let clock_now = now.clone();
        let mut store = FakeSweepStore {
            blobs: engine::storage::InMemoryBlobStore::with_clock(Arc::new(move || {
                clock_now.load(std::sync::atomic::Ordering::SeqCst)
            })),
            ..FakeSweepStore::default()
        };
        let dead = aged_blob(&store, b"dead payload").await;
        let dead_object = aged_blob(&store, b"dead object payload").await;
        let live = aged_blob(&store, b"held by a session").await;
        let parent = aged_blob(&store, b"manifest").await;
        let child = aged_blob(&store, b"file inside the manifest").await;
        let pinned = engine_blob_refs();
        for content in engine::storage::ENGINE_BLOB_CONTENTS {
            aged_blob(&store, content.as_bytes()).await;
        }
        store
            .blobs
            .record_blob_edges(vec![engine::storage::BlobEdge::contains(
                parent.clone(),
                child.clone(),
            )])
            .await
            .expect("edge");
        store.live.lock().expect("live lock").insert(live.clone());
        store.object_backed.insert(dead_object.clone());
        // Touched inside the grace: not a candidate however unreferenced.
        now.store(5_000, std::sync::atomic::Ordering::SeqCst);
        let fresh = aged_blob(&store, b"fresh upload").await;
        let cutoff_ms = 2_000;
        let universe_id = Uuid::new_v4();

        let mut dry = CasSweepStats::default();
        sweep_universe_once(
            universe_id,
            &store,
            cutoff_ms,
            &pinned,
            1024,
            true,
            &mut dry,
        )
        .await;
        assert_eq!(dry.candidates, 3, "dead, dead object, and the parent");
        assert_eq!(dry.rows_deleted, 0);
        assert_eq!(
            dry.bytes_freed,
            (b"dead payload".len() + b"dead object payload".len() + b"manifest".len()) as u64
        );
        assert!(
            store.blobs.has_blob(&dead).await.expect("has"),
            "dry run deletes nothing"
        );

        let mut first = CasSweepStats::default();
        sweep_universe_once(
            universe_id,
            &store,
            cutoff_ms,
            &pinned,
            1024,
            false,
            &mut first,
        )
        .await;
        assert_eq!(first.candidates, 3);
        assert_eq!(first.rows_deleted, 3);
        assert_eq!(
            first.bytes_freed, dry.bytes_freed,
            "dry run and real pass agree"
        );
        assert_eq!(first.objects_deleted, 1);
        assert_eq!(
            store
                .deleted_objects
                .lock()
                .expect("objects lock")
                .as_slice(),
            &[format!("objects/{dead_object}")]
        );
        assert!(!store.blobs.has_blob(&dead).await.expect("has"));
        assert!(!store.blobs.has_blob(&parent).await.expect("has"));
        assert!(store.blobs.has_blob(&live).await.expect("has"));
        assert!(store.blobs.has_blob(&fresh).await.expect("has"));
        assert!(
            store.blobs.has_blob(&child).await.expect("has"),
            "a child survives the pass that deletes its parent"
        );
        for pinned_ref in &pinned {
            assert!(store.blobs.has_blob(pinned_ref).await.expect("has"));
        }

        let mut second = CasSweepStats::default();
        sweep_universe_once(
            universe_id,
            &store,
            cutoff_ms,
            &pinned,
            1024,
            false,
            &mut second,
        )
        .await;
        assert_eq!(second.rows_deleted, 1, "the exposed child drains next");
        assert!(!store.blobs.has_blob(&child).await.expect("has"));

        let mut third = CasSweepStats::default();
        sweep_universe_once(
            universe_id,
            &store,
            cutoff_ms,
            &pinned,
            1024,
            false,
            &mut third,
        )
        .await;
        assert_eq!(third.candidates, 0, "a repeated sweep is a no-op");
        assert!(third.rows_scanned > 0);
        assert!(third.reportable(), "live pages still report examined rows");

        store.live.lock().expect("live lock").clear();
        let mut released = CasSweepStats::default();
        sweep_universe_once(
            universe_id,
            &store,
            cutoff_ms,
            &pinned,
            1024,
            false,
            &mut released,
        )
        .await;
        assert_eq!(
            released.rows_deleted, 1,
            "a blob whose last holder went is collected"
        );
        assert!(!store.blobs.has_blob(&live).await.expect("has"));
        store.holder_conflict = false;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cas_sweep_counts_holder_conflicts_and_object_failures_without_failing_the_pass() {
        let store = FakeSweepStore {
            blobs: engine::storage::InMemoryBlobStore::with_clock(Arc::new(|| 10)),
            holder_conflict: true,
            ..FakeSweepStore::default()
        };
        aged_blob(&store, b"would be deleted").await;
        let mut stats = CasSweepStats::default();
        sweep_universe_once(Uuid::new_v4(), &store, 100, &[], 1024, false, &mut stats).await;
        assert_eq!(stats.candidates, 1);
        assert_eq!(stats.holder_conflicts, 1);
        assert_eq!(stats.rows_deleted, 0);
        assert!(stats.reportable());

        let mut store = FakeSweepStore {
            blobs: engine::storage::InMemoryBlobStore::with_clock(Arc::new(|| 10)),
            ..FakeSweepStore::default()
        };
        let failing = aged_blob(&store, b"object whose delete fails").await;
        let fine = aged_blob(&store, b"object whose delete works").await;
        store.object_backed.insert(failing.clone());
        store.object_backed.insert(fine);
        store.failing_objects.insert(format!("objects/{failing}"));
        let mut stats = CasSweepStats::default();
        sweep_universe_once(Uuid::new_v4(), &store, 100, &[], 1024, false, &mut stats).await;
        assert_eq!(stats.rows_deleted, 2, "rows go before objects");
        assert_eq!(stats.objects_deleted, 1);
        assert_eq!(stats.object_errors, 1);
        assert_eq!(stats.errors, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cas_sweep_respects_the_batch_limit() {
        let store = FakeSweepStore {
            blobs: engine::storage::InMemoryBlobStore::with_clock(Arc::new(|| 10)),
            ..FakeSweepStore::default()
        };
        for index in 0..5u8 {
            aged_blob(&store, &[index]).await;
        }
        let mut stats = CasSweepStats::default();
        sweep_universe_once(Uuid::new_v4(), &store, 100, &[], 2, false, &mut stats).await;
        assert_eq!(stats.candidates, 2);
        assert_eq!(stats.rows_deleted, 2);
        assert_eq!(store.blobs.blob_refs().len(), 3);
    }

    async fn paged_sweep_store(count: usize) -> FakeSweepStore {
        let store = FakeSweepStore {
            blobs: engine::storage::InMemoryBlobStore::with_clock(Arc::new(|| 10)),
            ..FakeSweepStore::default()
        };
        for index in 0..count {
            aged_blob(&store, &index.to_le_bytes()).await;
        }
        store
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cas_sweep_pages_past_live_rows_and_resumes_after_its_row_limit() {
        let store = paged_sweep_store(CAS_SWEEP_PAGE_LIMIT + 4).await;
        store.live.lock().expect("live lock").extend(
            store
                .blobs
                .blobs_touched_before(100)
                .into_iter()
                .take(CAS_SWEEP_PAGE_LIMIT)
                .map(|blob| blob.blob_ref),
        );
        let universe_id = Uuid::new_v4();
        let mut cursor = None;
        let mut first = CasSweepStats::default();
        sweep_universe_pass(
            universe_id,
            &store,
            100,
            &[],
            CAS_SWEEP_PAGE_LIMIT + 2,
            false,
            std::time::Instant::now() + CAS_SWEEP_PASS_BUDGET,
            &mut first,
            &mut cursor,
        )
        .await;
        assert_eq!(first.universes_scanned, 1, "count universes, not pages");
        assert_eq!(first.rows_scanned, CAS_SWEEP_PAGE_LIMIT + 2);
        assert_eq!(
            first.rows_deleted, 2,
            "live rows count against the allowance"
        );
        assert!(cursor.is_some());
        assert_eq!(
            *store.scan_limits.lock().unwrap(),
            vec![CAS_SWEEP_PAGE_LIMIT, 2]
        );

        let mut next = CasSweepStats::default();
        sweep_universe_pass(
            universe_id,
            &store,
            100,
            &[],
            CAS_SWEEP_ROW_LIMIT,
            false,
            std::time::Instant::now() + CAS_SWEEP_PASS_BUDGET,
            &mut next,
            &mut cursor,
        )
        .await;
        assert_eq!(next.rows_scanned, 2, "resume beyond the previous live page");
        assert_eq!(next.rows_deleted, 2);
        assert!(
            cursor.is_none(),
            "stop at the end without wrapping this pass"
        );
        assert_eq!(store.blobs.blob_refs().len(), CAS_SWEEP_PAGE_LIMIT);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cas_sweep_dry_run_traverses_multiple_pages_without_deleting_or_wrapping() {
        let count = CAS_SWEEP_PAGE_LIMIT + 3;
        let store = paged_sweep_store(count).await;
        let mut stats = CasSweepStats::default();
        sweep_universe_once(
            Uuid::new_v4(),
            &store,
            100,
            &[],
            CAS_SWEEP_ROW_LIMIT,
            true,
            &mut stats,
        )
        .await;
        assert_eq!(stats.universes_scanned, 1);
        assert_eq!(stats.rows_scanned, count);
        assert_eq!(stats.candidates, count);
        assert_eq!(stats.rows_deleted, 0);
        assert_eq!(store.blobs.blob_refs().len(), count);
        assert_eq!(
            *store.scan_limits.lock().unwrap(),
            vec![CAS_SWEEP_PAGE_LIMIT; 2]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cas_sweep_stops_at_an_expired_deadline_or_a_page_failure() {
        let mut store = paged_sweep_store(CAS_SWEEP_PAGE_LIMIT + 1).await;
        let mut stats = CasSweepStats::default();
        let mut cursor = None;
        sweep_universe_pass(
            Uuid::new_v4(),
            &store,
            100,
            &[],
            CAS_SWEEP_ROW_LIMIT,
            false,
            std::time::Instant::now(),
            &mut stats,
            &mut cursor,
        )
        .await;
        assert_eq!(stats.rows_scanned, 0);
        assert!(store.scan_limits.lock().unwrap().is_empty());
        assert!(cursor.is_none());

        store.holder_conflict = true;
        let mut stats = CasSweepStats::default();
        sweep_universe_once(
            Uuid::new_v4(),
            &store,
            100,
            &[],
            CAS_SWEEP_ROW_LIMIT,
            false,
            &mut stats,
        )
        .await;
        assert_eq!(stats.rows_scanned, CAS_SWEEP_PAGE_LIMIT);
        assert_eq!(stats.holder_conflicts, 1, "stop after the failed page");
        assert_eq!(stats.rows_deleted, 0);
        assert_eq!(
            *store.scan_limits.lock().unwrap(),
            vec![CAS_SWEEP_PAGE_LIMIT]
        );
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
                            metadata: Default::default(),
                            session_id: session_id.clone(),
                            display_name: None,
                            lifecycle_status: engine::storage::SessionLifecycleStatus::New,
                            closed_at_seq: None,
                            closed_at_ms: None,
                            activity: Default::default(),
                            retention_root_session_id: session_id,
                            delete_after_close_ms: None,
                            delete_at_ms: None,
                            managed: false,
                            head: None,
                            source_session_id: None,
                            source_seq: None,
                            origin: None,
                            created_at_ms: 0,
                            updated_at_ms: 0,
                        },
                        state,
                    },
                )
            })
            .collect()
    }
}

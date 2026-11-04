//! Deployment-level background loops of the sessions role: promise/source
//! repair, session retention, and content-addressed blob collection.
//!
//! Workflow-local machinery handles the normal path: terminal runs notify
//! holder workflows, and holder-side promise cancellation flushes source
//! cancellation. The promise reaper is the backstop for promise and source
//! state that no single workflow can repair by itself, including what a
//! terminated workflow leaves behind and what is only visible by scanning
//! session logs. It does not send missed run terminals again: it reports an
//! active run on a stopped workflow, and leaves the session to whoever owns
//! it. The retention reaper soft-deletes closed session trees whose
//! deadline passed, and the blob sweeper frees the blobs nothing references
//! any more.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use anyhow::Context as _;
use async_trait::async_trait;
use harness::{
    BlobRef, CoreAgentAction, CoreAgentCommand, CoreAgentDrive, CoreAgentState, CoreAgentStatus,
    Promise, PromiseId, PromiseResolution, PromiseScope, PromiseSource, RunId, SessionId,
    storage::{
        AppendSessionEvents, BlobStore, DeleteClosedSessions, DeleteClosedSessionsResult,
        ListSessions, RetentionRootCursor, SessionLifecycleStatus, SessionListCursor,
        SessionRecord, SessionStore, SessionStoreError, harness_blob_refs,
    },
};
use store_pg::{
    CasObjectDeletion, CasSweepCandidate, CasSweepCursor, CasSweepError, CasSweepLeader,
    CasSweepPage, PgStore,
};
use temporal_workflow::{AgentAdmission, AgentSessionWorkflow, compose_workflow_id};
use temporalio_client::WorkflowExecutionStatus;
use temporalio_client::{
    Client, WorkflowDescribeOptions, WorkflowSignalOptions, errors::WorkflowInteractionError,
};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    config::DeploymentStores,
    gateway::service::ExecutionLiveness,
    session_deletion::{SessionDeletionCause, delete_session_subtree},
};

const DEFAULT_REAPER_INTERVAL: Duration = Duration::from_secs(5 * 60);
const SESSION_PAGE_LIMIT: usize = 256;
/// Blocked retention roots a warning names; the count carries the rest.
const OPEN_TREE_SAMPLE_LIMIT: usize = 8;
const CAS_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Bound each database scan/delete independently of the hourly allowance.
const CAS_SWEEP_PAGE_LIMIT: usize = 1024;
/// Catalog rows examined per universe and pass, including live blobs.
const CAS_SWEEP_ROW_LIMIT: usize = 100_000;
const CAS_SWEEP_PASS_BUDGET: Duration = Duration::from_secs(10 * 60);

/// Periodic collector of content-addressed blobs nothing references.
///
/// One leader scans bounded catalog pages. Each universe's position is stored, so
/// a restart or a new leader carries on past rows already examined: live rows count
/// against a pass's allowance, and starting over each time would never get past an
/// old live region to the garbage behind it. FK-backed holders protect concurrent attachments. Physical object keys are
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
    last_universe: Option<Uuid>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CasSweepStats {
    pub rows_scanned: usize,
    pub leader_busy: bool,
    pub universes_scanned: usize,
    /// Universes whose walk reached the end of the catalog in this pass.
    pub walks_completed: usize,
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
                            target: "temporal_runtime",
                            universes_scanned = stats.universes_scanned,
                            rows_scanned = stats.rows_scanned,
                            walks_completed = stats.walks_completed,
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
                            tracing::warn!(target: "temporal_runtime", %error, "cas blob sweep leader failed");
                            break;
                        }
                    }
                    tokio::time::sleep(self.interval).await;
                },
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(target: "temporal_runtime", %error, "could not acquire cas sweep leadership")
                }
            }
            tokio::time::sleep(self.interval).await;
        }
    }

    /// One bounded pass. Manual deletion yields to an active background
    /// leader; dry-run remains available, starts at the stored position and
    /// never advances it.
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
        if let Some(last) = progress.last_universe {
            let offset = universes.partition_point(|(id, _)| *id <= last);
            universes.rotate_left(offset);
        }
        let deadline = std::time::Instant::now() + CAS_SWEEP_PASS_BUDGET;
        let cutoff_ms = now_ms().saturating_sub(duration_ms(self.grace));
        let pinned = harness_blob_refs();
        let mut stats = CasSweepStats::default();
        for (universe_id, _) in universes {
            if std::time::Instant::now() >= deadline {
                break;
            }
            if let Some(leader) = leader.as_mut() {
                leader.check().await?;
            }
            let store = self.stores.store_for(universe_id);
            sweep_universe_from_stored_cursor(
                universe_id,
                store.as_ref(),
                cutoff_ms,
                &pinned,
                CAS_SWEEP_ROW_LIMIT,
                dry_run,
                deadline,
                &mut stats,
            )
            .await;
            if !dry_run {
                progress.last_universe = Some(universe_id);
            }
        }
        Ok(stats)
    }
}

/// One universe's pass, resumed from and written back to its stored position. The
/// store holds the only copy, so a leader that lost and regained the lock cannot
/// walk back to where it was before another leader moved on.
#[allow(clippy::too_many_arguments)]
async fn sweep_universe_from_stored_cursor(
    universe_id: Uuid,
    store: &dyn CasSweepStore,
    cutoff_ms: u64,
    pinned: &[BlobRef],
    row_limit: usize,
    dry_run: bool,
    deadline: std::time::Instant,
    stats: &mut CasSweepStats,
) {
    let mut cursor = match store.load_sweep_cursor().await {
        Ok(cursor) => cursor,
        Err(error) => {
            stats.errors += 1;
            tracing::warn!(
                target: "temporal_runtime",
                %universe_id,
                %error,
                "could not load the cas sweep cursor"
            );
            return;
        }
    };
    let rows_scanned_before = stats.rows_scanned;
    sweep_universe_pass(
        universe_id,
        store,
        cutoff_ms,
        pinned,
        row_limit,
        dry_run,
        deadline,
        stats,
        &mut cursor,
    )
    .await;
    let rows_scanned = stats.rows_scanned - rows_scanned_before;
    let wrapped = rows_scanned > 0 && cursor.is_none();
    if wrapped {
        stats.walks_completed += 1;
    }
    if rows_scanned > 0 {
        tracing::info!(
            target: "temporal_runtime",
            %universe_id,
            rows_scanned,
            cursor_touched_at_ms = cursor.as_ref().map(|cursor| cursor.touched_at_ms),
            wrapped,
            dry_run,
            "cas sweep universe pass"
        );
    }
    if !dry_run && let Err(error) = store.save_sweep_cursor(cursor.as_ref()).await {
        stats.errors += 1;
        tracing::warn!(
            target: "temporal_runtime",
            %universe_id,
            %error,
            "could not save the cas sweep cursor"
        );
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

    async fn load_sweep_cursor(&self) -> Result<Option<CasSweepCursor>, CasSweepError>;

    async fn save_sweep_cursor(&self, cursor: Option<&CasSweepCursor>)
    -> Result<(), CasSweepError>;
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

    async fn load_sweep_cursor(&self) -> Result<Option<CasSweepCursor>, CasSweepError> {
        PgStore::load_sweep_cursor(self).await
    }

    async fn save_sweep_cursor(
        &self,
        cursor: Option<&CasSweepCursor>,
    ) -> Result<(), CasSweepError> {
        PgStore::save_sweep_cursor(self, cursor).await
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
                target: "temporal_runtime",
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
                target: "temporal_runtime",
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
                target: "temporal_runtime",
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
            target: "temporal_runtime",
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
    /// Due roots that stayed because a descendant is still open.
    pub open_tree_skips: usize,
    /// The first few of those roots, so an operator can find them.
    pub open_tree_sample: Vec<SessionId>,
    pub conflicts: usize,
    pub errors: usize,
}

impl SessionRetentionReaperStats {
    fn reportable(&self) -> bool {
        self.due_roots_scanned > 0 || self.errors > 0
    }

    fn report_open_tree(&mut self, root: SessionId) {
        self.open_tree_skips += 1;
        if self.open_tree_sample.len() < OPEN_TREE_SAMPLE_LIMIT {
            self.open_tree_sample.push(root);
        }
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
                Ok(stats) => {
                    if stats.reportable() {
                        tracing::info!(
                            target: "temporal_runtime",
                            universes_scanned = stats.universes_scanned,
                            due_roots_scanned = stats.due_roots_scanned,
                            roots_deleted = stats.roots_deleted,
                            sessions_deleted = stats.sessions_deleted,
                            open_tree_skips = stats.open_tree_skips,
                            conflicts = stats.conflicts,
                            errors = stats.errors,
                            "session retention reaper pass complete"
                        );
                    }
                    if stats.open_tree_skips > 0 {
                        tracing::warn!(
                            target: "temporal_runtime",
                            open_tree_skips = stats.open_tree_skips,
                            sample = ?stats.open_tree_sample,
                            "session trees past their retention deadline stay because a descendant is still open; force close it"
                        );
                    }
                }
                Err(error) => tracing::warn!(
                    target: "temporal_runtime",
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
            reap_due_roots(universe_id, store.as_ref(), now_ms, &mut stats).await;
        }
        Ok(stats)
    }
}

/// What a retention pass needs from a store, so the walk is testable against an
/// in-memory double. Postgres is the only production implementation.
#[async_trait]
trait RetentionStore: Send + Sync {
    async fn list_due_roots(
        &self,
        now_ms: u64,
        after: Option<&RetentionRootCursor>,
        limit: usize,
    ) -> Result<Vec<SessionRecord>, SessionStoreError>;

    async fn delete_due_tree(
        &self,
        root: &SessionId,
        now_ms: u64,
    ) -> Result<DeleteClosedSessionsResult, SessionStoreError>;
}

#[async_trait]
impl RetentionStore for PgStore {
    async fn list_due_roots(
        &self,
        now_ms: u64,
        after: Option<&RetentionRootCursor>,
        limit: usize,
    ) -> Result<Vec<SessionRecord>, SessionStoreError> {
        self.list_retention_roots_due_for_deletion(now_ms, after, limit)
            .await
    }

    async fn delete_due_tree(
        &self,
        root: &SessionId,
        now_ms: u64,
    ) -> Result<DeleteClosedSessionsResult, SessionStoreError> {
        delete_session_subtree(
            self,
            DeleteClosedSessions {
                session_id: root.clone(),
                cascade: true,
                due_at_or_before_ms: Some(now_ms),
            },
            SessionDeletionCause::Retention,
            true,
            false,
        )
        .await
    }
}

/// Walks every due root once. The walk moves by position rather than by what got
/// deleted, because a root that stays keeps its deadline: without a position
/// enough of them would fill the page on every pass and hide every later root.
async fn reap_due_roots(
    universe_id: Uuid,
    store: &dyn RetentionStore,
    now_ms: u64,
    stats: &mut SessionRetentionReaperStats,
) {
    let mut after: Option<RetentionRootCursor> = None;
    loop {
        let due = match store
            .list_due_roots(now_ms, after.as_ref(), SESSION_PAGE_LIMIT)
            .await
        {
            Ok(due) => due,
            Err(error) => {
                stats.errors += 1;
                tracing::warn!(
                    target: "temporal_runtime",
                    %universe_id,
                    %error,
                    "could not list due session-retention roots"
                );
                return;
            }
        };
        stats.due_roots_scanned += due.len();
        let last_page = due.len() < SESSION_PAGE_LIMIT;
        after = due.last().and_then(|root| {
            root.delete_at_ms.map(|delete_at_ms| RetentionRootCursor {
                delete_at_ms,
                session_id: root.session_id.clone(),
            })
        });
        for root in due {
            match store.delete_due_tree(&root.session_id, now_ms).await {
                Ok(deleted) => {
                    stats.roots_deleted += 1;
                    stats.sessions_deleted += deleted.deleted_session_ids.len();
                }
                Err(SessionStoreError::SessionTreeNotClosed { .. })
                | Err(SessionStoreError::SessionNotClosed { .. }) => {
                    stats.report_open_tree(root.session_id);
                }
                Err(SessionStoreError::SessionRetentionNotDue { .. })
                | Err(SessionStoreError::SessionNotFound { .. }) => {
                    stats.conflicts += 1;
                }
                Err(error) => {
                    stats.errors += 1;
                    tracing::warn!(
                        target: "temporal_runtime",
                        %universe_id,
                        session_id = %root.session_id,
                        %error,
                        "could not delete due session-retention tree"
                    );
                }
            }
        }
        if last_page || after.is_none() {
            return;
        }
    }
}

#[derive(Clone)]
pub struct PromiseReaper {
    client: Client,
    stores: DeploymentStores,
    interval: Duration,
    signalled: SignalledRepairs,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReaperStats {
    pub universes_scanned: usize,
    pub sessions_scanned: usize,
    pub events_read: u64,
    pub promises_examined: usize,
    pub holder_repairs_signalled: usize,
    /// Repairs signalled again because the previous pass's signal changed nothing.
    pub holder_repairs_repeated: usize,
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
        self.events_read += other.events_read;
        self.promises_examined += other.promises_examined;
        self.holder_repairs_signalled += other.holder_repairs_signalled;
        self.holder_repairs_repeated += other.holder_repairs_repeated;
        self.holder_repairs_appended += other.holder_repairs_appended;
        self.stale_active_projections += other.stale_active_projections;
        self.workflow_status_errors += other.workflow_status_errors;
        self.conflicts += other.conflicts;
        self.errors += other.errors;
    }
}

/// Which repairs were signalled on the previous pass. The plan comes from the
/// log, and a workflow that is not processing admissions accepts signals while
/// its log stands still, so the same repair comes back on every pass. Holding
/// only the last pass's keys keeps this bounded by the repairs in flight.
#[derive(Clone, Debug, Default)]
struct SignalledRepairs {
    previous: BTreeSet<RepairKey>,
    current: BTreeSet<RepairKey>,
}

type RepairKey = (Uuid, SessionId, PromiseId);

impl SignalledRepairs {
    fn start_pass(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }

    /// Records a signal and says whether the previous pass sent the same one.
    fn record(&mut self, key: RepairKey) -> bool {
        let repeated = self.previous.contains(&key);
        self.current.insert(key);
        repeated
    }
}

impl PromiseReaper {
    pub fn new(client: Client, stores: DeploymentStores) -> Self {
        Self {
            client,
            stores,
            interval: DEFAULT_REAPER_INTERVAL,
            signalled: SignalledRepairs::default(),
        }
    }

    pub async fn run_forever(mut self) {
        loop {
            let started = std::time::Instant::now();
            match self.run_once().await {
                Ok(stats) => {
                    tracing::info!(
                        target: "temporal_runtime",
                        duration_ms = started.elapsed().as_millis() as u64,
                        universes_scanned = stats.universes_scanned,
                        sessions_scanned = stats.sessions_scanned,
                        events_read = stats.events_read,
                        promises_examined = stats.promises_examined,
                        holder_repairs_signalled = stats.holder_repairs_signalled,
                        holder_repairs_repeated = stats.holder_repairs_repeated,
                        holder_repairs_appended = stats.holder_repairs_appended,
                        stale_active_projections = stats.stale_active_projections,
                        workflow_status_errors = stats.workflow_status_errors,
                        conflicts = stats.conflicts,
                        errors = stats.errors,
                        "promise reaper pass complete"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        target: "temporal_runtime",
                        error = %format_args!("{error:#}"),
                        "promise reaper pass failed"
                    );
                }
            }
            tokio::time::sleep(self.interval).await;
        }
    }

    pub async fn run_once(&mut self) -> anyhow::Result<ReaperStats> {
        let workflows = TemporalWorkflowRepairClient {
            client: self.client.clone(),
        };
        let universes = store_pg::list_universes(self.stores.pool())
            .await
            .context("list universes for promise repair")?;
        let mut stats = ReaperStats::default();
        self.signalled.start_pass();
        for (universe_id, _) in universes {
            let store = self.stores.store_for(universe_id);
            let universe_stats = reap_universe_once(
                universe_id,
                store.as_ref(),
                store.as_ref(),
                &workflows,
                &mut self.signalled,
                now_ms(),
            )
            .await;
            stats.merge(universe_stats);
        }
        Ok(stats)
    }
}

/// Walks a universe's open sessions one at a time, so at most one reduced state
/// is in memory however many sessions the universe holds. Closed sessions cannot
/// hold pending promises and are not read.
async fn reap_universe_once(
    universe_id: Uuid,
    sessions: &dyn SessionStore,
    blobs: &dyn BlobStore,
    workflows: &dyn WorkflowRepairClient,
    signalled: &mut SignalledRepairs,
    now_ms: u64,
) -> ReaperStats {
    let mut stats = ReaperStats {
        universes_scanned: 1,
        ..ReaperStats::default()
    };
    let mut cursor: Option<SessionListCursor> = None;
    loop {
        let page = match sessions
            .list_sessions(ListSessions {
                cursor,
                limit: SESSION_PAGE_LIMIT,
                closed: Some(false),
                ..Default::default()
            })
            .await
        {
            Ok(page) => page,
            Err(error) => {
                stats.errors += 1;
                tracing::warn!(target: "temporal_runtime", %universe_id, operation = "list_sessions",
                    %error, "promise reaper could not list sessions");
                return stats;
            }
        };
        for record in page.sessions {
            reap_session(
                universe_id,
                &record.session_id,
                sessions,
                blobs,
                workflows,
                signalled,
                now_ms,
                &mut stats,
            )
            .await;
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            return stats;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn reap_session(
    universe_id: Uuid,
    session_id: &SessionId,
    sessions: &dyn SessionStore,
    blobs: &dyn BlobStore,
    workflows: &dyn WorkflowRepairClient,
    signalled: &mut SignalledRepairs,
    now_ms: u64,
    stats: &mut ReaperStats,
) {
    stats.sessions_scanned += 1;
    let mut state = match load_open_state(sessions, blobs, session_id, stats).await {
        Ok(Some(state)) => state,
        Ok(None) => return,
        Err(error) => {
            stats.errors += 1;
            tracing::warn!(target: "temporal_runtime", %universe_id, %session_id,
                error = %format_args!("{error:#}"), "promise reaper skipped unreadable session");
            return;
        }
    };
    if let Some(run_id) = active_or_queued_run(&state) {
        let temporal_status = match workflows.workflow_status(universe_id, session_id).await {
            SessionWorkflowStatus::Running => None,
            SessionWorkflowStatus::Terminal(status) => Some(format!("{status:?}")),
            SessionWorkflowStatus::NotFound => Some("not_found".to_owned()),
            SessionWorkflowStatus::Unavailable(error) => {
                stats.workflow_status_errors += 1;
                tracing::warn!(
                    target: "temporal_runtime",
                    event = "session_workflow_status_unavailable",
                    %universe_id,
                    %session_id,
                    lightspeed_run_id = run_id.as_u64(),
                    %error,
                    "could not verify active session projection against Temporal"
                );
                None
            }
        };
        if let Some(temporal_status) = temporal_status {
            // A stopped execution writes nothing more, so this read sees its
            // final log. A run that ended, or a close that landed, between the
            // first read and the describe is an ordinary exit.
            state = match load_open_state(sessions, blobs, session_id, stats).await {
                Ok(Some(state)) => state,
                Ok(None) => return,
                Err(error) => {
                    stats.workflow_status_errors += 1;
                    tracing::warn!(target: "temporal_runtime", %universe_id, %session_id,
                        error = %format_args!("{error:#}"),
                        "could not recheck a session whose workflow has stopped");
                    return;
                }
            };
            if let Some(run_id) = active_or_queued_run(&state) {
                stats.stale_active_projections += 1;
                tracing::error!(
                    target: "temporal_runtime",
                    event = "stale_active_projection",
                    %universe_id,
                    %session_id,
                    workflow_id = %compose_workflow_id(universe_id, session_id),
                    lightspeed_run_id = run_id.as_u64(),
                    session_head_seq = state.reduced_to.as_ref().map(|head| head.seq.as_u64()),
                    temporal_status,
                    "session log has an active or queued run but its workflow has stopped"
                );
            }
        }
    }
    let repairs = plan_session_repairs(&state, now_ms, stats);
    if !repairs.is_empty() {
        apply_session_repairs(
            universe_id,
            session_id,
            sessions,
            workflows,
            signalled,
            state,
            repairs,
            now_ms,
            stats,
        )
        .await;
    }
}

/// The session's state now, or nothing once it has closed or gone. The record is
/// read here rather than taken from the list page, because a checkpoint newer
/// than a list-time head would be rejected and replace a cheap load with a replay.
async fn load_open_state(
    sessions: &dyn SessionStore,
    blobs: &dyn BlobStore,
    session_id: &SessionId,
    stats: &mut ReaperStats,
) -> anyhow::Result<Option<CoreAgentState>> {
    let Some(record) = sessions.load_session(session_id).await? else {
        return Ok(None);
    };
    if record.lifecycle_status == SessionLifecycleStatus::Closed {
        return Ok(None);
    }
    let loaded = crate::checkpoint::load_reduction(sessions, blobs, &record).await?;
    stats.events_read += loaded.tail_event_count;
    Ok(Some(loaded.reduced.core_state))
}

fn active_or_queued_run(state: &CoreAgentState) -> Option<RunId> {
    state
        .runs
        .active
        .as_ref()
        .map(|run| run.run_id)
        .or_else(|| state.runs.queued.first().map(|run| run.run_id))
}

fn plan_session_repairs(
    state: &CoreAgentState,
    now_ms: u64,
    stats: &mut ReaperStats,
) -> Vec<(PromiseId, PromiseResolution)> {
    let mut repairs = Vec::new();
    for promise in state.promises.pending() {
        stats.promises_examined += 1;
        let resolution = if promise_owner_live(state, promise) {
            promise_source_resolution(&promise.source, now_ms)
        } else {
            Some(PromiseResolution::Cancelled)
        };
        if let Some(resolution) = resolution {
            repairs.push((promise.promise_id.clone(), resolution));
        }
    }
    repairs
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

/// An accepted signal is only a request: the repair has happened once the log
/// no longer plans it. A repeat logs louder and counts apart, and goes out again
/// because a workflow that was merely slow still needs it.
#[allow(clippy::too_many_arguments)]
async fn apply_session_repairs(
    universe_id: Uuid,
    session_id: &SessionId,
    store: &dyn SessionStore,
    workflows: &dyn WorkflowRepairClient,
    signalled: &mut SignalledRepairs,
    state: CoreAgentState,
    repairs: Vec<(PromiseId, PromiseResolution)>,
    now_ms: u64,
    stats: &mut ReaperStats,
) {
    let admissions = repairs
        .iter()
        .map(|(promise_id, resolution)| AgentAdmission {
            command: CoreAgentCommand::ResolvePromise {
                promise_id: promise_id.clone(),
                resolution: resolution.clone(),
            },
            correlation_token: None,
        })
        .collect();
    match workflows
        .signal_admissions(universe_id, session_id, admissions)
        .await
    {
        Ok(()) => {
            for (promise_id, _) in &repairs {
                let key = (universe_id, session_id.clone(), promise_id.clone());
                if signalled.record(key) {
                    stats.holder_repairs_repeated += 1;
                    tracing::warn!(
                        target: "temporal_runtime",
                        event = "promise_repair_repeated",
                        %universe_id,
                        %session_id,
                        %promise_id,
                        "promise repair signalled on the previous pass is still pending; the session workflow may be stalled"
                    );
                } else {
                    stats.holder_repairs_signalled += 1;
                    tracing::info!(
                        target: "temporal_runtime",
                        event = "promise_repair_signalled",
                        %universe_id,
                        %session_id,
                        %promise_id,
                        "promise repair signalled to the session workflow"
                    );
                }
            }
            return;
        }
        Err(WorkflowSignalFailure::NotFound) => {}
        Err(WorkflowSignalFailure::Other(error)) => {
            stats.errors += 1;
            tracing::warn!(
                target: "temporal_runtime",
                %universe_id,
                %session_id,
                %error,
                "promise reaper failed to signal holder repair"
            );
            return;
        }
    }
    let mut drive = CoreAgentDrive::from_replayed(session_id.clone(), state);
    for (promise_id, resolution) in repairs {
        let command = CoreAgentCommand::ResolvePromise {
            promise_id: promise_id.clone(),
            resolution,
        };
        match append_command_direct(store, session_id, &mut drive, command, now_ms).await {
            Ok(true) => {
                stats.holder_repairs_appended += 1;
                tracing::info!(
                    target: "temporal_runtime",
                    event = "promise_repair_appended",
                    %universe_id,
                    %session_id,
                    %promise_id,
                    "promise repair appended for a session with no running workflow"
                );
            }
            Ok(false) => {}
            Err(DirectAppendError::Conflict) => {
                stats.conflicts += 1;
                return;
            }
            Err(DirectAppendError::Other(error)) => {
                stats.errors += 1;
                tracing::warn!(
                    target: "temporal_runtime",
                    %universe_id,
                    %session_id,
                    %promise_id,
                    %error,
                    "promise reaper failed to append holder repair"
                );
                return;
            }
        }
    }
}

#[derive(Debug, Error)]
enum DirectAppendError {
    #[error("expected head conflict")]
    Conflict,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Returns whether the command wrote anything. The drive's state fences the
/// append, so a writer that got in after this state was read wins the conflict.
async fn append_command_direct(
    store: &dyn SessionStore,
    session_id: &SessionId,
    drive: &mut CoreAgentDrive,
    command: CoreAgentCommand,
    now_ms: u64,
) -> Result<bool, DirectAppendError> {
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
                    SessionStoreError::ExpectedHeadMismatch { .. } => DirectAppendError::Conflict,
                    other => DirectAppendError::Other(anyhow::anyhow!("{other}")),
                })?;
            let wrote = !result.entries.is_empty();
            drive
                .resume_appended(result.entries)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok(wrote)
        }
        CoreAgentAction::Idle | CoreAgentAction::Closed => Ok(false),
        other => Err(DirectAppendError::Other(anyhow::anyhow!(
            "direct repair command produced unexpected action: {other:?}"
        ))),
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
            Ok(description)
                if ExecutionLiveness::from_status(description.status())
                    == ExecutionLiveness::Running =>
            {
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
    use std::{collections::BTreeMap, sync::Mutex};

    use harness::{
        ContentRef, ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentCodec,
        CoreAgentEvent, CoreAgentLifecycleEvent, ModelSelection, PromiseEvent, PromiseOwnership,
        PromiseStatus, ProviderApiKind, RunRequestCommand, RunRequestSource,
        storage::{BlobGraphStore as _, InMemoryBlobStore},
    };
    use temporal_workflow::{default_run_config, default_session_config};

    use super::*;

    async fn stored_session(
        store: &harness::storage::InMemorySessionStore,
        id: &str,
        events: Vec<harness::StoredEvent>,
    ) -> SessionId {
        let session_id = SessionId::new(id);
        store
            .create_session(harness::storage::CreateSession {
                session_id: session_id.clone(),
                display_name: None,
                metadata: Default::default(),
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .unwrap();
        store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events: events
                    .into_iter()
                    .map(|event| harness::session::UncommittedStoredEvent {
                        observed_at_ms: 1,
                        joins: Default::default(),
                        event,
                    })
                    .collect(),
            })
            .await
            .unwrap();
        session_id
    }

    #[derive(Clone)]
    struct LogWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unreadable_sessions_are_identified_without_blocking_healthy_repairs() {
        let store = harness::storage::InMemorySessionStore::new();
        let opened = opened_event();
        // Stores refuse an event that does not decode, so a log a reader cannot
        // decode is one whose joins went bad.
        let malformed = stored_session(&store, "malformed", vec![opened.clone()]).await;
        store
            .append(AppendSessionEvents {
                session_id: malformed.clone(),
                expected_head: store.head(&malformed).await.unwrap(),
                events: vec![harness::session::UncommittedStoredEvent {
                    observed_at_ms: 2,
                    joins: harness::session::StoredJoins::from([(
                        "run_id".to_owned(),
                        "not a number".to_owned(),
                    )]),
                    event: CoreAgentCodec
                        .encode_event(&CoreAgentEvent::Environment(
                            harness::EnvironmentEvent::ActiveEnvironmentCleared,
                        ))
                        .unwrap(),
                }],
            })
            .await
            .unwrap();
        let invalid = stored_session(&store, "invalid", vec![opened.clone(), opened.clone()]).await;
        let promise_id = PromiseId::from_number(1);
        let healthy = stored_session(&store, "healthy", vec![opened, timer_event(1, 5)]).await;
        let logs = Arc::new(Mutex::new(Vec::new()));
        let writer = LogWriter(logs.clone());
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let universe_id = Uuid::new_v4();
        let blobs = InMemoryBlobStore::new();
        let stats = reap_universe_once(
            universe_id,
            &store,
            &blobs,
            &FakeWorkflows::default(),
            &mut SignalledRepairs::default(),
            10,
        )
        .await;
        assert_eq!(stats.sessions_scanned, 3);
        assert_eq!(stats.errors, 2);
        assert_eq!(stats.holder_repairs_appended, 1);
        let state = current_state(&store, &healthy).await;
        assert_eq!(
            state.promises.promises[&promise_id].status,
            PromiseStatus::Resolved
        );
        let captured = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
        let fields: Vec<serde_json::Value> = captured
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["fields"].clone())
            .collect();
        for session in [&malformed, &invalid] {
            let log = fields
                .iter()
                .find(|log| {
                    log["session_id"] == session.as_str()
                        && log["message"] == "promise reaper skipped unreadable session"
                })
                .expect("session-specific warning");
            assert_eq!(log["universe_id"], universe_id.to_string());
            assert!(log["error"].as_str().is_some_and(|error| !error.is_empty()));
        }
        let repair = fields
            .iter()
            .find(|log| log["event"] == "promise_repair_appended")
            .expect("a repair names its session and promise");
        assert_eq!(repair["session_id"], healthy.as_str());
        assert_eq!(repair["promise_id"], promise_id.as_str());
    }

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

    fn opened_event() -> harness::StoredEvent {
        CoreAgentCodec
            .encode_event(&CoreAgentEvent::Lifecycle(
                CoreAgentLifecycleEvent::Opened {
                    config: default_session_config(test_model()),
                },
            ))
            .unwrap()
    }

    fn timer_event(number: u64, fire_at_ms: u64) -> harness::StoredEvent {
        CoreAgentCodec
            .encode_event(&CoreAgentEvent::Promise(PromiseEvent::Created {
                promise: Promise {
                    promise_id: PromiseId::from_number(number),
                    source: PromiseSource::Timer { fire_at_ms },
                    scope: PromiseScope::Session,
                    ownership: PromiseOwnership::Model,
                    status: PromiseStatus::Pending,
                    payload_ref: None,
                    error_ref: None,
                    deadline_at_ms: None,
                },
            }))
            .unwrap()
    }

    fn test_model() -> ModelSelection {
        ModelSelection {
            api_kind: ProviderApiKind::OpenAiResponses,
            provider_id: "openai".to_owned(),
            model: "test-model".to_owned(),
        }
    }

    async fn current_state(
        store: &harness::storage::InMemorySessionStore,
        session_id: &SessionId,
    ) -> CoreAgentState {
        let record = store.load_session(session_id).await.unwrap().unwrap();
        crate::checkpoint::load_reduction(store, &InMemoryBlobStore::new(), &record)
            .await
            .unwrap()
            .reduced
            .core_state
    }

    async fn apply_command(
        store: &harness::storage::InMemorySessionStore,
        session_id: &SessionId,
        command: CoreAgentCommand,
    ) {
        let state = current_state(store, session_id).await;
        let mut drive = CoreAgentDrive::from_replayed(session_id.clone(), state);
        append_command_direct(store, session_id, &mut drive, command, 2)
            .await
            .unwrap();
    }

    async fn session_with_active_run(
        store: &harness::storage::InMemorySessionStore,
        id: &str,
    ) -> SessionId {
        let session_id = stored_session(store, id, vec![opened_event()]).await;
        apply_command(
            store,
            &session_id,
            CoreAgentCommand::RequestRun(RunRequestCommand {
                requested_by: None,
                notify_on_terminal: Vec::new(),
                submission_id: None,
                source: RunRequestSource::Input {
                    input: vec![ContextEntryInput {
                        kind: ContextEntryKind::Message {
                            role: ContextMessageRole::User,
                        },
                        content: ContentRef {
                            content_ref: BlobRef::from_bytes(b"hello"),
                            media_type: Some("text/plain".to_owned()),
                            provider_kind: None,
                        },
                        preview: None,
                        origin: None,
                        provenance_ref: None,
                        token_estimate: None,
                    }],
                },
                run_config: default_run_config(),
                input_limit_tokens: None,
            }),
        )
        .await;
        assert!(active_or_queued_run(&current_state(store, &session_id).await).is_some());
        session_id
    }

    async fn reap(
        store: &harness::storage::InMemorySessionStore,
        workflows: &dyn WorkflowRepairClient,
        signalled: &mut SignalledRepairs,
        now_ms: u64,
    ) -> ReaperStats {
        signalled.start_pass();
        reap_universe_once(
            Uuid::from_u128(1),
            store,
            &InMemoryBlobStore::new(),
            workflows,
            signalled,
            now_ms,
        )
        .await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stopped_workflow_with_active_run_is_observed_as_stale() {
        let store = harness::storage::InMemorySessionStore::new();
        let session_id = session_with_active_run(&store, "stale").await;
        let workflows = FakeWorkflows {
            statuses: BTreeMap::from([(
                session_id,
                SessionWorkflowStatus::Terminal(WorkflowExecutionStatus::Failed),
            )]),
            ..FakeWorkflows::default()
        };
        let stats = reap(&store, &workflows, &mut SignalledRepairs::default(), 10).await;
        assert_eq!(stats.stale_active_projections, 1);
        assert_eq!(stats.workflow_status_errors, 0);
    }

    /// Finishes the session between the reaper's read and its describe, as a
    /// child that is force closed and exits does, then reports it stopped.
    struct ClosesDuringDescribe {
        store: harness::storage::InMemorySessionStore,
        status: SessionWorkflowStatus,
    }

    #[async_trait]
    impl WorkflowRepairClient for ClosesDuringDescribe {
        async fn signal_admissions(
            &self,
            _universe_id: Uuid,
            _session_id: &SessionId,
            _admissions: Vec<AgentAdmission>,
        ) -> Result<(), WorkflowSignalFailure> {
            panic!("a closed session has nothing to repair");
        }

        async fn workflow_status(
            &self,
            _universe_id: Uuid,
            session_id: &SessionId,
        ) -> SessionWorkflowStatus {
            apply_command(
                &self.store,
                session_id,
                CoreAgentCommand::CloseSession { force: true },
            )
            .await;
            self.status.clone()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_session_that_closed_before_its_describe_is_not_reported_stale() {
        for status in [
            SessionWorkflowStatus::Terminal(WorkflowExecutionStatus::Completed),
            SessionWorkflowStatus::Terminal(WorkflowExecutionStatus::Terminated),
            SessionWorkflowStatus::NotFound,
        ] {
            let store = harness::storage::InMemorySessionStore::new();
            session_with_active_run(&store, "child").await;
            let workflows = ClosesDuringDescribe {
                store: store.clone(),
                status,
            };
            let stats = reap(&store, &workflows, &mut SignalledRepairs::default(), 10).await;
            assert_eq!(stats.sessions_scanned, 1);
            assert_eq!(stats.stale_active_projections, 0);
            assert_eq!(stats.workflow_status_errors, 0);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_signalled_repair_that_stays_pending_counts_as_a_repeat_and_logs_at_warn() {
        let store = harness::storage::InMemorySessionStore::new();
        let session_id =
            stored_session(&store, "stalled", vec![opened_event(), timer_event(1, 5)]).await;
        let workflows = FakeWorkflows {
            running: BTreeSet::from([session_id.clone()]),
            ..FakeWorkflows::default()
        };
        let logs = Arc::new(Mutex::new(Vec::new()));
        let writer = LogWriter(logs.clone());
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let mut signalled = SignalledRepairs::default();

        let first = reap(&store, &workflows, &mut signalled, 10).await;
        assert_eq!(first.holder_repairs_signalled, 1);
        assert_eq!(first.holder_repairs_repeated, 0);

        let second = reap(&store, &workflows, &mut signalled, 10).await;
        assert_eq!(second.holder_repairs_signalled, 0);
        assert_eq!(second.holder_repairs_repeated, 1);
        assert_eq!(workflows.signals.lock().unwrap().len(), 2);

        let captured = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
        let logged: Vec<serde_json::Value> = captured
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let repeat = logged
            .iter()
            .find(|log| log["fields"]["event"] == "promise_repair_repeated")
            .expect("a repeat is logged");
        assert_eq!(repeat["level"], "WARN");
        assert_eq!(repeat["fields"]["session_id"], session_id.as_str());
        assert_eq!(
            repeat["fields"]["promise_id"],
            PromiseId::from_number(1).as_str()
        );

        // Landing the resolution drops the key, so a later repair of the same
        // promise id would count as new.
        apply_command(
            &store,
            &session_id,
            CoreAgentCommand::ResolvePromise {
                promise_id: PromiseId::from_number(1),
                resolution: PromiseResolution::Resolved { payload_ref: None },
            },
        )
        .await;
        let settled = reap(&store, &workflows, &mut signalled, 10).await;
        assert_eq!(settled.holder_repairs_signalled, 0);
        assert_eq!(settled.holder_repairs_repeated, 0);
        assert!(signalled.current.is_empty());
        signalled.start_pass();
        assert!(signalled.previous.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closed_sessions_are_not_read_and_a_later_page_is_still_repaired() {
        let store = harness::storage::InMemorySessionStore::new();
        // A closed session whose log would fail any read of it.
        let closed = stored_session(
            &store,
            "zz-closed",
            vec![
                opened_event(),
                CoreAgentCodec
                    .encode_event(&CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Closed))
                    .unwrap(),
            ],
        )
        .await;
        store
            .append(AppendSessionEvents {
                session_id: closed.clone(),
                expected_head: store.head(&closed).await.unwrap(),
                events: vec![harness::session::UncommittedStoredEvent {
                    observed_at_ms: 2,
                    joins: harness::session::StoredJoins::from([(
                        "run_id".to_owned(),
                        "not a number".to_owned(),
                    )]),
                    event: CoreAgentCodec
                        .encode_event(&CoreAgentEvent::Environment(
                            harness::EnvironmentEvent::ActiveEnvironmentCleared,
                        ))
                        .unwrap(),
                }],
            })
            .await
            .unwrap();
        let target =
            stored_session(&store, "aa-target", vec![opened_event(), timer_event(1, 5)]).await;
        for index in 0..SESSION_PAGE_LIMIT {
            stored_session(&store, &format!("s-{index:04}"), vec![opened_event()]).await;
        }
        let first_page = store
            .list_sessions(ListSessions {
                limit: SESSION_PAGE_LIMIT,
                closed: Some(false),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            first_page
                .sessions
                .iter()
                .all(|record| record.session_id != target),
            "the repair sits past the first page"
        );

        let stats = reap(
            &store,
            &FakeWorkflows::default(),
            &mut SignalledRepairs::default(),
            10,
        )
        .await;
        assert_eq!(stats.sessions_scanned, SESSION_PAGE_LIMIT + 1);
        assert_eq!(stats.errors, 0, "the closed session was never read");
        assert_eq!(stats.holder_repairs_appended, 1);
        assert_eq!(
            current_state(&store, &target)
                .await
                .promises
                .pending()
                .count(),
            0
        );
    }

    /// The fence of a direct repair is the state it was planned from, so a write
    /// after the list page costs nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn a_direct_repair_fences_on_the_state_it_read_not_the_list_page() {
        let store = harness::storage::InMemorySessionStore::new();
        let session_id = stored_session(&store, "s", vec![opened_event()]).await;
        let listed = store.load_session(&session_id).await.unwrap().unwrap();
        store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: listed.head.clone(),
                events: vec![harness::session::UncommittedStoredEvent {
                    observed_at_ms: 3,
                    joins: Default::default(),
                    event: timer_event(1, 5),
                }],
            })
            .await
            .unwrap();
        let mut stats = ReaperStats::default();
        reap_session(
            Uuid::from_u128(1),
            &listed.session_id,
            &store,
            &InMemoryBlobStore::new(),
            &FakeWorkflows::default(),
            &mut SignalledRepairs::default(),
            10,
            &mut stats,
        )
        .await;
        assert_eq!(stats.conflicts, 0);
        assert_eq!(stats.holder_repairs_appended, 1);
    }

    /// A plan built from an older state loses to the writer that moved the log,
    /// and writes nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn a_direct_repair_from_a_stale_state_conflicts_and_writes_nothing() {
        let store = harness::storage::InMemorySessionStore::new();
        let session_id = stored_session(&store, "s", vec![opened_event(), timer_event(1, 5)]).await;
        let stale = current_state(&store, &session_id).await;
        store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: store.head(&session_id).await.unwrap(),
                events: vec![harness::session::UncommittedStoredEvent {
                    observed_at_ms: 3,
                    joins: Default::default(),
                    event: timer_event(2, 50),
                }],
            })
            .await
            .unwrap();
        let mut stats = ReaperStats::default();
        apply_session_repairs(
            Uuid::from_u128(1),
            &session_id,
            &store,
            &FakeWorkflows::default(),
            &mut SignalledRepairs::default(),
            stale,
            vec![(
                PromiseId::from_number(1),
                PromiseResolution::Resolved { payload_ref: None },
            )],
            10,
            &mut stats,
        )
        .await;
        assert_eq!(stats.conflicts, 1);
        assert_eq!(stats.holder_repairs_appended, 0);
        assert_eq!(
            current_state(&store, &session_id)
                .await
                .promises
                .pending()
                .count(),
            2
        );
    }

    #[async_trait]
    impl RetentionStore for harness::storage::InMemorySessionStore {
        async fn list_due_roots(
            &self,
            now_ms: u64,
            after: Option<&RetentionRootCursor>,
            limit: usize,
        ) -> Result<Vec<SessionRecord>, SessionStoreError> {
            self.list_retention_roots_due_for_deletion(now_ms, after, limit)
                .await
        }

        async fn delete_due_tree(
            &self,
            root: &SessionId,
            now_ms: u64,
        ) -> Result<DeleteClosedSessionsResult, SessionStoreError> {
            self.delete_closed_sessions(DeleteClosedSessions {
                session_id: root.clone(),
                cascade: true,
                due_at_or_before_ms: Some(now_ms),
            })
            .await
        }
    }

    async fn closed_root(store: &harness::storage::InMemorySessionStore, id: &str, at_ms: u64) {
        let session_id = SessionId::new(id);
        store
            .create_session(harness::storage::CreateSession {
                session_id: session_id.clone(),
                display_name: None,
                metadata: Default::default(),
                origin: None,
                delete_after_close_ms: Some(1),
                created_at_ms: 1,
            })
            .await
            .unwrap();
        store
            .append(AppendSessionEvents {
                session_id,
                expected_head: None,
                events: vec![
                    harness::session::UncommittedStoredEvent {
                        observed_at_ms: at_ms,
                        joins: Default::default(),
                        event: opened_event(),
                    },
                    harness::session::UncommittedStoredEvent {
                        observed_at_ms: at_ms,
                        joins: Default::default(),
                        event: CoreAgentCodec
                            .encode_event(&CoreAgentEvent::Lifecycle(
                                CoreAgentLifecycleEvent::Closed,
                            ))
                            .unwrap(),
                    },
                ],
            })
            .await
            .unwrap();
    }

    /// The row a delegation reserves before its child opens.
    async fn reserved_child(store: &harness::storage::InMemorySessionStore, root: &str, id: &str) {
        store
            .create_session(harness::storage::CreateSession {
                session_id: SessionId::new(id),
                display_name: None,
                metadata: Default::default(),
                origin: Some(harness::storage::SessionOrigin {
                    kind: harness::storage::SessionOriginKind::Subagent,
                    parent_session_id: SessionId::new(root),
                    parent_run_id: 1,
                    root_session_id: SessionId::new(root),
                    depth: 1,
                    invocation_id: format!("invocation-{id}"),
                    profile_id: "reviewer".to_owned(),
                    profile_revision: 1,
                    limits: harness::SubagentLimits::default(),
                }),
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_retention_roots_do_not_hide_a_later_due_root_and_are_counted() {
        let store = harness::storage::InMemorySessionStore::new();
        // More roots with a never-opened child than one page holds, all due first.
        let blocked = SESSION_PAGE_LIMIT + 2;
        for index in 0..blocked {
            let root = format!("blocked-{index:04}");
            closed_root(&store, &root, 10 + index as u64).await;
            reserved_child(&store, &root, &format!("child-{index:04}")).await;
        }
        closed_root(&store, "clean-root", 10_000).await;

        let mut stats = SessionRetentionReaperStats::default();
        reap_due_roots(Uuid::from_u128(1), &store, 20_000, &mut stats).await;

        assert_eq!(stats.due_roots_scanned, blocked + 1);
        assert_eq!(stats.open_tree_skips, blocked);
        assert_eq!(stats.open_tree_sample.len(), OPEN_TREE_SAMPLE_LIMIT);
        assert_eq!(stats.roots_deleted, 1);
        assert_eq!(stats.errors, 0);
        assert_eq!(
            store
                .load_session(&SessionId::new("clean-root"))
                .await
                .unwrap(),
            None
        );

        // Discarding the child that never opened lets its tree go on the next pass.
        assert!(
            store
                .discard_unopened_session(&SessionId::new("child-0000"))
                .await
                .unwrap()
        );
        let mut next = SessionRetentionReaperStats::default();
        reap_due_roots(Uuid::from_u128(1), &store, 20_000, &mut next).await;
        assert_eq!(next.roots_deleted, 1);
        assert_eq!(next.open_tree_skips, blocked - 1);
    }

    /// Sweep double over the in-memory blob store: `live` refs stand in for
    /// every holder kind, recorded edges protect children exactly like the
    /// catalog's incoming-edge check, and the object-backed set plus failure
    /// list drive the object phase.
    #[derive(Default)]
    struct FakeSweepStore {
        blobs: harness::storage::InMemoryBlobStore,
        live: Mutex<BTreeSet<BlobRef>>,
        object_backed: BTreeSet<BlobRef>,
        failing_objects: BTreeSet<String>,
        holder_conflict: bool,
        deleted_objects: Mutex<Vec<String>>,
        scan_limits: Mutex<Vec<usize>>,
        stored_cursor: Mutex<Option<CasSweepCursor>>,
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

        async fn load_sweep_cursor(&self) -> Result<Option<CasSweepCursor>, CasSweepError> {
            Ok(self.stored_cursor.lock().expect("cursor lock").clone())
        }

        async fn save_sweep_cursor(
            &self,
            cursor: Option<&CasSweepCursor>,
        ) -> Result<(), CasSweepError> {
            *self.stored_cursor.lock().expect("cursor lock") = cursor.cloned();
            Ok(())
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
            blobs: harness::storage::InMemoryBlobStore::with_clock(Arc::new(move || {
                clock_now.load(std::sync::atomic::Ordering::SeqCst)
            })),
            ..FakeSweepStore::default()
        };
        let dead = aged_blob(&store, b"dead payload").await;
        let dead_object = aged_blob(&store, b"dead object payload").await;
        let live = aged_blob(&store, b"held by a session").await;
        let parent = aged_blob(&store, b"manifest").await;
        let child = aged_blob(&store, b"file inside the manifest").await;
        let pinned = harness_blob_refs();
        for content in harness::storage::ENGINE_BLOB_CONTENTS {
            aged_blob(&store, content.as_bytes()).await;
        }
        store
            .blobs
            .record_blob_edges(vec![harness::storage::BlobEdge::contains(
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
            blobs: harness::storage::InMemoryBlobStore::with_clock(Arc::new(|| 10)),
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
            blobs: harness::storage::InMemoryBlobStore::with_clock(Arc::new(|| 10)),
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
            blobs: harness::storage::InMemoryBlobStore::with_clock(Arc::new(|| 10)),
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
            blobs: harness::storage::InMemoryBlobStore::with_clock(Arc::new(|| 10)),
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

    /// Live rows at the front of the order fill a pass's allowance, so a process
    /// that started over each time would never reach the garbage behind them.
    #[tokio::test(flavor = "current_thread")]
    async fn cas_sweep_carries_on_past_live_rows_after_a_restart_and_clears_at_the_end() {
        let store = paged_sweep_store(CAS_SWEEP_PAGE_LIMIT + 2).await;
        let rows = store.blobs.blobs_touched_before(100);
        store.live.lock().expect("live lock").extend(
            rows[..CAS_SWEEP_PAGE_LIMIT]
                .iter()
                .map(|blob| blob.blob_ref.clone()),
        );
        let universe_id = Uuid::new_v4();
        // Each call stands for a new process: nothing but the store carries over.
        let pass = |limit, dry_run| {
            let store = &store;
            async move {
                let mut stats = CasSweepStats::default();
                sweep_universe_from_stored_cursor(
                    universe_id,
                    store,
                    100,
                    &[],
                    limit,
                    dry_run,
                    std::time::Instant::now() + CAS_SWEEP_PASS_BUDGET,
                    &mut stats,
                )
                .await;
                stats
            }
        };

        let first = pass(CAS_SWEEP_PAGE_LIMIT, false).await;
        assert_eq!(first.rows_scanned, CAS_SWEEP_PAGE_LIMIT);
        assert_eq!(first.rows_deleted, 0, "only live rows fit this pass");
        assert_eq!(first.walks_completed, 0);
        let stored = store.stored_cursor.lock().unwrap().clone();
        assert!(stored.is_some());

        let dry = pass(CAS_SWEEP_ROW_LIMIT, true).await;
        assert_eq!(
            dry.rows_scanned, 2,
            "a dry run starts at the stored position"
        );
        assert_eq!(dry.candidates, 2);
        assert_eq!(
            *store.stored_cursor.lock().unwrap(),
            stored,
            "and does not move it"
        );

        let second = pass(CAS_SWEEP_ROW_LIMIT, false).await;
        assert_eq!(second.rows_scanned, 2, "resumes past the live rows");
        assert_eq!(second.rows_deleted, 2);
        assert_eq!(second.walks_completed, 1);
        assert_eq!(
            *store.stored_cursor.lock().unwrap(),
            None,
            "reaching the end clears the position"
        );

        // The next walk starts at the oldest row and revisits everything.
        let third = pass(CAS_SWEEP_ROW_LIMIT, false).await;
        assert_eq!(third.rows_scanned, CAS_SWEEP_PAGE_LIMIT);
        assert_eq!(third.rows_deleted, 0);
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
}

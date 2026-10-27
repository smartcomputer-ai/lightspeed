//! `job/*`: durable batch commands with dependencies and queues, on the
//! shared execution primitive.
//!
//! A job is identified by its `jobId` together with the `requestId` of the
//! `job/start` that created it and a hash of its spec: an identical retry
//! replays the job, any other reuse of the id is `jobIdInUse`. A job runs
//! once its dependencies satisfy its policy and its queue key is free. When
//! its root exits the daemon sweeps its process group (`SIGTERM`, the
//! cancel grace, `SIGKILL`) and reports what it swept.
//!
//! Records and output persist under `<state>/jobs/` through the job store,
//! off the runtime thread; output keeps a head and a tail with the middle
//! as an overflow gap. Startup loads summaries only, quarantines unreadable
//! records instead of failing, marks unfinished jobs `interrupted` and kills
//! the groups an earlier instance left running in this boot.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use environment_protocol::{
    bytes::encode_hex,
    data::{
        exec::{
            CommandSpec, ExecInput, ExitStatus, LeftoverDisposition, LeftoverReport, OutputPage,
            OutputStream, ReturnOn, StoppedBy,
        },
        handshake::JobLimits,
        job::{
            CancelJobsParams, CancelJobsResponse, JobCancelScope, JobDependency,
            JobDependencyPolicy, JobReadEntry, JobReadRequest, JobStatus, JobSummary,
            JobSummaryEntry, ReadJobsParams, ReadJobsResponse, StartJobsParams, StartJobsResponse,
        },
    },
    error::{PreconditionReason, ProtocolError},
    ids::{DaemonInstanceId, JobId, JobName, JobRequestId, QueueKey},
    limits::MAX_PAGE_CHUNKS,
    paths::EnvironmentPath,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::sync::watch;

use crate::{
    dispatch::RequestContext,
    exec::{
        AfterExit, ExecOptions, Execution, OutputSink, PreparedCommand,
        job_store::{JobStore, load_output},
        log::OutputLog,
        outcome::describe_exit,
        spawn_record::{SpawnRecord, write_json_atomic},
        validate_command,
    },
    filesystem::Confinement,
    process_group,
};

/// Version of the persisted job record; a record of another version is
/// quarantined.
const JOB_RECORD_FORMAT: u32 = 1;

/// Terminal jobs whose output stays in memory after completion or a load.
const MAX_CACHED_OUTPUTS: usize = 16;

/// Everything a [`JobManager`] needs from the daemon.
pub struct JobConfig {
    /// Canonical default `cwd`.
    pub cwd: PathBuf,
    /// Resolves every `cwd` when the job starts running.
    pub confinement: Arc<Confinement>,
    /// Records and output live in `<state_dir>/jobs`.
    pub state_dir: PathBuf,
    /// Daemon variables no child inherits.
    pub scrubbed_env: Vec<String>,
    pub instance_id: DaemonInstanceId,
    pub limits: JobLimits,
    /// `limits.maxWaitMs`, the `waitMs` ceiling.
    pub max_wait_ms: u64,
}

/// The daemon's jobs. Cheap to clone.
#[derive(Clone)]
pub struct JobManager {
    inner: Arc<JobsInner>,
}

struct JobsInner {
    config: JobConfig,
    store: JobStore,
    state: Mutex<JobsState>,
    /// Bumped on every status change and every output append.
    changed: watch::Sender<u64>,
}

#[derive(Default)]
struct JobsState {
    jobs: BTreeMap<JobId, JobEntry>,
    next_seq: u64,
    /// Terminal jobs holding their output in memory, oldest first.
    cached: VecDeque<JobId>,
}

struct JobEntry {
    record: StoredJob,
    /// The command of a queued job, secrets included. Memory only.
    pending: Option<CommandSpec>,
    run: Option<Run>,
    /// The output of a terminal job, while cached.
    output: Option<Arc<OutputLog>>,
}

struct Run {
    execution: Execution<JobOutput>,
    started: Instant,
}

/// A running job's output: retained head and tail in memory, every byte
/// forwarded to the store.
struct JobOutput {
    job_id: JobId,
    log: OutputLog,
    cap: u64,
    store: JobStore,
    changed: watch::Sender<u64>,
}

impl OutputSink for JobOutput {
    fn append(&mut self, stream: OutputStream, data: &[u8]) {
        self.store
            .append(&self.job_id, self.log.end(), stream, data);
        self.log.append(stream, data);
        self.log.enforce_cap(self.cap, 0);
        self.changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}

/// The persisted record of one job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredJob {
    format: u32,
    job_id: JobId,
    request_id: JobRequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<JobName>,
    spec_hash: String,
    seq: u64,
    status: JobStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<JobId>,
    dependency_policy: JobDependencyPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    queue_key: Option<QueueKey>,
    created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    queued_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cancel_requested_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit: Option<ExitStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stopped_by: Option<StoppedBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    leftovers: Option<LeftoverReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<ProtocolError>,
    /// The running job's process group, for restart reconciliation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spawn: Option<SpawnRecord>,
    /// Output bytes produced, final once the job is terminal.
    #[serde(default)]
    output_end_bytes: u64,
}

impl StoredJob {
    fn summary(&self) -> JobSummary {
        JobSummary {
            job_id: self.job_id.clone(),
            request_id: self.request_id.clone(),
            name: self.name.clone(),
            status: self.status,
            depends_on: self.depends_on.clone(),
            queue_key: self.queue_key.clone(),
            created_at_ms: self.created_at_ms,
            queued_at_ms: self.queued_at_ms,
            started_at_ms: self.started_at_ms,
            finished_at_ms: self.finished_at_ms,
            cancel_requested_at_ms: self.cancel_requested_at_ms,
            duration_ms: self.duration_ms,
            exit: self.exit.clone(),
            stopped_by: self.stopped_by,
            leftovers: self.leftovers.clone(),
            error: self.error.clone(),
        }
    }

    fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("job records encode")
    }
}

/// A validated `job/start` entry with its dependencies resolved.
struct ResolvedJob {
    job_id: JobId,
    name: Option<JobName>,
    command: CommandSpec,
    depends_on: Vec<JobId>,
    dependency_policy: JobDependencyPolicy,
    queue_key: Option<QueueKey>,
    spec_hash: String,
}

impl JobManager {
    /// Loads job summaries and reconciles what an earlier instance left.
    /// Never fails on a record: unreadable ones are quarantined.
    pub fn new(config: JobConfig) -> anyhow::Result<Self> {
        let jobs_dir = config.state_dir.join("jobs");
        std::fs::create_dir_all(&jobs_dir)?;
        let quarantine_dir = config.state_dir.join("jobs-quarantine");
        let boot = process_group::boot_id();
        let now = now_ms();
        let mut state = JobsState::default();
        for entry in std::fs::read_dir(&jobs_dir)? {
            let path = entry?.path();
            let mut record = match load_record(&path) {
                Ok(record) => record,
                Err(reason) => {
                    quarantine(&path, &quarantine_dir, &reason);
                    continue;
                }
            };
            if !record.status.is_terminal() {
                if record.status == JobStatus::Running
                    && let Some(spawn) = &record.spawn
                    && spawn.same_boot(boot.as_deref())
                {
                    spawn.group().signal(process_group::GroupSignal::Kill);
                }
                record.error = Some(ProtocolError::aborted(
                    None,
                    format!(
                        "the daemon stopped while the job was {}",
                        status_name(record.status)
                    ),
                ));
                record.status = JobStatus::Interrupted;
                record.finished_at_ms = Some(now);
                // The last record predates the output the job produced.
                let cap = config.limits.max_retained_output_bytes;
                if let Ok(log) = load_output(&path, record.output_end_bytes, cap) {
                    record.output_end_bytes = log.end();
                }
                if let Err(error) = write_json_atomic(&path.join("record.json"), &record) {
                    eprintln!(
                        "lightspeed-envd: could not persist interrupted job {}: {error}",
                        record.job_id
                    );
                }
            }
            state.next_seq = state.next_seq.max(record.seq.saturating_add(1));
            state.jobs.insert(
                record.job_id.clone(),
                JobEntry {
                    record,
                    pending: None,
                    run: None,
                    output: None,
                },
            );
        }
        let cap = config.limits.max_retained_output_bytes;
        let store = JobStore::start(jobs_dir, cap / 2, cap - cap / 2)?;
        Ok(Self {
            inner: Arc::new(JobsInner {
                config,
                store,
                state: Mutex::new(state),
                changed: watch::Sender::new(0),
            }),
        })
    }

    /// Jobs that are `running`.
    pub async fn running_count(&self) -> u32 {
        self.maintain();
        let running = self
            .state()
            .jobs
            .values()
            .filter(|entry| entry.record.status == JobStatus::Running)
            .count();
        u32::try_from(running).unwrap_or(u32::MAX)
    }

    pub async fn start(
        &self,
        _ctx: &RequestContext,
        params: StartJobsParams,
    ) -> Result<StartJobsResponse, ProtocolError> {
        let resolved = self.resolve_start(&params)?;
        self.maintain();
        let records = {
            let mut state = self.state();
            if let Some(replay) = replay_or_conflict(&state, &params.request_id, &resolved)? {
                return Ok(StartJobsResponse { jobs: replay });
            }
            for (index, job) in resolved.iter().enumerate() {
                for dependency in &job.depends_on {
                    let known = state.jobs.contains_key(dependency)
                        || resolved.iter().any(|other| &other.job_id == dependency);
                    if !known {
                        return Err(ProtocolError::invalid_field(
                            format!("jobs[{index}].dependsOn"),
                            format!("unknown job id {dependency}"),
                        ));
                    }
                }
            }
            let now = now_ms();
            let mut started = Vec::new();
            let mut written = Vec::new();
            for job in resolved {
                let seq = state.next_seq;
                state.next_seq += 1;
                let record = StoredJob {
                    format: JOB_RECORD_FORMAT,
                    job_id: job.job_id.clone(),
                    request_id: params.request_id.clone(),
                    name: job.name,
                    spec_hash: job.spec_hash,
                    seq,
                    status: JobStatus::Queued,
                    depends_on: job.depends_on,
                    dependency_policy: job.dependency_policy,
                    queue_key: job.queue_key,
                    created_at_ms: now,
                    queued_at_ms: None,
                    started_at_ms: None,
                    finished_at_ms: None,
                    cancel_requested_at_ms: None,
                    duration_ms: None,
                    exit: None,
                    stopped_by: None,
                    leftovers: None,
                    error: None,
                    spawn: None,
                    output_end_bytes: 0,
                };
                written.push(self.inner.store.write_record(&job.job_id, record.encode()));
                started.push(job.job_id.clone());
                state.jobs.insert(
                    job.job_id,
                    JobEntry {
                        record,
                        pending: Some(job.command),
                        run: None,
                        output: None,
                    },
                );
            }
            (started, written)
        };
        // The writes were queued under the lock, so the scheduler's later
        // writes land behind them; the start is answered once they are on
        // disk.
        let (started, written) = records;
        for written in written {
            let _ = written.await;
        }
        self.schedule();
        let state = self.state();
        Ok(StartJobsResponse {
            jobs: started
                .iter()
                .filter_map(|job_id| state.jobs.get(job_id))
                .map(|entry| entry.record.summary())
                .collect(),
        })
    }

    pub async fn read(
        &self,
        ctx: &RequestContext,
        params: ReadJobsParams,
    ) -> Result<ReadJobsResponse, ProtocolError> {
        let limits = &self.inner.config.limits;
        check_job_count("jobs", params.jobs.len(), limits.max_request_jobs)?;
        if params.max_bytes > limits.max_read_bytes {
            return Err(ProtocolError::above_ceiling(
                "maxBytes",
                params.max_bytes,
                limits.max_read_bytes,
            ));
        }
        if params.wait_ms > self.inner.config.max_wait_ms {
            return Err(ProtocolError::above_ceiling(
                "waitMs",
                params.wait_ms,
                self.inner.config.max_wait_ms,
            ));
        }
        for (index, entry) in params.jobs.iter().enumerate() {
            if entry.after.is_some() && entry.tail_bytes.is_some() {
                return Err(ProtocolError::invalid_field(
                    format!("jobs[{index}].tailBytes"),
                    "after and tailBytes exclude each other",
                ));
            }
            if let Some(max_bytes) = entry.max_bytes
                && max_bytes > params.max_bytes
            {
                return Err(ProtocolError::above_ceiling(
                    format!("jobs[{index}].maxBytes"),
                    max_bytes,
                    params.max_bytes,
                ));
            }
        }
        self.maintain();
        // The read pages from its own handles on terminal jobs' output: the
        // shared cache may evict a log between loading and paging.
        let mut logs: BTreeMap<JobId, Arc<OutputLog>> = BTreeMap::new();
        for entry in params
            .jobs
            .iter()
            .filter(|entry| needs_output(&params, entry))
        {
            if !logs.contains_key(&entry.job_id)
                && let Some(log) = self.ensure_output(&entry.job_id).await
            {
                logs.insert(entry.job_id.clone(), log);
            }
        }

        let (starts, initial) = {
            let state = self.state();
            let mut starts = Vec::new();
            let mut initial = Vec::new();
            for (index, entry) in params.jobs.iter().enumerate() {
                let job = state.jobs.get(&entry.job_id);
                let end = job.map_or(0, output_end);
                let start = match (entry.after, entry.tail_bytes) {
                    (Some(after), _) => after,
                    (None, Some(tail)) => end.saturating_sub(tail),
                    (None, None) => 0,
                };
                if job.is_some() && start > end {
                    return Err(ProtocolError::invalid_field(
                        format!("jobs[{index}].after"),
                        format!("after {start} is past the end of the output ({end})"),
                    ));
                }
                starts.push(start);
                initial.push(job.map(|job| job.record.status));
            }
            (starts, initial)
        };

        let deadline = tokio::time::Instant::now() + Duration::from_millis(params.wait_ms);
        let mut changes = self.inner.changed.subscribe();
        loop {
            changes.borrow_and_update();
            if self.read_ready(&params, &starts, &initial) {
                break;
            }
            tokio::select! {
                () = ctx.cancel.cancelled() => return Err(ProtocolError::cancelled()),
                () = tokio::time::sleep_until(deadline) => break,
                changed = changes.changed() => if changed.is_err() { break },
            }
        }

        // Jobs that finished during the wait: take their logs under the
        // lock that pages them, loading any the cache no longer holds.
        let mut attempted = BTreeSet::new();
        let state = loop {
            let missing = match self.take_logs(&params, &mut logs, &mut attempted) {
                Ok(state) => break state,
                Err(missing) => missing,
            };
            for job_id in missing {
                if let Some(log) = self.ensure_output(&job_id).await {
                    logs.insert(job_id, log);
                }
            }
        };
        let mut budget = params.max_bytes;
        let mut items = MAX_PAGE_CHUNKS;
        let mut jobs = Vec::with_capacity(params.jobs.len());
        for (entry, start) in params.jobs.iter().zip(starts) {
            let Some(job) = state.jobs.get(&entry.job_id) else {
                jobs.push(JobReadEntry::Error {
                    job_id: entry.job_id.clone(),
                    error: unknown_job(&entry.job_id),
                });
                continue;
            };
            let entry_budget = entry.max_bytes.map_or(budget, |max| max.min(budget));
            let page = |log: &OutputLog| (log.page(start, entry_budget, items), log.end());
            let output = match &job.run {
                Some(_) => with_log(job, page),
                None => logs.get(&entry.job_id).map(|log| page(log)),
            };
            let output = match output {
                Some((page, end)) => {
                    budget -= page.bytes;
                    items = items.saturating_sub(page.items());
                    OutputPage {
                        chunks: page.chunks,
                        next: page.next,
                        end,
                        gaps: page.gaps,
                    }
                }
                None => OutputPage {
                    next: start,
                    end: output_end(job),
                    ..OutputPage::default()
                },
            };
            jobs.push(JobReadEntry::Job {
                summary: Box::new(job.record.summary()),
                output,
            });
        }
        Ok(ReadJobsResponse { jobs })
    }

    pub async fn cancel(
        &self,
        _ctx: &RequestContext,
        params: CancelJobsParams,
    ) -> Result<CancelJobsResponse, ProtocolError> {
        let limits = &self.inner.config.limits;
        check_job_count("jobIds", params.job_ids.len(), limits.max_request_jobs)?;
        self.maintain();
        let response = {
            let mut state = self.state();
            let mut named = Vec::new();
            for job_id in &params.job_ids {
                if !named.contains(job_id) {
                    named.push(job_id.clone());
                }
            }
            let mut dependents = Vec::new();
            if params.scope == JobCancelScope::Dependents {
                let mut affected: BTreeSet<JobId> = named.iter().cloned().collect();
                loop {
                    let next: Vec<JobId> = state
                        .jobs
                        .values()
                        .filter(|entry| {
                            !entry.record.status.is_terminal()
                                && !affected.contains(&entry.record.job_id)
                                && entry
                                    .record
                                    .depends_on
                                    .iter()
                                    .any(|dependency| affected.contains(dependency))
                        })
                        .map(|entry| entry.record.job_id.clone())
                        .collect();
                    if next.is_empty() {
                        break;
                    }
                    for job_id in next {
                        affected.insert(job_id.clone());
                        dependents.push(job_id);
                    }
                }
            }
            let now = now_ms();
            for job_id in named.iter().chain(&dependents) {
                if let Some(entry) = state.jobs.get_mut(job_id) {
                    self.cancel_entry(entry, params.force, now);
                }
            }
            let max_entries = limits.max_request_jobs as usize;
            let mut jobs: Vec<JobSummaryEntry> = named
                .iter()
                .map(|job_id| match state.jobs.get(job_id) {
                    Some(entry) => JobSummaryEntry::Job {
                        summary: Box::new(entry.record.summary()),
                    },
                    None => JobSummaryEntry::Error {
                        job_id: job_id.clone(),
                        error: unknown_job(job_id),
                    },
                })
                .collect();
            let room = max_entries.saturating_sub(jobs.len());
            let unlisted = dependents.len().saturating_sub(room);
            jobs.extend(dependents.iter().take(room).filter_map(|job_id| {
                state.jobs.get(job_id).map(|entry| JobSummaryEntry::Job {
                    summary: Box::new(entry.record.summary()),
                })
            }));
            CancelJobsResponse {
                jobs,
                unlisted_job_count: u32::try_from(unlisted).unwrap_or(u32::MAX),
            }
        };
        self.bump();
        self.schedule();
        Ok(response)
    }

    /// Cancels one job: a queued job ends at once; a running one is
    /// signalled and ends `cancelled` when its root exits.
    fn cancel_entry(&self, entry: &mut JobEntry, force: bool, now: i64) {
        match entry.record.status {
            JobStatus::Queued => {
                entry.pending = None;
                entry.record.status = JobStatus::Cancelled;
                entry.record.finished_at_ms = Some(now);
                self.inner
                    .store
                    .queue_record(&entry.record.job_id, entry.record.encode());
            }
            JobStatus::Running => {
                if entry.record.cancel_requested_at_ms.is_none() {
                    entry.record.cancel_requested_at_ms = Some(now);
                    self.inner
                        .store
                        .queue_record(&entry.record.job_id, entry.record.encode());
                }
                if let Some(run) = &entry.run {
                    run.execution.stop(if force {
                        StoppedBy::Kill
                    } else {
                        StoppedBy::Terminate
                    });
                }
            }
            _ => {}
        }
    }

    /// Validates a start and resolves its dependencies within the request.
    fn resolve_start(&self, params: &StartJobsParams) -> Result<Vec<ResolvedJob>, ProtocolError> {
        let limits = &self.inner.config.limits;
        check_job_count("jobs", params.jobs.len(), limits.max_request_jobs)?;
        let mut ids = BTreeSet::new();
        let mut names = BTreeMap::new();
        for (index, job) in params.jobs.iter().enumerate() {
            let field = format!("jobs[{index}].command");
            validate_command(&job.command, &field)?;
            if matches!(job.command.input, ExecInput::Pipe {}) {
                return Err(ProtocolError::invalid_field(
                    format!("{field}.input"),
                    "jobs take no pipe input; use bytes",
                ));
            }
            if !ids.insert(job.job_id.clone()) {
                return Err(ProtocolError::invalid_field(
                    format!("jobs[{index}].jobId"),
                    format!("job id {} appears twice", job.job_id),
                ));
            }
            if let Some(name) = &job.name
                && names.insert(name.clone(), job.job_id.clone()).is_some()
            {
                return Err(ProtocolError::invalid_field(
                    format!("jobs[{index}].name"),
                    format!("job name {name} appears twice"),
                ));
            }
        }
        let mut resolved = Vec::new();
        for (index, job) in params.jobs.iter().enumerate() {
            let mut depends_on = Vec::new();
            for dependency in &job.depends_on {
                let job_id = match dependency {
                    JobDependency::JobId { job_id } => job_id.clone(),
                    JobDependency::Name { name } => names.get(name).cloned().ok_or_else(|| {
                        ProtocolError::invalid_field(
                            format!("jobs[{index}].dependsOn"),
                            format!("no job of this request is named {name}"),
                        )
                    })?,
                };
                if !depends_on.contains(&job_id) {
                    depends_on.push(job_id);
                }
            }
            let spec_hash = spec_hash(
                job.name.as_ref(),
                &job.command,
                &depends_on,
                job.dependency_policy,
                job.queue_key.as_ref(),
            );
            resolved.push(ResolvedJob {
                job_id: job.job_id.clone(),
                name: job.name.clone(),
                command: job.command.clone(),
                depends_on,
                dependency_policy: job.dependency_policy,
                queue_key: job.queue_key.clone(),
                spec_hash,
            });
        }
        reject_cycles(&resolved)?;
        Ok(resolved)
    }

    /// Fails jobs whose dependencies can no longer succeed and starts every
    /// runnable job whose queue is free, in start order.
    fn schedule(&self) {
        let mut state = self.state();
        let mut changed = false;
        loop {
            let now = now_ms();
            let mut progressed = false;
            let doomed: Vec<JobId> = state
                .jobs
                .values()
                .filter(|entry| {
                    entry.record.status == JobStatus::Queued
                        && entry.record.dependency_policy == JobDependencyPolicy::AllSucceeded
                        && entry.record.depends_on.iter().any(|dependency| {
                            state.jobs.get(dependency).is_none_or(|dependency| {
                                dependency.record.status.is_terminal()
                                    && dependency.record.status != JobStatus::Succeeded
                            })
                        })
                })
                .map(|entry| entry.record.job_id.clone())
                .collect();
            for job_id in doomed {
                if let Some(entry) = state.jobs.get_mut(&job_id) {
                    entry.pending = None;
                    entry.record.status = JobStatus::DependencyFailed;
                    entry.record.finished_at_ms = Some(now);
                    self.inner
                        .store
                        .queue_record(&job_id, entry.record.encode());
                    progressed = true;
                }
            }

            let mut busy: BTreeSet<QueueKey> = state
                .jobs
                .values()
                .filter(|entry| entry.record.status == JobStatus::Running)
                .filter_map(|entry| entry.record.queue_key.clone())
                .collect();
            let mut runnable: Vec<(u64, JobId)> = state
                .jobs
                .values()
                .filter(|entry| {
                    entry.record.status == JobStatus::Queued
                        && entry.record.depends_on.iter().all(|dependency| {
                            state.jobs.get(dependency).is_none_or(|dependency| {
                                match entry.record.dependency_policy {
                                    JobDependencyPolicy::AllSucceeded => {
                                        dependency.record.status == JobStatus::Succeeded
                                    }
                                    JobDependencyPolicy::AllTerminal => {
                                        dependency.record.status.is_terminal()
                                    }
                                }
                            })
                        })
                })
                .map(|entry| (entry.record.seq, entry.record.job_id.clone()))
                .collect();
            runnable.sort();
            for (_, job_id) in runnable {
                let entry = state.jobs.get_mut(&job_id).expect("runnable job exists");
                let newly_runnable = entry.record.queued_at_ms.is_none();
                if newly_runnable {
                    entry.record.queued_at_ms = Some(now);
                }
                if let Some(key) = &entry.record.queue_key
                    && !busy.insert(key.clone())
                {
                    if newly_runnable {
                        self.inner
                            .store
                            .queue_record(&job_id, entry.record.encode());
                    }
                    continue;
                }
                self.start_entry(entry, now);
                progressed = true;
            }
            changed |= progressed;
            if !progressed {
                break;
            }
        }
        drop(state);
        if changed {
            self.bump();
        }
    }

    /// Starts one runnable job, or fails it when its `cwd` or spawn fails.
    fn start_entry(&self, entry: &mut JobEntry, now: i64) {
        let config = &self.inner.config;
        let job_id = entry.record.job_id.clone();
        let Some(command) = entry.pending.take() else {
            entry.record.status = JobStatus::Failed;
            entry.record.finished_at_ms = Some(now);
            entry.record.error = Some(ProtocolError::internal("the job's command was lost"));
            self.inner
                .store
                .queue_record(&job_id, entry.record.encode());
            return;
        };
        let options = ExecOptions {
            timeout: command.timeout_ms.map(Duration::from_millis),
            terminate_grace: Duration::from_millis(config.limits.cancel_grace_ms),
            after_exit: AfterExit::Manual,
        };
        let spawned = resolve_cwd(config, command.cwd.as_ref()).and_then(|cwd| {
            let sink = JobOutput {
                job_id: job_id.clone(),
                log: OutputLog::default(),
                cap: config.limits.max_retained_output_bytes,
                store: self.inner.store.clone(),
                changed: self.inner.changed.clone(),
            };
            PreparedCommand::new(command, cwd, &config.scrubbed_env, None).spawn(sink, options, 0)
        });
        match spawned {
            Ok(execution) => {
                entry.record.status = JobStatus::Running;
                entry.record.started_at_ms = Some(now);
                entry.record.spawn = Some(SpawnRecord::new(execution.group(), &config.instance_id));
                self.inner
                    .store
                    .queue_record(&job_id, entry.record.encode());
                execution.start_input();
                entry.run = Some(Run {
                    execution: execution.clone(),
                    started: Instant::now(),
                });
                let manager = self.clone();
                tokio::spawn(async move { manager.supervise(job_id, execution).await });
            }
            Err(error) => {
                entry.record.status = JobStatus::Failed;
                entry.record.finished_at_ms = Some(now);
                entry.record.error = Some(error);
                self.inner
                    .store
                    .queue_record(&job_id, entry.record.encode());
            }
        }
    }

    /// Follows a running job to its end: root exit, sweep, settled output,
    /// final record, then the scheduler.
    async fn supervise(self, job_id: JobId, execution: Execution<JobOutput>) {
        execution.wait_until(|state| state.exited()).await;
        let leftovers = execution.sample_leftovers(LeftoverDisposition::Swept);
        if leftovers.is_some() || execution.group_alive() {
            execution
                .group()
                .terminate(Duration::from_millis(
                    self.inner.config.limits.cancel_grace_ms,
                ))
                .await;
        }
        execution.settle(true).await;
        let (exit, stopped_by, error, log) = execution.with_state(|state| {
            (
                state.exit().cloned(),
                state.stopped_by(),
                state.error().cloned(),
                Arc::new(state.sink.log.clone()),
            )
        });
        let status = match (&exit, stopped_by) {
            (_, Some(StoppedBy::Timeout)) => JobStatus::TimedOut,
            (_, Some(StoppedBy::Terminate | StoppedBy::Kill)) => JobStatus::Cancelled,
            (Some(ExitStatus::Code { code: 0 }), None) if error.is_none() => JobStatus::Succeeded,
            _ => JobStatus::Failed,
        };
        let error = error.or_else(|| match (&exit, status) {
            (Some(exit), JobStatus::Failed) if !matches!(exit, ExitStatus::Code { .. }) => {
                Some(ProtocolError::process_failed(describe_exit(exit)))
            }
            _ => None,
        });

        let final_record = {
            let state = self.state();
            let Some(entry) = state.jobs.get(&job_id) else {
                return;
            };
            let mut record = entry.record.clone();
            record.status = status;
            record.exit = exit;
            record.stopped_by = stopped_by;
            record.leftovers = leftovers;
            record.error = error;
            record.finished_at_ms = Some(now_ms());
            record.duration_ms = entry
                .run
                .as_ref()
                .map(|run| u64::try_from(run.started.elapsed().as_millis()).unwrap_or(u64::MAX));
            record.output_end_bytes = log.end();
            record
        };
        // The output and the record are on disk before anyone sees the job
        // terminal, so a later load from disk finds everything.
        self.inner
            .store
            .finish(&job_id, final_record.encode())
            .await;
        {
            let mut state = self.state();
            let JobsState { jobs, cached, .. } = &mut *state;
            if let Some(entry) = jobs.get_mut(&job_id) {
                let cancel_requested_at_ms = entry.record.cancel_requested_at_ms;
                entry.record = final_record;
                if entry.record.cancel_requested_at_ms != cancel_requested_at_ms {
                    entry.record.cancel_requested_at_ms = cancel_requested_at_ms;
                    self.inner
                        .store
                        .queue_record(&job_id, entry.record.encode());
                }
                entry.run = None;
                entry.output = Some(log);
                cache_output(jobs, cached, &job_id);
            }
        }
        self.bump();
        self.schedule();
    }

    /// A terminal job's output: the cached log, or the log loaded from disk
    /// (and cached). `None` for an unknown or running job.
    async fn ensure_output(&self, job_id: &JobId) -> Option<Arc<OutputLog>> {
        let (dir, end) = {
            let mut state = self.state();
            let JobsState { jobs, cached, .. } = &mut *state;
            let entry = jobs.get(job_id)?;
            if entry.run.is_some() || !entry.record.status.is_terminal() {
                return None;
            }
            if let Some(log) = entry.output.clone() {
                cache_output(jobs, cached, job_id);
                return Some(log);
            }
            (
                self.inner.store.root().join(job_id.as_str()),
                entry.record.output_end_bytes,
            )
        };
        let cap = self.inner.config.limits.max_retained_output_bytes;
        let loaded = tokio::task::spawn_blocking(move || load_output(&dir, end, cap)).await;
        let log = match loaded {
            Ok(Ok(log)) => log,
            Ok(Err(error)) => {
                eprintln!("lightspeed-envd: could not load output of job {job_id}: {error}");
                OutputLog::from_records([], end)
            }
            Err(error) => {
                eprintln!("lightspeed-envd: could not load output of job {job_id}: {error}");
                OutputLog::from_records([], end)
            }
        };
        let mut state = self.state();
        let JobsState { jobs, cached, .. } = &mut *state;
        let entry = jobs.get_mut(job_id)?;
        let log = match &entry.output {
            Some(log) => log.clone(),
            None => {
                entry.record.output_end_bytes = entry.record.output_end_bytes.max(log.end());
                let log = Arc::new(log);
                entry.output = Some(log.clone());
                log
            }
        };
        cache_output(jobs, cached, job_id);
        Some(log)
    }

    /// Adds to `logs` the cached output of every terminal job the read
    /// pages, and answers the locked state to page from. When a terminal
    /// job's output is in neither the cache nor `logs` (and was not
    /// `attempted`), answers those jobs instead: load them and call again.
    fn take_logs(
        &self,
        params: &ReadJobsParams,
        logs: &mut BTreeMap<JobId, Arc<OutputLog>>,
        attempted: &mut BTreeSet<JobId>,
    ) -> Result<MutexGuard<'_, JobsState>, Vec<JobId>> {
        let state = self.state();
        let mut missing = Vec::new();
        for entry in params
            .jobs
            .iter()
            .filter(|entry| needs_output(params, entry))
        {
            let Some(job) = state.jobs.get(&entry.job_id) else {
                continue;
            };
            if job.run.is_some() || logs.contains_key(&entry.job_id) {
                continue;
            }
            if let Some(log) = &job.output {
                logs.insert(entry.job_id.clone(), log.clone());
            } else if job.record.status.is_terminal() && attempted.insert(entry.job_id.clone()) {
                missing.push(entry.job_id.clone());
            }
        }
        if missing.is_empty() {
            Ok(state)
        } else {
            Err(missing)
        }
    }

    fn read_ready(
        &self,
        params: &ReadJobsParams,
        starts: &[u64],
        initial: &[Option<JobStatus>],
    ) -> bool {
        let state = self.state();
        let entries = params.jobs.iter().zip(starts).zip(initial);
        let all_terminal = params.jobs.iter().all(|entry| {
            state
                .jobs
                .get(&entry.job_id)
                .is_none_or(|job| job.record.status.is_terminal())
        });
        if all_terminal {
            return true;
        }
        match params.return_on {
            ReturnOn::Exit => false,
            ReturnOn::AnyOutput => entries.into_iter().any(|((entry, start), initial)| {
                let job = state.jobs.get(&entry.job_id);
                job.map(|job| job.record.status) != *initial
                    || job.is_some_and(|job| output_end(job) > *start)
            }),
            ReturnOn::Window => entries.into_iter().any(|((entry, start), _)| {
                let budget = entry.max_bytes.unwrap_or(params.max_bytes);
                state
                    .jobs
                    .get(&entry.job_id)
                    .is_some_and(|job| output_end(job).saturating_sub(*start) >= budget)
            }),
        }
    }

    /// Garbage-collects terminal jobs past the record retention.
    fn maintain(&self) {
        let retention =
            i64::try_from(self.inner.config.limits.record_retention_ms).unwrap_or(i64::MAX);
        let now = now_ms();
        let mut state = self.state();
        let expired: Vec<JobId> = state
            .jobs
            .values()
            .filter(|entry| {
                entry.record.status.is_terminal()
                    && entry
                        .record
                        .finished_at_ms
                        .is_some_and(|finished| now.saturating_sub(finished) >= retention)
            })
            .map(|entry| entry.record.job_id.clone())
            .collect();
        for job_id in expired {
            state.jobs.remove(&job_id);
            state.cached.retain(|cached| cached != &job_id);
            self.inner.store.remove(&job_id);
        }
    }

    fn state(&self) -> MutexGuard<'_, JobsState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn bump(&self) {
        self.inner
            .changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}

/// An identical retry replays every job; any other overlap with existing
/// jobs is `jobIdInUse`. `None` when every job is new.
fn replay_or_conflict(
    state: &JobsState,
    request_id: &JobRequestId,
    resolved: &[ResolvedJob],
) -> Result<Option<Vec<JobSummary>>, ProtocolError> {
    let existing = resolved
        .iter()
        .filter(|job| state.jobs.contains_key(&job.job_id))
        .count();
    if existing == 0 {
        return Ok(None);
    }
    let mut summaries = Vec::new();
    for job in resolved {
        let in_use = |detail: &str| {
            ProtocolError::failed_precondition(
                PreconditionReason::JobIdInUse,
                format!(
                    "job id {} {detail}; choose a new job id for a new run",
                    job.job_id
                ),
            )
        };
        let Some(entry) = state.jobs.get(&job.job_id) else {
            return Err(in_use(
                "is new while other jobs of this request already exist",
            ));
        };
        if &entry.record.request_id != request_id {
            return Err(in_use("was started by another request"));
        }
        if entry.record.spec_hash != job.spec_hash {
            return Err(in_use("was started with a different spec"));
        }
        summaries.push(entry.record.summary());
    }
    Ok(Some(summaries))
}

fn reject_cycles(resolved: &[ResolvedJob]) -> Result<(), ProtocolError> {
    let index: BTreeMap<&JobId, usize> = resolved
        .iter()
        .enumerate()
        .map(|(position, job)| (&job.job_id, position))
        .collect();
    // 0 unvisited, 1 on the stack, 2 done.
    let mut marks = vec![0u8; resolved.len()];
    fn visit(
        position: usize,
        resolved: &[ResolvedJob],
        index: &BTreeMap<&JobId, usize>,
        marks: &mut [u8],
    ) -> Result<(), usize> {
        match marks[position] {
            2 => return Ok(()),
            1 => return Err(position),
            _ => {}
        }
        marks[position] = 1;
        for dependency in &resolved[position].depends_on {
            if let Some(&next) = index.get(dependency) {
                visit(next, resolved, index, marks)?;
            }
        }
        marks[position] = 2;
        Ok(())
    }
    for position in 0..resolved.len() {
        if let Err(cycle) = visit(position, resolved, &index, &mut marks) {
            return Err(ProtocolError::invalid_field(
                format!("jobs[{cycle}].dependsOn"),
                format!(
                    "job {} depends on itself through a cycle",
                    resolved[cycle].job_id
                ),
            ));
        }
    }
    Ok(())
}

/// The identity hash of a job's spec. Secret values and input bytes enter
/// only as their own digests.
fn spec_hash(
    name: Option<&JobName>,
    command: &CommandSpec,
    depends_on: &[JobId],
    dependency_policy: JobDependencyPolicy,
    queue_key: Option<&QueueKey>,
) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Material<'a> {
        name: Option<&'a JobName>,
        argv: &'a [String],
        cwd: Option<&'a EnvironmentPath>,
        env: &'a BTreeMap<String, String>,
        secret_env: BTreeMap<&'a str, String>,
        input: String,
        timeout_ms: Option<u64>,
        depends_on: &'a [JobId],
        dependency_policy: JobDependencyPolicy,
        queue_key: Option<&'a QueueKey>,
    }
    let digest = |bytes: &[u8]| encode_hex(&Sha256::digest(bytes));
    let material = Material {
        name,
        argv: &command.argv,
        cwd: command.cwd.as_ref(),
        env: &command.env,
        secret_env: command
            .secret_env
            .iter()
            .map(|(name, value)| (name.as_str(), digest(value.expose().as_bytes())))
            .collect(),
        input: match &command.input {
            ExecInput::None {} => "none".to_owned(),
            ExecInput::Bytes { data } => format!("bytes:{}", digest(data.as_slice())),
            ExecInput::Pipe {} => "pipe".to_owned(),
        },
        timeout_ms: command.timeout_ms,
        depends_on,
        dependency_policy,
        queue_key,
    };
    digest(&serde_json::to_vec(&material).expect("hash material encodes"))
}

fn resolve_cwd(
    config: &JobConfig,
    cwd: Option<&EnvironmentPath>,
) -> Result<PathBuf, ProtocolError> {
    match cwd {
        Some(path) => config.confinement.resolve_directory(path),
        None => Ok(config.cwd.clone()),
    }
}

/// Whether a `job/read` entry pages output (rather than only a summary).
fn needs_output(params: &ReadJobsParams, entry: &JobReadRequest) -> bool {
    params.max_bytes > 0 || entry.after.is_some() || entry.tail_bytes.is_some()
}

/// Runs `f` on the job's retained output: the live log while it runs, the
/// cached log once terminal. `None` when neither is in memory.
fn with_log<R>(entry: &JobEntry, f: impl FnOnce(&OutputLog) -> R) -> Option<R> {
    if let Some(run) = &entry.run {
        return Some(run.execution.with_state(|state| f(&state.sink.log)));
    }
    entry.output.as_deref().map(f)
}

fn output_end(entry: &JobEntry) -> u64 {
    with_log(entry, OutputLog::end).unwrap_or(entry.record.output_end_bytes)
}

fn cache_output(
    jobs: &mut BTreeMap<JobId, JobEntry>,
    cached: &mut VecDeque<JobId>,
    job_id: &JobId,
) {
    cached.retain(|cached| cached != job_id);
    cached.push_back(job_id.clone());
    while cached.len() > MAX_CACHED_OUTPUTS {
        if let Some(evicted) = cached.pop_front()
            && let Some(entry) = jobs.get_mut(&evicted)
        {
            entry.output = None;
        }
    }
}

fn check_job_count(field: &str, count: usize, max: u32) -> Result<(), ProtocolError> {
    if count == 0 {
        return Err(ProtocolError::invalid_field(field, "name at least one job"));
    }
    if count > max as usize {
        return Err(ProtocolError::above_ceiling(
            field,
            count as u64,
            u64::from(max),
        ));
    }
    Ok(())
}

fn unknown_job(job_id: &JobId) -> ProtocolError {
    ProtocolError::not_found(format!("unknown job id: {job_id}"))
}

fn status_name(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "queued",
        JobStatus::Running => "running",
        JobStatus::Succeeded => "succeeded",
        JobStatus::Failed => "failed",
        JobStatus::TimedOut => "timedOut",
        JobStatus::Cancelled => "cancelled",
        JobStatus::DependencyFailed => "dependencyFailed",
        JobStatus::Interrupted => "interrupted",
    }
}

/// Reads one job directory's record.
fn load_record(dir: &Path) -> Result<StoredJob, String> {
    if !dir.is_dir() {
        return Err("not a job directory".to_owned());
    }
    let bytes = std::fs::read(dir.join("record.json")).map_err(|error| error.to_string())?;
    let record: StoredJob = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if record.format != JOB_RECORD_FORMAT {
        return Err(format!(
            "record format {} is not {JOB_RECORD_FORMAT}",
            record.format
        ));
    }
    if dir.file_name().and_then(|name| name.to_str()) != Some(record.job_id.as_str()) {
        return Err("the directory does not match the job id".to_owned());
    }
    Ok(record)
}

/// Moves an unreadable job aside so it never blocks startup again.
fn quarantine(path: &Path, quarantine_dir: &Path, reason: &str) {
    let name = path
        .file_name()
        .map_or_else(|| "job".into(), |name| name.to_string_lossy().into_owned());
    let target = quarantine_dir.join(format!("{name}.{}", now_ms()));
    let moved =
        std::fs::create_dir_all(quarantine_dir).and_then(|()| std::fs::rename(path, &target));
    match moved {
        Ok(()) => eprintln!(
            "lightspeed-envd: quarantined unreadable job record {} ({reason}) at {}",
            path.display(),
            target.display()
        ),
        Err(error) => eprintln!(
            "lightspeed-envd: could not quarantine unreadable job record {} ({reason}): {error}",
            path.display()
        ),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use environment_protocol::{
        bytes::ByteChunk,
        data::{exec::GapReason, job::JobSpec},
        error::{ErrorData, ProtocolErrorKind},
    };

    use super::*;

    fn limits() -> JobLimits {
        JobLimits {
            max_read_bytes: 4 * 1024 * 1024,
            max_request_jobs: 64,
            max_retained_output_bytes: 2 * 1024 * 1024,
            record_retention_ms: 7 * 24 * 60 * 60 * 1000,
            cancel_grace_ms: 200,
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        state_dir: PathBuf,
        manager: JobManager,
    }

    fn fixture_with(limits: JobLimits) -> Fixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let state_dir = root.join("state");
        let manager = manager_at(&root, &state_dir, limits);
        Fixture {
            _temp: temp,
            root,
            state_dir,
            manager,
        }
    }

    fn fixture() -> Fixture {
        fixture_with(limits())
    }

    fn manager_at(root: &Path, state_dir: &Path, limits: JobLimits) -> JobManager {
        let confinement =
            Confinement::new(root.to_owned(), state_dir.to_owned()).expect("confinement");
        JobManager::new(JobConfig {
            cwd: root.to_owned(),
            confinement: Arc::new(confinement),
            state_dir: state_dir.to_owned(),
            scrubbed_env: Vec::new(),
            instance_id: DaemonInstanceId::new("envd_test").unwrap(),
            limits,
            max_wait_ms: 60_000,
        })
        .expect("manager")
    }

    fn ctx() -> RequestContext {
        RequestContext::detached()
    }

    fn id(value: &str) -> JobId {
        JobId::new(value).unwrap()
    }

    fn request(value: &str) -> JobRequestId {
        JobRequestId::new(value).unwrap()
    }

    fn job(job_id: &str, script: &str) -> JobSpec {
        JobSpec {
            job_id: id(job_id),
            name: None,
            command: CommandSpec {
                argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
                cwd: None,
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                input: ExecInput::None {},
                timeout_ms: None,
            },
            depends_on: Vec::new(),
            dependency_policy: JobDependencyPolicy::AllSucceeded,
            queue_key: None,
        }
    }

    fn start(request_id: &str, jobs: Vec<JobSpec>) -> StartJobsParams {
        StartJobsParams {
            request_id: request(request_id),
            jobs,
        }
    }

    fn entry(job_id: &str) -> JobReadRequest {
        JobReadRequest {
            job_id: id(job_id),
            after: None,
            tail_bytes: None,
            max_bytes: None,
        }
    }

    /// Waits until every job is terminal and reads their output heads.
    async fn finished(manager: &JobManager, job_ids: &[&str]) -> Vec<(JobSummary, OutputPage)> {
        let response = manager
            .read(
                &ctx(),
                ReadJobsParams {
                    jobs: job_ids.iter().map(|job_id| entry(job_id)).collect(),
                    max_bytes: 4 * 1024 * 1024,
                    wait_ms: 30_000,
                    return_on: ReturnOn::Exit,
                },
            )
            .await
            .expect("read");
        response
            .jobs
            .into_iter()
            .map(|entry| match entry {
                JobReadEntry::Job { summary, output } => {
                    assert!(summary.status.is_terminal(), "{summary:?}");
                    (*summary, output)
                }
                JobReadEntry::Error { job_id, error } => panic!("{job_id}: {error}"),
            })
            .collect()
    }

    fn text(page: &OutputPage) -> String {
        String::from_utf8(
            page.chunks
                .iter()
                .flat_map(|chunk| chunk.data.as_slice().to_vec())
                .collect(),
        )
        .unwrap()
    }

    fn precondition(error: &ProtocolError) -> Option<PreconditionReason> {
        match &error.data {
            ErrorData::FailedPrecondition { reason } => Some(*reason),
            _ => None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn jobs_run_and_report_exits() {
        let fixture = fixture();
        let response = fixture
            .manager
            .start(
                &ctx(),
                start("r1", vec![job("ok", "printf done"), job("bad", "exit 4")]),
            )
            .await
            .expect("start");
        assert_eq!(response.jobs.len(), 2);
        let jobs = finished(&fixture.manager, &["ok", "bad"]).await;
        assert_eq!(jobs[0].0.status, JobStatus::Succeeded);
        assert_eq!(text(&jobs[0].1), "done");
        assert_eq!(jobs[0].1.end, 4);
        assert!(jobs[0].0.duration_ms.is_some());
        assert_eq!(jobs[1].0.status, JobStatus::Failed);
        assert_eq!(jobs[1].0.exit, Some(ExitStatus::Code { code: 4 }));
        assert_eq!(fixture.manager.running_count().await, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_resubmitted_job_id_is_refused_and_an_identical_retry_replays() {
        let fixture = fixture();
        let script = "echo run >> runs.txt";
        fixture
            .manager
            .start(&ctx(), start("first", vec![job("build", script)]))
            .await
            .expect("start");
        finished(&fixture.manager, &["build"]).await;

        let replay = fixture
            .manager
            .start(&ctx(), start("first", vec![job("build", script)]))
            .await
            .expect("replay");
        assert_eq!(replay.jobs[0].status, JobStatus::Succeeded);

        let error = fixture
            .manager
            .start(&ctx(), start("second", vec![job("build", script)]))
            .await
            .unwrap_err();
        assert_eq!(precondition(&error), Some(PreconditionReason::JobIdInUse));
        let error = fixture
            .manager
            .start(&ctx(), start("first", vec![job("build", "echo changed")]))
            .await
            .unwrap_err();
        assert_eq!(precondition(&error), Some(PreconditionReason::JobIdInUse));
        let error = fixture
            .manager
            .start(
                &ctx(),
                start("first", vec![job("build", script), job("new", "true")]),
            )
            .await
            .unwrap_err();
        assert_eq!(precondition(&error), Some(PreconditionReason::JobIdInUse));

        let runs = std::fs::read_to_string(fixture.root.join("runs.txt")).unwrap();
        assert_eq!(runs, "run\n", "nothing ran twice");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn large_input_never_keeps_a_timeout_from_firing() {
        // The child never reads its input, so the timeout must be armed
        // before the input is written or the job runs forever.
        let fixture = fixture();
        let mut stuck = job("stuck", "sleep 30");
        stuck.command.input = ExecInput::Bytes {
            data: ByteChunk::from(vec![b'x'; 1024 * 1024]),
        };
        stuck.command.timeout_ms = Some(300);
        let mut echo = job("echo", "cat");
        let input: Vec<u8> = (0..512 * 1024u32).map(|i| b'a' + (i % 26) as u8).collect();
        echo.command.input = ExecInput::Bytes {
            data: ByteChunk::from(input.clone()),
        };
        fixture
            .manager
            .start(&ctx(), start("r", vec![stuck, echo]))
            .await
            .expect("start");
        let jobs = finished(&fixture.manager, &["stuck", "echo"]).await;
        assert_eq!(jobs[0].0.status, JobStatus::TimedOut);
        assert_eq!(jobs[0].0.stopped_by, Some(StoppedBy::Timeout));
        assert_eq!(jobs[1].0.status, JobStatus::Succeeded);
        assert_eq!(text(&jobs[1].1).as_bytes(), input.as_slice());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn verbose_output_keeps_head_and_tail_and_reads_back_after_a_restart() {
        let cap = 64 * 1024;
        let fixture = fixture_with(JobLimits {
            max_retained_output_bytes: cap,
            ..limits()
        });
        fixture
            .manager
            .start(
                &ctx(),
                start(
                    "r",
                    vec![job(
                        "verbose",
                        "i=0; while [ $i -lt 30000 ]; do echo line-$i; i=$((i+1)); done; echo 'FAILED: 3 tests'",
                    )],
                ),
            )
            .await
            .expect("start");
        let jobs = finished(&fixture.manager, &["verbose"]).await;
        let (summary, page) = &jobs[0];
        assert_eq!(summary.status, JobStatus::Succeeded);
        assert_eq!(page.gaps.len(), 1);
        assert_eq!(page.gaps[0].reason, GapReason::Overflow);
        assert_eq!(page.gaps[0].offset, cap / 2);
        let shown = text(page);
        assert!(shown.starts_with("line-0\n"));
        assert!(shown.ends_with("FAILED: 3 tests\n"));
        assert_eq!(shown.len() as u64 + page.gaps[0].omitted_bytes, page.end);

        // Head and tail in one read, as the job service asks for them.
        let response = fixture
            .manager
            .read(
                &ctx(),
                ReadJobsParams {
                    jobs: vec![
                        JobReadRequest {
                            max_bytes: Some(10),
                            ..entry("verbose")
                        },
                        JobReadRequest {
                            tail_bytes: Some(16),
                            ..entry("verbose")
                        },
                    ],
                    max_bytes: 26,
                    wait_ms: 0,
                    return_on: ReturnOn::Exit,
                },
            )
            .await
            .expect("head and tail");
        let pages: Vec<OutputPage> = response
            .jobs
            .into_iter()
            .map(|entry| match entry {
                JobReadEntry::Job { output, .. } => output,
                JobReadEntry::Error { error, .. } => panic!("{error}"),
            })
            .collect();
        assert_eq!(text(&pages[0]), "line-0\nlin");
        assert_eq!(text(&pages[1]), "FAILED: 3 tests\n");

        let restarted = manager_at(
            &fixture.root,
            &fixture.state_dir,
            JobLimits {
                max_retained_output_bytes: cap,
                ..limits()
            },
        );
        let reloaded = finished(&restarted, &["verbose"]).await;
        assert_eq!(reloaded[0].0.status, JobStatus::Succeeded);
        let reread = &reloaded[0].1;
        assert_eq!(text(reread), shown, "the output reads back from disk");
        assert_eq!(reread.gaps, page.gaps);
        assert_eq!(reread.end, page.end);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pages_continue_exactly_where_they_stopped() {
        let fixture = fixture();
        fixture
            .manager
            .start(
                &ctx(),
                start(
                    "r",
                    vec![job(
                        "page",
                        "i=0; while [ $i -lt 2000 ]; do printf '%09d\\n' $i; i=$((i+1)); done",
                    )],
                ),
            )
            .await
            .expect("start");
        finished(&fixture.manager, &["page"]).await;
        let mut after = 0;
        let mut collected = String::new();
        loop {
            let response = fixture
                .manager
                .read(
                    &ctx(),
                    ReadJobsParams {
                        jobs: vec![JobReadRequest {
                            after: Some(after),
                            ..entry("page")
                        }],
                        max_bytes: 100,
                        wait_ms: 0,
                        return_on: ReturnOn::Exit,
                    },
                )
                .await
                .expect("page");
            let JobReadEntry::Job { output, .. } = &response.jobs[0] else {
                panic!("job read failed");
            };
            collected.push_str(&text(output));
            if output.next == output.end {
                break;
            }
            assert!(output.next > after);
            after = output.next;
        }
        assert_eq!(collected.len(), 20_000, "no byte of 20 000 was skipped");
        assert!(collected.ends_with("000001999\n"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_read_of_more_jobs_than_the_cache_holds_returns_every_output() {
        let fixture = fixture();
        let count = MAX_CACHED_OUTPUTS + 4;
        let names: Vec<String> = (0..count).map(|index| format!("job-{index}")).collect();
        fixture
            .manager
            .start(
                &ctx(),
                start(
                    "r",
                    names
                        .iter()
                        .map(|name| job(name, &format!("printf {name}")))
                        .collect(),
                ),
            )
            .await
            .expect("start");
        let ids: Vec<&str> = names.iter().map(String::as_str).collect();
        finished(&fixture.manager, &ids).await;

        // A restarted daemon holds no output in memory, so every log is
        // loaded from disk during this one read.
        let restarted = manager_at(&fixture.root, &fixture.state_dir, limits());
        for manager in [&fixture.manager, &restarted] {
            let jobs = finished(manager, &ids).await;
            for (name, (_, page)) in names.iter().zip(&jobs) {
                assert_eq!(&text(page), name);
                assert_eq!(page.next, page.end);
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_jobs_are_per_entry_not_found() {
        let fixture = fixture();
        let response = fixture
            .manager
            .read(
                &ctx(),
                ReadJobsParams {
                    jobs: vec![entry("missing")],
                    max_bytes: 0,
                    wait_ms: 1_000,
                    return_on: ReturnOn::Exit,
                },
            )
            .await
            .expect("read");
        match &response.jobs[0] {
            JobReadEntry::Error { job_id, error } => {
                assert_eq!(job_id, &id("missing"));
                assert_eq!(error.kind(), ProtocolErrorKind::NotFound);
            }
            JobReadEntry::Job { .. } => panic!("no summary is invented"),
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn cancel_is_graceful_unless_forced() {
        let fixture = fixture();
        fixture
            .manager
            .start(
                &ctx(),
                start(
                    "r",
                    vec![
                        job(
                            "graceful",
                            "trap 'echo trapped; exit 0' TERM; sleep 30 & wait",
                        ),
                        job(
                            "forced",
                            "trap 'echo trapped; exit 0' TERM; sleep 30 & wait",
                        ),
                        job("queued", "true"),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, mut spec)| {
                        if index == 2 {
                            spec.depends_on = vec![JobDependency::JobId {
                                job_id: id("forced"),
                            }];
                        }
                        spec
                    })
                    .collect(),
                ),
            )
            .await
            .expect("start");
        tokio::time::sleep(Duration::from_millis(200)).await;
        fixture
            .manager
            .cancel(
                &ctx(),
                CancelJobsParams {
                    job_ids: vec![id("graceful")],
                    scope: JobCancelScope::Job,
                    force: false,
                },
            )
            .await
            .expect("cancel");
        let cancelled = fixture
            .manager
            .cancel(
                &ctx(),
                CancelJobsParams {
                    job_ids: vec![id("forced"), id("missing")],
                    scope: JobCancelScope::Dependents,
                    force: true,
                },
            )
            .await
            .expect("force cancel");
        assert_eq!(cancelled.jobs.len(), 3, "named jobs, then the dependent");
        assert!(
            matches!(&cancelled.jobs[1], JobSummaryEntry::Error { error, .. } if error.kind() == ProtocolErrorKind::NotFound)
        );
        let jobs = finished(&fixture.manager, &["graceful", "forced", "queued"]).await;
        assert_eq!(jobs[0].0.status, JobStatus::Cancelled);
        assert_eq!(jobs[0].0.stopped_by, Some(StoppedBy::Terminate));
        assert!(jobs[0].0.cancel_requested_at_ms.is_some());
        assert_eq!(
            text(&jobs[0].1),
            "trapped\n",
            "SIGTERM let the job clean up"
        );
        assert_eq!(jobs[1].0.status, JobStatus::Cancelled);
        assert_eq!(jobs[1].0.stopped_by, Some(StoppedBy::Kill));
        assert_eq!(text(&jobs[1].1), "", "a forced cancel kills at once");
        assert_eq!(jobs[2].0.status, JobStatus::Cancelled);
        assert_eq!(jobs[2].0.started_at_ms, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dependencies_and_queues_order_jobs() {
        let fixture = fixture();
        let mut first = job("first", "sleep 0.2; echo first >> order.txt");
        first.name = Some(JobName::new("first job").unwrap());
        first.queue_key = Some(QueueKey::new("q").unwrap());
        let mut second = job("second", "echo second >> order.txt");
        second.queue_key = Some(QueueKey::new("q").unwrap());
        let mut after = job("after", "echo after >> order.txt; exit 1");
        after.depends_on = vec![JobDependency::Name {
            name: JobName::new("first job").unwrap(),
        }];
        let mut doomed = job("doomed", "echo doomed >> order.txt");
        doomed.depends_on = vec![JobDependency::JobId {
            job_id: id("after"),
        }];
        let mut tolerant = job("tolerant", "echo tolerant >> order.txt");
        tolerant.depends_on = vec![JobDependency::JobId {
            job_id: id("after"),
        }];
        tolerant.dependency_policy = JobDependencyPolicy::AllTerminal;
        fixture
            .manager
            .start(
                &ctx(),
                start("r", vec![first, second, after, doomed, tolerant]),
            )
            .await
            .expect("start");
        let jobs = finished(
            &fixture.manager,
            &["first", "second", "after", "doomed", "tolerant"],
        )
        .await;
        assert_eq!(jobs[3].0.status, JobStatus::DependencyFailed);
        assert_eq!(jobs[3].0.started_at_ms, None);
        assert_eq!(jobs[4].0.status, JobStatus::Succeeded);
        assert_eq!(jobs[2].0.depends_on, vec![id("first")]);
        let order = std::fs::read_to_string(fixture.root.join("order.txt")).unwrap();
        let lines: Vec<&str> = order.lines().collect();
        assert_eq!(lines[0], "first", "one job per queue key, in start order");
        assert!(lines.iter().position(|line| *line == "second") > Some(0));
        assert!(!lines.contains(&"doomed"));

        let mut cyclic = job("a", "true");
        cyclic.depends_on = vec![JobDependency::JobId { job_id: id("b") }];
        let mut back = job("b", "true");
        back.depends_on = vec![JobDependency::JobId { job_id: id("a") }];
        let error = fixture
            .manager
            .start(&ctx(), start("cycle", vec![cyclic, back]))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::InvalidParams);
        let mut piped = job("piped", "cat");
        piped.command.input = ExecInput::Pipe {};
        let error = fixture
            .manager
            .start(&ctx(), start("pipe", vec![piped]))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::InvalidParams);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn leftovers_are_swept_and_reported() {
        let fixture = fixture();
        fixture
            .manager
            .start(
                &ctx(),
                start(
                    "r",
                    vec![job("leaky", "sleep 30 & echo $! > orphan.pid; exit 0")],
                ),
            )
            .await
            .expect("start");
        let jobs = finished(&fixture.manager, &["leaky"]).await;
        assert_eq!(jobs[0].0.status, JobStatus::Succeeded);
        let leftovers = jobs[0]
            .0
            .leftovers
            .as_ref()
            .expect("swept leftovers reported");
        assert_eq!(leftovers.disposition, LeftoverDisposition::Swept);
        let orphan: i32 = std::fs::read_to_string(fixture.root.join("orphan.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let alive = rustix::process::Pid::from_raw(orphan)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok());
        assert!(!alive, "the group was swept");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(flavor = "current_thread")]
    async fn a_restart_interrupts_unfinished_jobs_and_kills_their_groups() {
        let fixture = fixture();
        fixture
            .manager
            .start(
                &ctx(),
                start("r", vec![job("long", "echo $$ > root.pid; sleep 30")]),
            )
            .await
            .expect("start");
        let pid_file = fixture.root.join("root.pid");
        for _ in 0..250 {
            if std::fs::read_to_string(&pid_file).is_ok_and(|pid| !pid.trim().is_empty()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let root: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The running record reaches the disk through the store thread.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let restarted = manager_at(&fixture.root, &fixture.state_dir, limits());
        let jobs = finished(&restarted, &["long"]).await;
        assert_eq!(jobs[0].0.status, JobStatus::Interrupted);
        assert_eq!(
            jobs[0].0.error.as_ref().map(ProtocolError::kind),
            Some(ProtocolErrorKind::Aborted)
        );
        for _ in 0..100 {
            let alive = rustix::process::Pid::from_raw(root)
                .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok());
            if !alive {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the earlier instance's job group survived the restart");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unreadable_records_are_quarantined_instead_of_failing_startup() {
        let fixture = fixture();
        let bad = fixture.state_dir.join("jobs").join("broken");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("record.json"), b"{not json").unwrap();
        std::fs::write(fixture.state_dir.join("jobs").join("stray.json"), b"{}").unwrap();
        let restarted = manager_at(&fixture.root, &fixture.state_dir, limits());
        assert_eq!(restarted.running_count().await, 0);
        assert!(!bad.exists());
        let quarantined = std::fs::read_dir(fixture.state_dir.join("jobs-quarantine"))
            .unwrap()
            .count();
        assert_eq!(quarantined, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_bounds_are_ceilings_not_clamps() {
        let fixture = fixture_with(JobLimits {
            max_request_jobs: 2,
            ..limits()
        });
        let error = fixture
            .manager
            .start(
                &ctx(),
                start(
                    "r",
                    vec![job("a", "true"), job("b", "true"), job("c", "true")],
                ),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error.data,
            ErrorData::InvalidParams {
                ceiling: Some(2),
                ..
            }
        ));
        let error = fixture
            .manager
            .read(
                &ctx(),
                ReadJobsParams {
                    jobs: vec![JobReadRequest {
                        max_bytes: Some(20),
                        ..entry("a")
                    }],
                    max_bytes: 10,
                    wait_ms: 0,
                    return_on: ReturnOn::Exit,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::InvalidParams);
        let error = fixture
            .manager
            .cancel(
                &ctx(),
                CancelJobsParams {
                    job_ids: Vec::new(),
                    scope: JobCancelScope::Job,
                    force: false,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::InvalidParams);
    }
}

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use host_protocol::{
    data::jobs::{
        CancelJobsParams, CancelJobsResponse, JobCancelScope, JobDependencyPolicy, JobOutputChunk,
        JobOutputStream, JobReadResult, JobStartMode, JobStartSpec, JobStatus, JobSummary,
        ReadJobsParams, ReadJobsResponse, StartJobsParams, StartJobsResponse,
    },
    error::{HostError, HostErrorCode},
    shared::{ByteChunk, HostPath, JobId},
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{Mutex, Notify},
};

#[derive(Clone)]
pub struct JobManager {
    cwd: PathBuf,
    fs_root: PathBuf,
    jobs_root: PathBuf,
    state: Arc<Mutex<JobManagerState>>,
    notify: Arc<Notify>,
}

#[derive(Default)]
struct JobManagerState {
    jobs: BTreeMap<String, JobRecord>,
    running: BTreeMap<String, Arc<RunningJob>>,
}

struct RunningJob {
    cancel: Notify,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobRecord {
    job_id: JobId,
    deck_id: Option<String>,
    name: Option<String>,
    argv: Vec<String>,
    cwd: Option<HostPath>,
    env: BTreeMap<String, String>,
    stdin: Option<ByteChunk>,
    timeout_ms: Option<u64>,
    dependency_policy: JobDependencyPolicy,
    dependencies: Vec<JobId>,
    metadata: BTreeMap<String, String>,
    serial_lane: Option<String>,
    spec_hash: String,
    status: JobStatus,
    created_at_ms: u64,
    queued_at_ms: Option<u64>,
    started_at_ms: Option<u64>,
    finished_at_ms: Option<u64>,
    exit_code: Option<i32>,
    failure: Option<String>,
    output_chunks: Vec<JobOutputChunk>,
    output_next_seq: u64,
}

#[derive(Clone)]
struct ResolvedJob {
    spec: JobStartSpec,
    dependencies: Vec<JobId>,
    serial_lane: Option<String>,
    spec_hash: String,
}

#[derive(Clone, Copy)]
enum FinishKind {
    Exit,
    Cancelled,
    TimedOut,
}

impl JobManager {
    pub fn new(cwd: PathBuf, fs_root: PathBuf) -> anyhow::Result<Self> {
        let cwd = normalize_path(cwd);
        let fs_root = normalize_path(fs_root);
        let jobs_root = fs_root.join(".lightspeed").join("jobs");
        std::fs::create_dir_all(&jobs_root)?;
        let mut state = JobManagerState::default();
        let now = now_ms();

        for entry in std::fs::read_dir(&jobs_root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            let mut record: JobRecord = serde_json::from_slice(&bytes)?;
            if !record.status.is_terminal() {
                record.status = JobStatus::Interrupted;
                record.finished_at_ms = Some(now);
                record.failure =
                    Some("host-bridge restarted before this job could be recovered".to_owned());
                persist_record_at(&jobs_root, &record)?;
            }
            state.jobs.insert(record.job_id.as_str().to_owned(), record);
        }

        Ok(Self {
            cwd,
            fs_root,
            jobs_root,
            state: Arc::new(Mutex::new(state)),
            notify: Arc::new(Notify::new()),
        })
    }

    pub async fn start_jobs(
        &self,
        params: StartJobsParams,
    ) -> Result<StartJobsResponse, HostError> {
        let now = now_ms();
        let resolved = {
            let state = self.state.lock().await;
            validate_and_resolve_start(&state, &params)?
        };
        let deck_id = params.deck_id.clone().or_else(|| {
            resolved
                .first()
                .map(|job| format!("deck-{}", job.spec.job_id.as_str()))
        });

        let mut accepted = Vec::new();
        {
            let mut state = self.state.lock().await;
            for job in &resolved {
                if let Some(existing) = state.jobs.get(job.spec.job_id.as_str()) {
                    accepted.push(existing.summary());
                    continue;
                }

                let record = JobRecord {
                    job_id: job.spec.job_id.clone(),
                    deck_id: deck_id.clone(),
                    name: job.spec.name.clone(),
                    argv: job.spec.argv.clone(),
                    cwd: job.spec.cwd.clone(),
                    env: job.spec.env.clone(),
                    stdin: job.spec.stdin.clone(),
                    timeout_ms: job.spec.timeout_ms,
                    dependency_policy: job.spec.dependency_policy,
                    dependencies: job.dependencies.clone(),
                    metadata: job.spec.metadata.clone(),
                    serial_lane: job.serial_lane.clone(),
                    spec_hash: job.spec_hash.clone(),
                    status: JobStatus::Queued,
                    created_at_ms: now,
                    queued_at_ms: Some(now),
                    started_at_ms: None,
                    finished_at_ms: None,
                    exit_code: None,
                    failure: None,
                    output_chunks: Vec::new(),
                    output_next_seq: 0,
                };
                self.persist_record(&record)?;
                accepted.push(record.summary());
                state.jobs.insert(record.job_id.as_str().to_owned(), record);
            }
        }

        self.schedule_ready_jobs().await;

        let state = self.state.lock().await;
        Ok(StartJobsResponse {
            deck_id,
            jobs: accepted
                .into_iter()
                .map(|summary| {
                    state
                        .jobs
                        .get(summary.job_id.as_str())
                        .map(JobRecord::summary)
                        .unwrap_or(summary)
                })
                .collect(),
        })
    }

    pub async fn read_jobs(&self, params: ReadJobsParams) -> Result<ReadJobsResponse, HostError> {
        if params.jobs.is_empty() {
            return Err(HostError::new(
                HostErrorCode::InvalidRequest,
                "job/read requires at least one job id",
            ));
        }

        let deadline = params
            .wait_ms
            .map(|wait_ms| tokio::time::Instant::now() + Duration::from_millis(wait_ms));

        loop {
            let response = {
                let state = self.state.lock().await;
                let response = build_read_response(&state, &params);
                let all_terminal = response
                    .jobs
                    .iter()
                    .all(|result| result.summary.status.is_terminal());
                let should_wait = params.wait_ms.is_some()
                    && !all_terminal
                    && deadline.is_some_and(|deadline| tokio::time::Instant::now() < deadline);
                if should_wait { None } else { Some(response) }
            };
            if let Some(response) = response {
                return Ok(response);
            }

            let Some(deadline) = deadline else {
                continue;
            };
            tokio::select! {
                _ = self.notify.notified() => {}
                _ = tokio::time::sleep_until(deadline) => {}
            }
        }
    }

    pub async fn cancel_jobs(
        &self,
        params: CancelJobsParams,
    ) -> Result<CancelJobsResponse, HostError> {
        if params.jobs.is_empty() {
            return Err(HostError::new(
                HostErrorCode::InvalidRequest,
                "job/cancel requires at least one job id",
            ));
        }

        let now = now_ms();
        let mut cancel_handles = Vec::new();
        let target_ids = {
            let state = self.state.lock().await;
            resolve_cancel_scope(&state, &params)
        };

        {
            let mut state = self.state.lock().await;
            for job_id in &target_ids {
                let Some(status) = state.jobs.get(job_id).map(|record| record.status) else {
                    continue;
                };
                if status.is_terminal() {
                    continue;
                }
                let running = state.running.get(job_id).cloned();
                let Some(record) = state.jobs.get_mut(job_id) else {
                    continue;
                };
                match status {
                    JobStatus::Running | JobStatus::CancelRequested => {
                        record.status = JobStatus::CancelRequested;
                        record.failure = Some(if params.force {
                            "job force cancellation requested".to_owned()
                        } else {
                            "job cancellation requested".to_owned()
                        });
                        if let Some(running) = running {
                            cancel_handles.push(running);
                        }
                    }
                    _ => {
                        record.status = JobStatus::Cancelled;
                        record.finished_at_ms = Some(now);
                        record.failure = Some("job cancelled before start".to_owned());
                    }
                }
                self.persist_record(record)?;
            }
        }

        for running in cancel_handles {
            running.cancel.notify_waiters();
        }
        self.notify.notify_waiters();
        self.schedule_ready_jobs().await;

        let state = self.state.lock().await;
        Ok(CancelJobsResponse {
            jobs: target_ids
                .iter()
                .map(|job_id| {
                    state
                        .jobs
                        .get(job_id)
                        .map(JobRecord::summary)
                        .unwrap_or_else(|| lost_summary(job_id))
                })
                .collect(),
        })
    }

    async fn schedule_ready_jobs(&self) {
        loop {
            let ready = {
                let mut state = self.state.lock().await;
                if let Err(error) = self.mark_dependency_failures(&mut state) {
                    eprintln!("host-bridge job dependency update failed: {error:?}");
                }

                let ready_ids = state
                    .jobs
                    .iter()
                    .filter(|(job_id, record)| {
                        matches!(record.status, JobStatus::Accepted | JobStatus::Queued)
                            && !state.running.contains_key(job_id.as_str())
                            && dependencies_satisfied(&state, record)
                    })
                    .map(|(job_id, _)| job_id.clone())
                    .collect::<Vec<_>>();
                let now = now_ms();
                let mut ready = Vec::new();
                for job_id in ready_ids {
                    let mut ready_record = None;
                    if let Some(record) = state.jobs.get_mut(&job_id) {
                        record.status = JobStatus::Running;
                        record.started_at_ms = Some(now);
                        record.failure = None;
                        if let Err(error) = self.persist_record(record) {
                            eprintln!(
                                "host-bridge failed to persist running job {job_id}: {error:?}"
                            );
                        }
                        ready_record = Some(record.clone());
                    }
                    if let Some(record) = ready_record {
                        let running = Arc::new(RunningJob {
                            cancel: Notify::new(),
                        });
                        state.running.insert(job_id, running.clone());
                        ready.push((record.clone(), running));
                    }
                }
                ready
            };

            if ready.is_empty() {
                break;
            }
            for (record, running) in ready {
                let manager = self.clone();
                tokio::spawn(async move {
                    manager.run_job(record, running).await;
                });
            }
        }
    }

    fn mark_dependency_failures(&self, state: &mut JobManagerState) -> Result<(), HostError> {
        loop {
            let mut changed = false;
            let failed = state
                .jobs
                .iter()
                .filter(|(_, record)| {
                    matches!(record.status, JobStatus::Accepted | JobStatus::Queued)
                        && record.dependency_policy == JobDependencyPolicy::AllSucceeded
                        && record.dependencies.iter().any(|dependency| {
                            state
                                .jobs
                                .get(dependency.as_str())
                                .map(|dependency| {
                                    dependency.status.is_terminal()
                                        && dependency.status != JobStatus::Succeeded
                                })
                                .unwrap_or(true)
                        })
                })
                .map(|(job_id, _)| job_id.clone())
                .collect::<Vec<_>>();
            if failed.is_empty() {
                return Ok(());
            }
            let now = now_ms();
            for job_id in failed {
                let Some(record) = state.jobs.get_mut(&job_id) else {
                    continue;
                };
                if record.status.is_terminal() {
                    continue;
                }
                record.status = JobStatus::DependencyFailed;
                record.finished_at_ms = Some(now);
                record.failure = Some("one or more dependencies did not succeed".to_owned());
                self.persist_record(record)?;
                changed = true;
            }
            if !changed {
                return Ok(());
            }
            self.notify.notify_waiters();
        }
    }

    async fn run_job(&self, record: JobRecord, running: Arc<RunningJob>) {
        let result = self.spawn_and_wait(record.clone(), running.clone()).await;
        let (status, exit_code, failure) = match result {
            Ok((FinishKind::Exit, exit_code)) => {
                if exit_code == Some(0) {
                    (JobStatus::Succeeded, exit_code, None)
                } else {
                    (
                        JobStatus::Failed,
                        exit_code,
                        Some(match exit_code {
                            Some(code) => format!("job exited with status {code}"),
                            None => "job exited without a status code".to_owned(),
                        }),
                    )
                }
            }
            Ok((FinishKind::Cancelled, exit_code)) => (
                JobStatus::Cancelled,
                exit_code,
                Some("job cancelled".to_owned()),
            ),
            Ok((FinishKind::TimedOut, exit_code)) => (
                JobStatus::TimedOut,
                exit_code,
                Some("job timed out".to_owned()),
            ),
            Err(error) => (JobStatus::Failed, None, Some(error.message)),
        };

        {
            let mut state = self.state.lock().await;
            state.running.remove(record.job_id.as_str());
            if let Some(record) = state.jobs.get_mut(record.job_id.as_str()) {
                record.status = status;
                record.exit_code = exit_code;
                record.failure = failure;
                record.finished_at_ms = Some(now_ms());
                if let Err(error) = self.persist_record(record) {
                    eprintln!(
                        "host-bridge failed to persist finished job {}: {error:?}",
                        record.job_id
                    );
                }
            }
        }
        self.notify.notify_waiters();
        self.kick_scheduler();
    }

    fn kick_scheduler(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            manager.schedule_ready_jobs().await;
        });
    }

    async fn spawn_and_wait(
        &self,
        record: JobRecord,
        running: Arc<RunningJob>,
    ) -> Result<(FinishKind, Option<i32>), HostError> {
        if record.argv.is_empty() {
            return Err(HostError::new(
                HostErrorCode::InvalidRequest,
                "job argv must not be empty",
            ));
        }
        let cwd = record
            .cwd
            .as_ref()
            .map(|path| self.resolve_cwd(path))
            .transpose()?
            .unwrap_or_else(|| self.cwd.clone());

        let mut command = Command::new(&record.argv[0]);
        command
            .args(&record.argv[1..])
            .current_dir(cwd)
            .envs(record.env.iter())
            .stdin(if record.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|error| {
            HostError::new(
                HostErrorCode::ProcessFailed,
                format!("spawn job {:?}: {error}", record.argv),
            )
        })?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        if let Some(input) = record.stdin.as_ref() {
            let Some(stdin) = child.stdin.as_mut() else {
                return Err(HostError::new(
                    HostErrorCode::ProcessFailed,
                    "job stdin was not available",
                ));
            };
            stdin
                .write_all(input.as_slice())
                .await
                .map_err(|error| HostError::new(HostErrorCode::ProcessFailed, error.to_string()))?;
            child.stdin.take();
        }

        let stdout_task = stdout.map(|stdout| {
            tokio::spawn(read_job_stream(
                self.clone(),
                record.job_id.clone(),
                stdout,
                JobOutputStream::Stdout,
            ))
        });
        let stderr_task = stderr.map(|stderr| {
            tokio::spawn(read_job_stream(
                self.clone(),
                record.job_id.clone(),
                stderr,
                JobOutputStream::Stderr,
            ))
        });

        let (finish, status) = if let Some(timeout_ms) = record.timeout_ms {
            tokio::select! {
                status = child.wait() => {
                    (FinishKind::Exit, status)
                }
                _ = running.cancel.notified() => {
                    let _ = child.start_kill();
                    (FinishKind::Cancelled, child.wait().await)
                }
                _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                    let _ = child.start_kill();
                    (FinishKind::TimedOut, child.wait().await)
                }
            }
        } else {
            tokio::select! {
                status = child.wait() => {
                    (FinishKind::Exit, status)
                }
                _ = running.cancel.notified() => {
                    let _ = child.start_kill();
                    (FinishKind::Cancelled, child.wait().await)
                }
            }
        };

        if let Some(task) = stdout_task {
            let _ = task.await;
        }
        if let Some(task) = stderr_task {
            let _ = task.await;
        }

        let status = status
            .map_err(|error| HostError::new(HostErrorCode::ProcessFailed, error.to_string()))?;
        Ok((finish, status.code()))
    }

    fn resolve_cwd(&self, path: &HostPath) -> Result<PathBuf, HostError> {
        let candidate = if path.is_absolute() {
            PathBuf::from(path.as_str())
        } else if path.as_str() == "." {
            self.cwd.clone()
        } else {
            self.cwd.join(path.as_str())
        };
        let normalized = normalize_path(candidate);
        if !normalized.starts_with(&self.fs_root) {
            return Err(HostError::new(
                HostErrorCode::Forbidden,
                format!(
                    "job cwd is outside bridge fs root: {} (root {})",
                    normalized.display(),
                    self.fs_root.display()
                ),
            ));
        }
        Ok(normalized)
    }

    fn persist_record(&self, record: &JobRecord) -> Result<(), HostError> {
        persist_record_at(&self.jobs_root, record).map_err(|error| {
            HostError::new(
                HostErrorCode::Internal,
                format!("persist job {}: {error}", record.job_id),
            )
        })
    }
}

async fn read_job_stream<R>(
    manager: JobManager,
    job_id: JobId,
    mut reader: R,
    stream: JobOutputStream,
) where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0; 8192];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => {
                manager.notify.notify_waiters();
                return;
            }
            Ok(read) => read,
            Err(error) => {
                let mut state = manager.state.lock().await;
                if let Some(record) = state.jobs.get_mut(job_id.as_str()) {
                    record.failure = Some(error.to_string());
                    if let Err(error) = manager.persist_record(record) {
                        eprintln!(
                            "host-bridge failed to persist stream failure for {job_id}: {error:?}"
                        );
                    }
                }
                manager.notify.notify_waiters();
                return;
            }
        };
        let mut state = manager.state.lock().await;
        if let Some(record) = state.jobs.get_mut(job_id.as_str()) {
            let seq = record.output_next_seq;
            record.output_next_seq += 1;
            record.output_chunks.push(JobOutputChunk {
                seq,
                stream,
                chunk: ByteChunk::from(buffer[..read].to_vec()),
            });
            if let Err(error) = manager.persist_record(record) {
                eprintln!("host-bridge failed to persist output for {job_id}: {error:?}");
            }
        }
        manager.notify.notify_waiters();
    }
}

fn validate_and_resolve_start(
    state: &JobManagerState,
    params: &StartJobsParams,
) -> Result<Vec<ResolvedJob>, HostError> {
    if params.jobs.is_empty() {
        return Err(invalid_request("job/start requires at least one job"));
    }
    validate_optional_nonempty("deck_id", params.deck_id.as_deref())?;
    validate_optional_nonempty("serial_lane", params.serial_lane.as_deref())?;
    validate_optional_nonempty("idempotency_key", params.idempotency_key.as_deref())?;

    let mut seen_job_ids = BTreeSet::new();
    let mut seen_names = BTreeMap::new();
    for spec in &params.jobs {
        if spec.argv.is_empty() {
            return Err(invalid_request(format!(
                "job {} argv must not be empty",
                spec.job_id
            )));
        }
        if !seen_job_ids.insert(spec.job_id.as_str().to_owned()) {
            return Err(invalid_request(format!(
                "duplicate job id in start request: {}",
                spec.job_id
            )));
        }
        if let Some(name) = spec.name.as_deref() {
            validate_nonempty("job name", name)?;
            if seen_names
                .insert(name.to_owned(), spec.job_id.clone())
                .is_some()
            {
                return Err(invalid_request(format!("duplicate job name: {name}")));
            }
        }
    }

    let request_job_ids = params
        .jobs
        .iter()
        .map(|spec| spec.job_id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let serial_lane = if params.mode == JobStartMode::Serial {
        Some(
            params
                .serial_lane
                .clone()
                .unwrap_or_else(|| "default".to_owned()),
        )
    } else {
        None
    };
    let previous_serial_job = serial_lane
        .as_ref()
        .and_then(|lane| previous_non_terminal_in_lane(state, lane));

    let mut resolved = Vec::new();
    for (index, spec) in params.jobs.iter().enumerate() {
        let mut dependencies = resolve_explicit_dependencies(&seen_names, spec)?;
        if let Some(lane) = serial_lane.as_ref() {
            if index == 0 {
                if let Some(previous) = previous_serial_job.as_ref() {
                    push_unique_job_id(&mut dependencies, previous.clone());
                }
            } else {
                push_unique_job_id(&mut dependencies, params.jobs[index - 1].job_id.clone());
            }
            validate_nonempty("serial_lane", lane)?;
        }

        for dependency in &dependencies {
            if !request_job_ids.contains(dependency.as_str())
                && !state.jobs.contains_key(dependency.as_str())
            {
                return Err(invalid_request(format!(
                    "job {} depends on unknown job id {}",
                    spec.job_id, dependency
                )));
            }
        }

        let spec_hash = job_spec_hash(params, spec, &dependencies, serial_lane.as_deref())?;
        if let Some(existing) = state.jobs.get(spec.job_id.as_str()) {
            if existing.spec_hash != spec_hash {
                return Err(HostError::new(
                    HostErrorCode::Conflict,
                    format!(
                        "job id already exists with different input: {}",
                        spec.job_id
                    ),
                ));
            }
        }

        resolved.push(ResolvedJob {
            spec: spec.clone(),
            dependencies,
            serial_lane: serial_lane.clone(),
            spec_hash,
        });
    }

    reject_cycles(&resolved)?;
    Ok(resolved)
}

fn resolve_explicit_dependencies(
    names: &BTreeMap<String, JobId>,
    spec: &JobStartSpec,
) -> Result<Vec<JobId>, HostError> {
    let mut dependencies = Vec::new();
    for dependency in &spec.depends_on {
        match (&dependency.job_id, &dependency.name) {
            (Some(job_id), None) => push_unique_job_id(&mut dependencies, job_id.clone()),
            (None, Some(name)) => {
                validate_nonempty("dependency name", name)?;
                let Some(job_id) = names.get(name) else {
                    return Err(invalid_request(format!(
                        "job {} depends on unknown local name {name}",
                        spec.job_id
                    )));
                };
                push_unique_job_id(&mut dependencies, job_id.clone());
            }
            (Some(_), Some(_)) => {
                return Err(invalid_request(format!(
                    "job {} dependency must use either jobId or name, not both",
                    spec.job_id
                )));
            }
            (None, None) => {
                return Err(invalid_request(format!(
                    "job {} dependency must include jobId or name",
                    spec.job_id
                )));
            }
        }
    }
    for dependency in &dependencies {
        if dependency == &spec.job_id {
            return Err(invalid_request(format!(
                "job {} cannot depend on itself",
                spec.job_id
            )));
        }
    }
    Ok(dependencies)
}

fn reject_cycles(resolved: &[ResolvedJob]) -> Result<(), HostError> {
    let graph = resolved
        .iter()
        .map(|job| {
            (
                job.spec.job_id.as_str().to_owned(),
                job.dependencies
                    .iter()
                    .filter(|dependency| {
                        resolved
                            .iter()
                            .any(|job| job.spec.job_id.as_str() == dependency.as_str())
                    })
                    .map(|dependency| dependency.as_str().to_owned())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for job_id in graph.keys() {
        visit_job(job_id, &graph, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_job(
    job_id: &str,
    graph: &BTreeMap<String, Vec<String>>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) -> Result<(), HostError> {
    if visited.contains(job_id) {
        return Ok(());
    }
    if !visiting.insert(job_id.to_owned()) {
        return Err(invalid_request(format!(
            "job dependency cycle includes {job_id}"
        )));
    }
    for dependency in graph.get(job_id).into_iter().flatten() {
        visit_job(dependency, graph, visiting, visited)?;
    }
    visiting.remove(job_id);
    visited.insert(job_id.to_owned());
    Ok(())
}

fn previous_non_terminal_in_lane(state: &JobManagerState, lane: &str) -> Option<JobId> {
    state
        .jobs
        .values()
        .filter(|record| {
            record.serial_lane.as_deref() == Some(lane) && !record.status.is_terminal()
        })
        .max_by_key(|record| record.created_at_ms)
        .map(|record| record.job_id.clone())
}

fn push_unique_job_id(dependencies: &mut Vec<JobId>, job_id: JobId) {
    if !dependencies
        .iter()
        .any(|existing| existing.as_str() == job_id.as_str())
    {
        dependencies.push(job_id);
    }
}

fn dependencies_satisfied(state: &JobManagerState, record: &JobRecord) -> bool {
    record.dependencies.iter().all(|dependency| {
        let Some(dependency) = state.jobs.get(dependency.as_str()) else {
            return false;
        };
        match record.dependency_policy {
            JobDependencyPolicy::AllSucceeded => dependency.status == JobStatus::Succeeded,
            JobDependencyPolicy::AllTerminal => dependency.status.is_terminal(),
        }
    })
}

fn build_read_response(state: &JobManagerState, params: &ReadJobsParams) -> ReadJobsResponse {
    ReadJobsResponse {
        jobs: params
            .jobs
            .iter()
            .map(|job_id| match state.jobs.get(job_id.as_str()) {
                Some(record) => {
                    let (output_chunks, output_next_seq) =
                        select_output_chunks(record, params.after_seq, params.max_bytes);
                    JobReadResult {
                        summary: record.summary(),
                        output_chunks,
                        output_next_seq,
                        artifacts: Vec::new(),
                    }
                }
                None => JobReadResult {
                    summary: lost_summary(job_id.as_str()),
                    output_chunks: Vec::new(),
                    output_next_seq: 0,
                    artifacts: Vec::new(),
                },
            })
            .collect(),
    }
}

fn select_output_chunks(
    record: &JobRecord,
    after_seq: Option<u64>,
    max_bytes: Option<usize>,
) -> (Vec<JobOutputChunk>, u64) {
    let after_seq = after_seq.unwrap_or(0);
    let mut chunks = Vec::new();
    let mut bytes = 0usize;
    let mut next_seq = after_seq;

    for chunk in record
        .output_chunks
        .iter()
        .filter(|chunk| chunk.seq >= after_seq)
    {
        let chunk_bytes = chunk.chunk.as_slice();
        if let Some(max_bytes) = max_bytes {
            if bytes >= max_bytes {
                break;
            }
            let remaining = max_bytes - bytes;
            if chunk_bytes.len() > remaining {
                chunks.push(JobOutputChunk {
                    seq: chunk.seq,
                    stream: chunk.stream,
                    chunk: ByteChunk::from(chunk_bytes[..remaining].to_vec()),
                });
                next_seq = chunk.seq + 1;
                break;
            }
        }
        bytes += chunk_bytes.len();
        next_seq = chunk.seq + 1;
        chunks.push(chunk.clone());
    }

    if chunks.is_empty() && next_seq < record.output_next_seq {
        next_seq = record.output_next_seq;
    }

    (chunks, next_seq)
}

fn resolve_cancel_scope(state: &JobManagerState, params: &CancelJobsParams) -> Vec<String> {
    let mut target_ids = params
        .jobs
        .iter()
        .map(|job_id| job_id.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    match params.scope {
        JobCancelScope::Job => {}
        JobCancelScope::Dependents => {
            let mut changed = true;
            while changed {
                changed = false;
                for (job_id, record) in &state.jobs {
                    if record.status.is_terminal() || target_ids.contains(job_id) {
                        continue;
                    }
                    if record
                        .dependencies
                        .iter()
                        .any(|dependency| target_ids.contains(dependency.as_str()))
                    {
                        changed |= target_ids.insert(job_id.clone());
                    }
                }
            }
        }
        JobCancelScope::Deck => {
            let deck_ids = params
                .jobs
                .iter()
                .filter_map(|job_id| state.jobs.get(job_id.as_str()))
                .filter_map(|record| record.deck_id.clone())
                .collect::<BTreeSet<_>>();
            for (job_id, record) in &state.jobs {
                if record
                    .deck_id
                    .as_ref()
                    .is_some_and(|deck_id| deck_ids.contains(deck_id))
                {
                    target_ids.insert(job_id.clone());
                }
            }
        }
    }

    target_ids.into_iter().collect()
}

fn job_spec_hash(
    params: &StartJobsParams,
    spec: &JobStartSpec,
    dependencies: &[JobId],
    serial_lane: Option<&str>,
) -> Result<String, HostError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct JobHashMaterial<'a> {
        deck_id: &'a Option<String>,
        idempotency_key: &'a Option<String>,
        mode: JobStartMode,
        serial_lane: Option<&'a str>,
        job: JobHashSpec<'a>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct JobHashSpec<'a> {
        job_id: &'a JobId,
        name: &'a Option<String>,
        argv: &'a [String],
        cwd: &'a Option<HostPath>,
        env: &'a BTreeMap<String, String>,
        stdin: &'a Option<ByteChunk>,
        timeout_ms: Option<u64>,
        dependencies: &'a [JobId],
        dependency_policy: JobDependencyPolicy,
        output_policy: &'a Option<host_protocol::data::jobs::JobOutputPolicy>,
        metadata: &'a BTreeMap<String, String>,
    }

    let material = JobHashMaterial {
        deck_id: &params.deck_id,
        idempotency_key: &params.idempotency_key,
        mode: params.mode,
        serial_lane,
        job: JobHashSpec {
            job_id: &spec.job_id,
            name: &spec.name,
            argv: &spec.argv,
            cwd: &spec.cwd,
            env: &spec.env,
            stdin: &spec.stdin,
            timeout_ms: spec.timeout_ms,
            dependencies,
            dependency_policy: spec.dependency_policy,
            output_policy: &spec.output_policy,
            metadata: &spec.metadata,
        },
    };
    let bytes = serde_json::to_vec(&material).map_err(|error| {
        HostError::new(
            HostErrorCode::Internal,
            format!("encode job idempotency hash: {error}"),
        )
    })?;
    Ok(format!("{:016x}", fnv1a64(&bytes)))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

impl JobRecord {
    fn summary(&self) -> JobSummary {
        JobSummary {
            job_id: self.job_id.clone(),
            deck_id: self.deck_id.clone(),
            name: self.name.clone(),
            status: self.status,
            dependencies: self.dependencies.clone(),
            created_at_ms: self.created_at_ms,
            queued_at_ms: self.queued_at_ms,
            started_at_ms: self.started_at_ms,
            finished_at_ms: self.finished_at_ms,
            exit_code: self.exit_code,
            failure: self.failure.clone(),
            serial_lane: self.serial_lane.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

fn lost_summary(job_id: &str) -> JobSummary {
    JobSummary {
        job_id: JobId::new(job_id.to_owned()),
        deck_id: None,
        name: None,
        status: JobStatus::Lost,
        dependencies: Vec::new(),
        created_at_ms: 0,
        queued_at_ms: None,
        started_at_ms: None,
        finished_at_ms: Some(now_ms()),
        exit_code: None,
        failure: Some("unknown job id".to_owned()),
        serial_lane: None,
        metadata: BTreeMap::new(),
    }
}

fn persist_record_at(jobs_root: &PathBuf, record: &JobRecord) -> anyhow::Result<()> {
    std::fs::create_dir_all(jobs_root)?;
    let path = jobs_root.join(format!("{}.json", record.job_id.as_str()));
    let temp_path = jobs_root.join(format!("{}.json.tmp", record.job_id.as_str()));
    let bytes = serde_json::to_vec_pretty(record)?;
    std::fs::write(&temp_path, bytes)?;
    std::fs::rename(temp_path, path)?;
    Ok(())
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    normalized
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

fn validate_optional_nonempty(label: &str, value: Option<&str>) -> Result<(), HostError> {
    if let Some(value) = value {
        validate_nonempty(label, value)?;
    }
    Ok(())
}

fn validate_nonempty(label: &str, value: &str) -> Result<(), HostError> {
    if value.trim().is_empty() {
        return Err(invalid_request(format!("{label} must not be empty")));
    }
    Ok(())
}

fn invalid_request(message: impl Into<String>) -> HostError {
    HostError::new(HostErrorCode::InvalidRequest, message)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, time::Duration};

    use host_protocol::{
        data::jobs::{
            JobCancelScope, JobDependency, JobDependencyPolicy, JobStartMode, JobStatus,
            ReadJobsParams, StartJobsParams,
        },
        shared::JobId,
    };

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn job_manager_runs_parallel_jobs_and_retains_output() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = JobManager::new(root.clone(), root).expect("manager");

        let response = manager
            .start_jobs(StartJobsParams {
                deck_id: Some("deck-parallel".to_owned()),
                jobs: vec![job("job-a", "printf a"), job("job-b", "printf b")],
                mode: JobStartMode::Parallel,
                serial_lane: None,
                idempotency_key: None,
                metadata: BTreeMap::new(),
            })
            .await
            .expect("start");

        assert_eq!(response.jobs.len(), 2);
        let read = wait_for_jobs(&manager, ["job-a", "job-b"]).await;
        assert!(
            read.jobs
                .iter()
                .all(|job| job.summary.status == JobStatus::Succeeded)
        );
        let output = read
            .jobs
            .iter()
            .flat_map(|job| {
                job.output_chunks
                    .iter()
                    .flat_map(|chunk| chunk.chunk.as_slice().to_vec())
            })
            .collect::<Vec<_>>();
        assert!(output.contains(&b'a'));
        assert!(output.contains(&b'b'));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_manager_honors_serial_lane_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let marker = root.join("order.txt");
        let manager = JobManager::new(root.clone(), root.clone()).expect("manager");

        manager
            .start_jobs(StartJobsParams {
                deck_id: Some("deck-serial".to_owned()),
                jobs: vec![
                    job("serial-1", "printf 1 >> order.txt"),
                    job("serial-2", "printf 2 >> order.txt"),
                    job("serial-3", "printf 3 >> order.txt"),
                ],
                mode: JobStartMode::Serial,
                serial_lane: Some("lane-a".to_owned()),
                idempotency_key: None,
                metadata: BTreeMap::new(),
            })
            .await
            .expect("start");

        let read = wait_for_jobs(&manager, ["serial-1", "serial-2", "serial-3"]).await;
        assert!(
            read.jobs
                .iter()
                .all(|job| job.summary.status == JobStatus::Succeeded)
        );
        assert_eq!(std::fs::read_to_string(marker).expect("marker"), "123");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_manager_honors_explicit_dependencies_and_dependency_failure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = JobManager::new(root.clone(), root).expect("manager");

        let mut dependent = job("dependent", "printf should-not-run");
        dependent.depends_on = vec![JobDependency::name("setup")];
        manager
            .start_jobs(StartJobsParams {
                deck_id: Some("deck-deps".to_owned()),
                jobs: vec![named_job("setup", "setup", "exit 7"), dependent],
                mode: JobStartMode::Parallel,
                serial_lane: None,
                idempotency_key: None,
                metadata: BTreeMap::new(),
            })
            .await
            .expect("start");

        let read = wait_for_jobs(&manager, ["setup", "dependent"]).await;
        let setup = result(&read, "setup");
        let dependent = result(&read, "dependent");
        assert_eq!(setup.summary.status, JobStatus::Failed);
        assert_eq!(dependent.summary.status, JobStatus::DependencyFailed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_manager_all_terminal_dependencies_run_after_failed_dependency() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = JobManager::new(root.clone(), root).expect("manager");

        let mut cleanup = job("cleanup", "printf cleanup");
        cleanup.depends_on = vec![JobDependency::job_id("fails")];
        cleanup.dependency_policy = JobDependencyPolicy::AllTerminal;
        manager
            .start_jobs(StartJobsParams {
                deck_id: Some("deck-cleanup".to_owned()),
                jobs: vec![job("fails", "exit 2"), cleanup],
                mode: JobStartMode::Parallel,
                serial_lane: None,
                idempotency_key: None,
                metadata: BTreeMap::new(),
            })
            .await
            .expect("start");

        let read = wait_for_jobs(&manager, ["fails", "cleanup"]).await;
        assert_eq!(result(&read, "fails").summary.status, JobStatus::Failed);
        assert_eq!(
            result(&read, "cleanup").summary.status,
            JobStatus::Succeeded
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_manager_cancels_running_jobs_and_queued_dependents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = JobManager::new(root.clone(), root).expect("manager");

        let mut dependent = job("after", "printf after");
        dependent.depends_on = vec![JobDependency::job_id("sleep")];
        manager
            .start_jobs(StartJobsParams {
                deck_id: Some("deck-cancel".to_owned()),
                jobs: vec![job("sleep", "sleep 5"), dependent],
                mode: JobStartMode::Parallel,
                serial_lane: None,
                idempotency_key: None,
                metadata: BTreeMap::new(),
            })
            .await
            .expect("start");

        tokio::time::sleep(Duration::from_millis(50)).await;
        manager
            .cancel_jobs(CancelJobsParams {
                jobs: vec![JobId::new("sleep")],
                scope: JobCancelScope::Dependents,
                force: false,
            })
            .await
            .expect("cancel");
        let read = wait_for_jobs(&manager, ["sleep", "after"]).await;
        assert_eq!(result(&read, "sleep").summary.status, JobStatus::Cancelled);
        assert_eq!(result(&read, "after").summary.status, JobStatus::Cancelled);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_manager_times_out_jobs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = JobManager::new(root.clone(), root).expect("manager");
        let mut spec = job("timeout", "sleep 5");
        spec.timeout_ms = Some(50);

        manager
            .start_jobs(StartJobsParams {
                deck_id: Some("deck-timeout".to_owned()),
                jobs: vec![spec],
                mode: JobStartMode::Parallel,
                serial_lane: None,
                idempotency_key: None,
                metadata: BTreeMap::new(),
            })
            .await
            .expect("start");

        let read = wait_for_jobs(&manager, ["timeout"]).await;
        assert_eq!(result(&read, "timeout").summary.status, JobStatus::TimedOut);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_manager_retries_same_start_idempotently_and_rejects_conflict() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = JobManager::new(root.clone(), root).expect("manager");
        let params = StartJobsParams {
            deck_id: Some("deck-retry".to_owned()),
            jobs: vec![job("retry", "printf once")],
            mode: JobStartMode::Parallel,
            serial_lane: None,
            idempotency_key: Some("idem".to_owned()),
            metadata: BTreeMap::new(),
        };

        manager.start_jobs(params.clone()).await.expect("first");
        manager.start_jobs(params).await.expect("retry");
        let conflict = manager
            .start_jobs(StartJobsParams {
                deck_id: Some("deck-retry".to_owned()),
                jobs: vec![job("retry", "printf different")],
                mode: JobStartMode::Parallel,
                serial_lane: None,
                idempotency_key: Some("idem".to_owned()),
                metadata: BTreeMap::new(),
            })
            .await
            .expect_err("conflict");
        assert_eq!(conflict.code, HostErrorCode::Conflict);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_manager_marks_unfinished_persisted_jobs_interrupted_on_startup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let jobs_root = root.join(".lightspeed").join("jobs");
        std::fs::create_dir_all(&jobs_root).expect("jobs root");
        persist_record_at(
            &jobs_root,
            &JobRecord {
                job_id: JobId::new("recover"),
                deck_id: Some("deck-recover".to_owned()),
                name: None,
                argv: vec!["sleep".to_owned(), "5".to_owned()],
                cwd: None,
                env: BTreeMap::new(),
                stdin: None,
                timeout_ms: None,
                dependency_policy: JobDependencyPolicy::AllSucceeded,
                dependencies: Vec::new(),
                metadata: BTreeMap::new(),
                serial_lane: None,
                spec_hash: "seed".to_owned(),
                status: JobStatus::Running,
                created_at_ms: now_ms(),
                queued_at_ms: Some(now_ms()),
                started_at_ms: Some(now_ms()),
                finished_at_ms: None,
                exit_code: None,
                failure: None,
                output_chunks: Vec::new(),
                output_next_seq: 0,
            },
        )
        .expect("persist seed");

        let recovered = JobManager::new(root.clone(), root).expect("recovered manager");
        let read = recovered
            .read_jobs(ReadJobsParams {
                jobs: vec![JobId::new("recover")],
                after_seq: None,
                max_bytes: None,
                include_artifacts: false,
                wait_ms: None,
            })
            .await
            .expect("read");
        assert_eq!(
            result(&read, "recover").summary.status,
            JobStatus::Interrupted
        );
    }

    fn job(job_id: &str, shell: &str) -> JobStartSpec {
        named_job(job_id, job_id, shell)
    }

    fn named_job(job_id: &str, name: &str, shell: &str) -> JobStartSpec {
        JobStartSpec {
            job_id: JobId::new(job_id),
            name: Some(name.to_owned()),
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), shell.to_owned()],
            cwd: None,
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: Some(5_000),
            depends_on: Vec::new(),
            dependency_policy: JobDependencyPolicy::AllSucceeded,
            output_policy: None,
            metadata: BTreeMap::new(),
        }
    }

    async fn wait_for_jobs<const N: usize>(
        manager: &JobManager,
        job_ids: [&str; N],
    ) -> ReadJobsResponse {
        manager
            .read_jobs(ReadJobsParams {
                jobs: job_ids.iter().map(|job_id| JobId::new(*job_id)).collect(),
                after_seq: None,
                max_bytes: None,
                include_artifacts: false,
                wait_ms: Some(5_000),
            })
            .await
            .expect("read jobs")
    }

    fn result<'a>(response: &'a ReadJobsResponse, job_id: &str) -> &'a JobReadResult {
        response
            .jobs
            .iter()
            .find(|result| result.summary.job_id.as_str() == job_id)
            .expect("job result")
    }
}

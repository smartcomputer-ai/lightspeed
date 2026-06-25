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
        JobOutputStream, JobReadResult, JobStartSpec, JobStatus, JobSummary, ListJobsParams,
        ListJobsResponse, ReadJobsParams, ReadJobsResponse, StartJobsParams, StartJobsResponse,
    },
    error::{HostError, HostErrorCode},
    shared::{ByteChunk, HostPath, JobId, SecretString},
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
    jobs: BTreeMap<JobKey, JobRecord>,
    running: BTreeMap<JobKey, Arc<RunningJob>>,
    secret_envs: BTreeMap<JobKey, BTreeMap<String, SecretString>>,
    next_accept_seq: u64,
}

type JobKey = (String, JobId);

struct RunningJob {
    cancel: Notify,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobRecord {
    namespace: String,
    job_id: JobId,
    name: Option<String>,
    argv: Vec<String>,
    cwd: Option<HostPath>,
    env: BTreeMap<String, String>,
    #[serde(default)]
    secret_env_names: Vec<String>,
    stdin: Option<ByteChunk>,
    timeout_ms: Option<u64>,
    dependency_policy: JobDependencyPolicy,
    dependencies: Vec<JobId>,
    queue_key: Option<String>,
    spec_hash: String,
    #[serde(default)]
    accept_seq: u64,
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
    secret_env: BTreeMap<String, SecretString>,
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
            state.next_accept_seq = state
                .next_accept_seq
                .max(record.accept_seq.saturating_add(1));
            state
                .jobs
                .insert(job_key(&record.namespace, &record.job_id), record);
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
        let mut accepted = Vec::new();
        {
            let mut state = self.state.lock().await;
            for job in &resolved {
                let key = job_key(&params.namespace, &job.spec.job_id);
                if state.jobs.contains_key(&key) {
                    state
                        .secret_envs
                        .entry(key.clone())
                        .or_insert_with(|| job.secret_env.clone());
                    accepted.push(key);
                    continue;
                }
                let accept_seq = state.next_accept_seq;
                state.next_accept_seq = state.next_accept_seq.saturating_add(1);

                let record = JobRecord {
                    namespace: params.namespace.clone(),
                    job_id: job.spec.job_id.clone(),
                    name: job.spec.name.clone(),
                    argv: job.spec.argv.clone(),
                    cwd: job.spec.cwd.clone(),
                    env: job.spec.env.clone(),
                    secret_env_names: job.secret_env.keys().cloned().collect(),
                    stdin: job.spec.stdin.clone(),
                    timeout_ms: job.spec.timeout_ms,
                    dependency_policy: job.spec.dependency_policy,
                    dependencies: job.dependencies.clone(),
                    queue_key: job.spec.queue_key.clone(),
                    spec_hash: job.spec_hash.clone(),
                    accept_seq,
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
                accepted.push(key.clone());
                state
                    .secret_envs
                    .insert(key.clone(), job.secret_env.clone());
                state.jobs.insert(key, record);
            }
        }

        self.schedule_ready_jobs().await;

        let state = self.state.lock().await;
        Ok(StartJobsResponse {
            jobs: accepted
                .into_iter()
                .map(|key| {
                    state
                        .jobs
                        .get(&key)
                        .map(JobRecord::summary)
                        .unwrap_or_else(|| lost_summary(&key.0, &key.1))
                })
                .collect(),
        })
    }

    pub async fn read_jobs(&self, params: ReadJobsParams) -> Result<ReadJobsResponse, HostError> {
        validate_id_component("namespace", &params.namespace)?;
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

    pub async fn list_jobs(&self, params: ListJobsParams) -> Result<ListJobsResponse, HostError> {
        validate_id_component("namespace", &params.namespace)?;
        if matches!(params.limit, Some(0)) {
            return Err(HostError::new(
                HostErrorCode::InvalidRequest,
                "job/list limit must be greater than zero",
            ));
        }
        let state = self.state.lock().await;
        let mut jobs = state
            .jobs
            .values()
            .filter(|record| record.namespace == params.namespace)
            .map(JobRecord::summary)
            .collect::<Vec<_>>();
        jobs.sort_by(|left, right| {
            right
                .created_at_ms
                .cmp(&left.created_at_ms)
                .then_with(|| left.job_id.cmp(&right.job_id))
        });
        if let Some(limit) = params.limit {
            jobs.truncate(limit);
        }
        Ok(ListJobsResponse { jobs })
    }

    pub async fn cancel_jobs(
        &self,
        params: CancelJobsParams,
    ) -> Result<CancelJobsResponse, HostError> {
        validate_id_component("namespace", &params.namespace)?;
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
            for key in &target_ids {
                let Some(status) = state.jobs.get(key).map(|record| record.status) else {
                    continue;
                };
                if status.is_terminal() {
                    continue;
                }
                let running = state.running.get(key).cloned();
                let Some(record) = state.jobs.get_mut(key) else {
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
                .map(|key| {
                    state
                        .jobs
                        .get(key)
                        .map(JobRecord::summary)
                        .unwrap_or_else(|| lost_summary(&key.0, &key.1))
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

                let mut busy_queues = state
                    .running
                    .keys()
                    .filter_map(|key| state.jobs.get(key))
                    .filter_map(|record| {
                        record
                            .queue_key
                            .as_ref()
                            .map(|queue_key| (record.namespace.clone(), queue_key.clone()))
                    })
                    .collect::<BTreeSet<_>>();
                let mut candidates = state
                    .jobs
                    .iter()
                    .filter_map(|(key, record)| {
                        if !matches!(record.status, JobStatus::Accepted | JobStatus::Queued)
                            || state.running.contains_key(key)
                            || !dependencies_satisfied(&state, record)
                        {
                            return None;
                        }
                        Some((
                            key.clone(),
                            record.accept_seq,
                            record.queued_at_ms.unwrap_or(record.created_at_ms),
                            record.created_at_ms,
                            record.job_id.clone(),
                        ))
                    })
                    .collect::<Vec<_>>();
                candidates.sort_by(|left, right| {
                    left.1
                        .cmp(&right.1)
                        .then_with(|| left.2.cmp(&right.2))
                        .then_with(|| left.3.cmp(&right.3))
                        .then_with(|| left.4.cmp(&right.4))
                });

                let mut ready_ids = Vec::new();
                for (key, _, _, _, _) in candidates {
                    let Some(record) = state.jobs.get(&key) else {
                        continue;
                    };
                    if let Some(queue_key) = record.queue_key.as_ref() {
                        if !busy_queues.insert((record.namespace.clone(), queue_key.clone())) {
                            continue;
                        }
                    }
                    ready_ids.push(key.clone());
                }
                let now = now_ms();
                let mut ready = Vec::new();
                for key in ready_ids {
                    let mut ready_record = None;
                    if let Some(record) = state.jobs.get_mut(&key) {
                        record.status = JobStatus::Running;
                        record.started_at_ms = Some(now);
                        record.failure = None;
                        if let Err(error) = self.persist_record(record) {
                            eprintln!(
                                "host-bridge failed to persist running job {}: {error:?}",
                                job_key_id(&key)
                            );
                        }
                        ready_record = Some(record.clone());
                    }
                    if let Some(record) = ready_record {
                        let secret_env = state.secret_envs.get(&key).cloned().unwrap_or_default();
                        let running = Arc::new(RunningJob {
                            cancel: Notify::new(),
                        });
                        state.running.insert(key, running.clone());
                        ready.push((record.clone(), running, secret_env));
                    }
                }
                ready
            };

            if ready.is_empty() {
                break;
            }
            for (record, running, secret_env) in ready {
                let manager = self.clone();
                tokio::spawn(async move {
                    manager.run_job(record, running, secret_env).await;
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
                                .get(&job_key(&record.namespace, dependency))
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

    async fn run_job(
        &self,
        record: JobRecord,
        running: Arc<RunningJob>,
        secret_env: BTreeMap<String, SecretString>,
    ) {
        let result = self
            .spawn_and_wait(record.clone(), running.clone(), secret_env)
            .await;
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
            let key = job_key(&record.namespace, &record.job_id);
            state.running.remove(&key);
            state.secret_envs.remove(&key);
            if let Some(record) = state.jobs.get_mut(&key) {
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
        secret_env: BTreeMap<String, SecretString>,
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
        for name in secret_env.keys() {
            if record.env.contains_key(name) {
                return Err(HostError::new(
                    HostErrorCode::InvalidRequest,
                    format!("job env collides with secret env: {name}"),
                ));
            }
        }
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
        for (name, value) in &secret_env {
            command.env(name, value.expose());
        }

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
                record.namespace.clone(),
                record.job_id.clone(),
                stdout,
                JobOutputStream::Stdout,
                redactions_for_secret_env(&secret_env),
            ))
        });
        let stderr_task = stderr.map(|stderr| {
            tokio::spawn(read_job_stream(
                self.clone(),
                record.namespace.clone(),
                record.job_id.clone(),
                stderr,
                JobOutputStream::Stderr,
                redactions_for_secret_env(&secret_env),
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
    namespace: String,
    job_id: JobId,
    mut reader: R,
    stream: JobOutputStream,
    redactions: Vec<Vec<u8>>,
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
                if let Some(record) = state.jobs.get_mut(&job_key(&namespace, &job_id)) {
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
        if let Some(record) = state.jobs.get_mut(&job_key(&namespace, &job_id)) {
            let seq = record.output_next_seq;
            record.output_next_seq += 1;
            record.output_chunks.push(JobOutputChunk {
                seq,
                stream,
                chunk: ByteChunk::from(redact_bytes(&buffer[..read], &redactions)),
            });
            if let Err(error) = manager.persist_record(record) {
                eprintln!("host-bridge failed to persist output for {job_id}: {error:?}");
            }
        }
        manager.notify.notify_waiters();
    }
}

fn redactions_for_secret_env(secret_env: &BTreeMap<String, SecretString>) -> Vec<Vec<u8>> {
    secret_env
        .values()
        .filter(|value| !value.is_empty())
        .map(|value| value.expose().as_bytes().to_vec())
        .collect()
}

fn redact_bytes(bytes: &[u8], redactions: &[Vec<u8>]) -> Vec<u8> {
    let mut output = bytes.to_vec();
    for secret in redactions {
        if secret.is_empty() || secret.len() > output.len() {
            continue;
        }
        let mut index = 0;
        while let Some(offset) = find_subslice(&output[index..], secret) {
            let start = index + offset;
            let end = start + secret.len();
            output.splice(start..end, b"<redacted>".iter().copied());
            index = start + b"<redacted>".len();
        }
    }
    output
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn validate_and_resolve_start(
    state: &JobManagerState,
    params: &StartJobsParams,
) -> Result<Vec<ResolvedJob>, HostError> {
    if params.jobs.is_empty() {
        return Err(invalid_request("job/start requires at least one job"));
    }
    validate_id_component("namespace", &params.namespace)?;
    validate_id_component("request_id", &params.request_id)?;

    let mut seen_job_ids = BTreeSet::new();
    let mut seen_names = BTreeMap::new();
    for spec in &params.jobs {
        validate_id_component("job id", spec.job_id.as_str())?;
        if spec.argv.is_empty() {
            return Err(invalid_request(format!(
                "job {} argv must not be empty",
                spec.job_id
            )));
        }
        for name in spec.secret_env.keys() {
            validate_nonempty("secret env name", name)?;
            if spec.env.contains_key(name) {
                return Err(invalid_request(format!(
                    "job {} env collides with secret env: {name}",
                    spec.job_id
                )));
            }
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
        validate_optional_id_component("queue_key", spec.queue_key.as_deref())?;
    }

    let request_job_ids = params
        .jobs
        .iter()
        .map(|spec| spec.job_id.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    let mut resolved = Vec::new();
    for spec in &params.jobs {
        let dependencies = resolve_explicit_dependencies(&seen_names, spec)?;
        for dependency in &dependencies {
            if !request_job_ids.contains(dependency.as_str())
                && !state
                    .jobs
                    .contains_key(&job_key(&params.namespace, dependency))
            {
                return Err(invalid_request(format!(
                    "job {} depends on unknown job id {}",
                    spec.job_id, dependency
                )));
            }
        }

        let spec_hash = job_spec_hash(params, spec, &dependencies)?;
        let key = job_key(&params.namespace, &spec.job_id);
        if let Some(existing) = state.jobs.get(&key) {
            if existing.spec_hash != spec_hash {
                return Err(HostError::new(
                    HostErrorCode::Conflict,
                    format!(
                        "job id already exists with different input in namespace {}: {}",
                        params.namespace, spec.job_id
                    ),
                ));
            }
        }

        resolved.push(ResolvedJob {
            spec: spec.clone(),
            dependencies,
            secret_env: spec.secret_env.clone(),
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
        let Some(dependency) = state.jobs.get(&job_key(&record.namespace, dependency)) else {
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
            .map(
                |job_id| match state.jobs.get(&job_key(&params.namespace, job_id)) {
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
                        summary: lost_summary(&params.namespace, job_id),
                        output_chunks: Vec::new(),
                        output_next_seq: 0,
                        artifacts: Vec::new(),
                    },
                },
            )
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

fn resolve_cancel_scope(state: &JobManagerState, params: &CancelJobsParams) -> Vec<JobKey> {
    let mut target_ids = params
        .jobs
        .iter()
        .map(|job_id| job_key(&params.namespace, job_id))
        .collect::<BTreeSet<_>>();
    match params.scope {
        JobCancelScope::Job => {}
        JobCancelScope::Dependents => {
            let mut changed = true;
            while changed {
                changed = false;
                for (key, record) in &state.jobs {
                    if record.namespace != params.namespace
                        || record.status.is_terminal()
                        || target_ids.contains(key)
                    {
                        continue;
                    }
                    if record.dependencies.iter().any(|dependency| {
                        target_ids.contains(&job_key(&params.namespace, dependency))
                    }) {
                        changed |= target_ids.insert(key.clone());
                    }
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
) -> Result<String, HostError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct JobHashMaterial<'a> {
        namespace: &'a str,
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
        secret_env_names: Vec<&'a String>,
        stdin: &'a Option<ByteChunk>,
        timeout_ms: Option<u64>,
        dependencies: &'a [JobId],
        dependency_policy: JobDependencyPolicy,
        queue_key: &'a Option<String>,
    }

    let material = JobHashMaterial {
        namespace: params.namespace.as_str(),
        job: JobHashSpec {
            job_id: &spec.job_id,
            name: &spec.name,
            argv: &spec.argv,
            cwd: &spec.cwd,
            env: &spec.env,
            secret_env_names: spec.secret_env.keys().collect(),
            stdin: &spec.stdin,
            timeout_ms: spec.timeout_ms,
            dependencies,
            dependency_policy: spec.dependency_policy,
            queue_key: &spec.queue_key,
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
            namespace: self.namespace.clone(),
            job_id: self.job_id.clone(),
            name: self.name.clone(),
            status: self.status,
            dependencies: self.dependencies.clone(),
            created_at_ms: self.created_at_ms,
            queued_at_ms: self.queued_at_ms,
            started_at_ms: self.started_at_ms,
            finished_at_ms: self.finished_at_ms,
            exit_code: self.exit_code,
            failure: self.failure.clone(),
            queue_key: self.queue_key.clone(),
        }
    }
}

fn lost_summary(namespace: &str, job_id: &JobId) -> JobSummary {
    JobSummary {
        namespace: namespace.to_owned(),
        job_id: job_id.clone(),
        name: None,
        status: JobStatus::Lost,
        dependencies: Vec::new(),
        created_at_ms: 0,
        queued_at_ms: None,
        started_at_ms: None,
        finished_at_ms: Some(now_ms()),
        exit_code: None,
        failure: Some("unknown job id".to_owned()),
        queue_key: None,
    }
}

fn persist_record_at(jobs_root: &PathBuf, record: &JobRecord) -> anyhow::Result<()> {
    std::fs::create_dir_all(jobs_root)?;
    let stem = record_file_stem(&record.namespace, &record.job_id);
    let path = jobs_root.join(format!("{stem}.json"));
    let temp_path = jobs_root.join(format!("{stem}.json.tmp"));
    let bytes = serde_json::to_vec_pretty(record)?;
    std::fs::write(&temp_path, bytes)?;
    std::fs::rename(temp_path, path)?;
    Ok(())
}

fn job_key(namespace: &str, job_id: &JobId) -> JobKey {
    (namespace.to_owned(), job_id.clone())
}

fn job_key_id(key: &JobKey) -> String {
    format!("{}/{}", key.0, key.1)
}

fn record_file_stem(namespace: &str, job_id: &JobId) -> String {
    format!("{namespace}--{}", job_id.as_str())
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

fn validate_optional_id_component(label: &str, value: Option<&str>) -> Result<(), HostError> {
    if let Some(value) = value {
        validate_id_component(label, value)?;
    }
    Ok(())
}

fn validate_id_component(label: &str, value: &str) -> Result<(), HostError> {
    validate_nonempty(label, value)?;
    if value.len() > 128 {
        return Err(invalid_request(format!(
            "{label} must be at most 128 bytes"
        )));
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(invalid_request(format!("{label} must not be empty")));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(invalid_request(format!(
            "{label} must start with an ASCII alphanumeric character"
        )));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-')) {
        return Err(invalid_request(format!(
            "{label} may contain only ASCII alphanumeric characters, '_', '.', ':', or '-'"
        )));
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
            JobCancelScope, JobDependency, JobDependencyPolicy, JobStatus, ListJobsParams,
            ReadJobsParams, StartJobsParams,
        },
        shared::{JobId, SecretString},
    };

    use super::*;

    const TEST_NAMESPACE: &str = "session_1";

    #[tokio::test(flavor = "current_thread")]
    async fn job_manager_runs_parallel_jobs_and_retains_output() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = JobManager::new(root.clone(), root).expect("manager");

        let response = manager
            .start_jobs(StartJobsParams {
                namespace: TEST_NAMESPACE.to_owned(),
                request_id: "parallel".to_owned(),
                jobs: vec![job("job-a", "printf a"), job("job-b", "printf b")],
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
    async fn job_manager_injects_secret_env_without_persisting_values() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = JobManager::new(root.clone(), root.clone()).expect("manager");
        let mut spec = job("secret-job", "printf \"$SECRET_TOKEN\"");
        spec.secret_env.insert(
            "SECRET_TOKEN".to_owned(),
            SecretString::new("super-secret-token"),
        );

        manager
            .start_jobs(StartJobsParams {
                namespace: TEST_NAMESPACE.to_owned(),
                request_id: "secret".to_owned(),
                jobs: vec![spec],
            })
            .await
            .expect("start");

        let read = wait_for_jobs(&manager, ["secret-job"]).await;
        let result = result(&read, "secret-job");
        assert_eq!(result.summary.status, JobStatus::Succeeded);
        let output = result
            .output_chunks
            .iter()
            .flat_map(|chunk| chunk.chunk.as_slice().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(output, b"<redacted>");

        let record_path = root
            .join(".lightspeed")
            .join("jobs")
            .join("session_1--secret-job.json");
        let persisted = std::fs::read_to_string(record_path).expect("persisted job record");
        assert!(!persisted.contains("super-secret-token"));
        assert!(persisted.contains("SECRET_TOKEN"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_manager_honors_queue_key_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let marker = root.join("order.txt");
        let manager = JobManager::new(root.clone(), root.clone()).expect("manager");

        let mut first = job("queued-z", "printf 1 >> order.txt");
        let mut second = job("queued-a", "printf 2 >> order.txt");
        let mut third = job("queued-m", "printf 3 >> order.txt");
        first.queue_key = Some("repo".to_owned());
        second.queue_key = Some("repo".to_owned());
        third.queue_key = Some("repo".to_owned());

        manager
            .start_jobs(StartJobsParams {
                namespace: TEST_NAMESPACE.to_owned(),
                request_id: "queue".to_owned(),
                jobs: vec![first, second, third],
            })
            .await
            .expect("start");

        let read = wait_for_jobs(&manager, ["queued-z", "queued-a", "queued-m"]).await;
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
                namespace: TEST_NAMESPACE.to_owned(),
                request_id: "deps".to_owned(),
                jobs: vec![named_job("setup", "setup", "exit 7"), dependent],
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
                namespace: TEST_NAMESPACE.to_owned(),
                request_id: "cleanup".to_owned(),
                jobs: vec![job("fails", "exit 2"), cleanup],
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
                namespace: TEST_NAMESPACE.to_owned(),
                request_id: "cancel".to_owned(),
                jobs: vec![job("sleep", "sleep 5"), dependent],
            })
            .await
            .expect("start");

        tokio::time::sleep(Duration::from_millis(50)).await;
        manager
            .cancel_jobs(CancelJobsParams {
                namespace: TEST_NAMESPACE.to_owned(),
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
                namespace: TEST_NAMESPACE.to_owned(),
                request_id: "timeout".to_owned(),
                jobs: vec![spec],
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
            namespace: TEST_NAMESPACE.to_owned(),
            request_id: "retry".to_owned(),
            jobs: vec![job("retry", "printf once")],
        };

        manager.start_jobs(params.clone()).await.expect("first");
        manager.start_jobs(params).await.expect("retry");
        let conflict = manager
            .start_jobs(StartJobsParams {
                namespace: TEST_NAMESPACE.to_owned(),
                request_id: "retry-conflict".to_owned(),
                jobs: vec![job("retry", "printf different")],
            })
            .await
            .expect_err("conflict");
        assert_eq!(conflict.code, HostErrorCode::Conflict);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_manager_lists_latest_jobs_with_limit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = JobManager::new(root.clone(), root).expect("manager");

        manager
            .start_jobs(StartJobsParams {
                namespace: TEST_NAMESPACE.to_owned(),
                request_id: "list-1".to_owned(),
                jobs: vec![job("job-old", "printf old")],
            })
            .await
            .expect("start old");
        tokio::time::sleep(Duration::from_millis(2)).await;
        manager
            .start_jobs(StartJobsParams {
                namespace: TEST_NAMESPACE.to_owned(),
                request_id: "list-2".to_owned(),
                jobs: vec![job("job-new", "printf new")],
            })
            .await
            .expect("start new");

        let listed = manager
            .list_jobs(ListJobsParams {
                namespace: TEST_NAMESPACE.to_owned(),
                limit: Some(1),
            })
            .await
            .expect("list jobs");

        assert_eq!(listed.jobs.len(), 1);
        assert_eq!(listed.jobs[0].job_id.as_str(), "job-new");
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
                namespace: TEST_NAMESPACE.to_owned(),
                job_id: JobId::new("recover"),
                name: None,
                argv: vec!["sleep".to_owned(), "5".to_owned()],
                cwd: None,
                env: BTreeMap::new(),
                secret_env_names: Vec::new(),
                stdin: None,
                timeout_ms: None,
                dependency_policy: JobDependencyPolicy::AllSucceeded,
                dependencies: Vec::new(),
                queue_key: None,
                spec_hash: "seed".to_owned(),
                accept_seq: 0,
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
                namespace: TEST_NAMESPACE.to_owned(),
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
            secret_env: BTreeMap::new(),
            stdin: None,
            timeout_ms: Some(5_000),
            depends_on: Vec::new(),
            dependency_policy: JobDependencyPolicy::AllSucceeded,
            queue_key: None,
        }
    }

    async fn wait_for_jobs<const N: usize>(
        manager: &JobManager,
        job_ids: [&str; N],
    ) -> ReadJobsResponse {
        manager
            .read_jobs(ReadJobsParams {
                namespace: TEST_NAMESPACE.to_owned(),
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

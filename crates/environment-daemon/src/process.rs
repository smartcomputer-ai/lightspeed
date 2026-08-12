use std::{
    collections::BTreeMap,
    io::{Read as _, Write as _},
    path::{Component, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use host_protocol::{
    data::process::{
        ProcessOutputChunk, ProcessOutputStream, ReadProcessParams, ReadProcessResponse,
        ResizeProcessParams, ResizeProcessResponse, StartProcessParams, StartProcessResponse,
        TerminateProcessParams, TerminateProcessResponse, WriteProcessParams, WriteProcessResponse,
        WriteProcessStatus,
    },
    error::{HostError, HostErrorCode},
    shared::{ByteChunk, HostPath, ProcessId},
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, Notify},
    task::JoinHandle,
    time::Instant,
};

use crate::process_group;

#[cfg(not(test))]
const TERMINAL_PROCESS_RETENTION: Duration = Duration::from_secs(60);
#[cfg(test)]
const TERMINAL_PROCESS_RETENTION: Duration = Duration::from_millis(10);

/// How often the exit watcher polls a running child. Readers only notify on
/// pipe events, and descendants holding the pipes can suppress EOF, so exit
/// observation must not depend on them.
#[cfg(not(test))]
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(test)]
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone)]
pub struct ProcessManager {
    cwd: PathBuf,
    fs_root: PathBuf,
    processes: Arc<Mutex<BTreeMap<String, Arc<ProcessEntry>>>>,
}

struct ProcessEntry {
    state: Mutex<ProcessState>,
    notify: Notify,
    readers: Mutex<Vec<JoinHandle<()>>>,
}

struct ProcessState {
    child: ProcessChild,
    pgid: Option<u32>,
    stdin: Option<ProcessInput>,
    pty_master: Option<Box<dyn MasterPty + Send>>,
    redactions: Vec<Vec<u8>>,
    chunks: Vec<ProcessOutputChunk>,
    next_seq: u64,
    exited: bool,
    exit_code: Option<i32>,
    orphaned_descendants: bool,
    failure: Option<String>,
    cleanup_scheduled: bool,
}

enum ProcessChild {
    Pipe(Child),
    Pty(Box<dyn portable_pty::Child + Send + Sync>),
}

enum ProcessInput {
    Pipe(ChildStdin),
    Pty(Box<dyn std::io::Write + Send>),
}

impl ProcessManager {
    pub fn new(cwd: PathBuf, fs_root: PathBuf) -> Self {
        Self {
            cwd: normalize_path(cwd),
            fs_root: normalize_path(fs_root),
            processes: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub async fn start_process(
        &self,
        params: StartProcessParams,
    ) -> Result<StartProcessResponse, HostError> {
        if params.argv.is_empty() {
            return Err(HostError::new(
                HostErrorCode::InvalidRequest,
                "process argv must not be empty",
            ));
        }
        let cwd = params
            .cwd
            .as_ref()
            .map(|path| self.resolve_cwd(path))
            .transpose()?
            .unwrap_or_else(|| self.cwd.clone());
        for name in params.secret_env.keys() {
            if params.env.contains_key(name) {
                return Err(HostError::new(
                    HostErrorCode::InvalidRequest,
                    format!("process env collides with secret env: {name}"),
                ));
            }
        }

        if params.tty {
            return self.start_pty_process(params, cwd).await;
        }

        let mut command = Command::new(&params.argv[0]);
        command
            .args(&params.argv[1..])
            .current_dir(&cwd)
            .envs(params.env.iter())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in &params.secret_env {
            command.env(name, value.expose());
        }
        if params.stdin.is_some() || params.pipe_stdin {
            command.stdin(Stdio::piped());
        } else {
            command.stdin(Stdio::null());
        }
        process_group::spawn_in_own_group(&mut command);

        let mut child = command.spawn().map_err(|error| {
            HostError::new(
                HostErrorCode::ProcessFailed,
                format!("spawn process {:?}: {error}", params.argv),
            )
        })?;
        let pgid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let mut stdin = child.stdin.take();
        if let Some(input) = params.stdin {
            let Some(writer) = stdin.as_mut() else {
                return Err(HostError::new(
                    HostErrorCode::ProcessFailed,
                    "process stdin was not available",
                ));
            };
            writer
                .write_all(input.as_slice())
                .await
                .map_err(|error| HostError::new(HostErrorCode::ProcessFailed, error.to_string()))?;
        }
        if !params.pipe_stdin {
            stdin.take();
        }

        let process_id = params.process_id;
        let redactions = params
            .secret_env
            .values()
            .filter(|value| !value.is_empty())
            .map(|value| value.expose().as_bytes().to_vec())
            .collect();
        let entry = Arc::new(ProcessEntry {
            state: Mutex::new(ProcessState {
                child: ProcessChild::Pipe(child),
                pgid,
                stdin: stdin.map(ProcessInput::Pipe),
                pty_master: None,
                redactions,
                chunks: Vec::new(),
                next_seq: 0,
                exited: false,
                exit_code: None,
                orphaned_descendants: false,
                failure: None,
                cleanup_scheduled: false,
            }),
            notify: Notify::new(),
            readers: Mutex::new(Vec::new()),
        });

        {
            let mut processes = self.processes.lock().await;
            if processes.contains_key(process_id.as_str()) {
                return Err(HostError::new(
                    HostErrorCode::Conflict,
                    format!("process id already exists: {process_id}"),
                ));
            }
            processes.insert(process_id.to_string(), entry.clone());
        }

        {
            let mut readers = entry.readers.lock().await;
            if let Some(stdout) = stdout {
                readers.push(tokio::spawn(read_stream(
                    entry.clone(),
                    stdout,
                    ProcessOutputStream::Stdout,
                )));
            }
            if let Some(stderr) = stderr {
                readers.push(tokio::spawn(read_stream(
                    entry.clone(),
                    stderr,
                    ProcessOutputStream::Stderr,
                )));
            }
        }
        if let Some(timeout_ms) = params.timeout_ms {
            tokio::spawn(timeout_process(
                entry.clone(),
                Duration::from_millis(timeout_ms),
            ));
        }
        tokio::spawn(watch_exit(entry.clone()));

        Ok(StartProcessResponse { process_id })
    }

    async fn start_pty_process(
        &self,
        params: StartProcessParams,
        cwd: PathBuf,
    ) -> Result<StartProcessResponse, HostError> {
        let pty = native_pty_system()
            .openpty(PtySize::default())
            .map_err(process_error("open PTY"))?;
        let mut command = CommandBuilder::new(&params.argv[0]);
        command.args(&params.argv[1..]);
        command.cwd(&cwd);
        for (name, value) in &params.env {
            command.env(name, value);
        }
        for (name, value) in &params.secret_env {
            command.env(name, value.expose());
        }
        let child = pty
            .slave
            .spawn_command(command)
            .map_err(process_error("spawn PTY process"))?;
        let reader = pty
            .master
            .try_clone_reader()
            .map_err(process_error("clone PTY reader"))?;
        let mut stdin = pty
            .master
            .take_writer()
            .map_err(process_error("open PTY writer"))?;
        if let Some(input) = params.stdin.as_ref() {
            stdin
                .write_all(input.as_slice())
                .map_err(process_error("write initial PTY input"))?;
            stdin.flush().map_err(process_error("flush PTY input"))?;
        }
        let stdin = params.pipe_stdin.then_some(ProcessInput::Pty(stdin));
        let process_id = params.process_id;
        let redactions = params
            .secret_env
            .values()
            .filter(|value| !value.is_empty())
            .map(|value| value.expose().as_bytes().to_vec())
            .collect();
        let entry = Arc::new(ProcessEntry {
            state: Mutex::new(ProcessState {
                child: ProcessChild::Pty(child),
                pgid: None,
                stdin,
                pty_master: Some(pty.master),
                redactions,
                chunks: Vec::new(),
                next_seq: 0,
                exited: false,
                exit_code: None,
                orphaned_descendants: false,
                failure: None,
                cleanup_scheduled: false,
            }),
            notify: Notify::new(),
            readers: Mutex::new(Vec::new()),
        });
        {
            let mut processes = self.processes.lock().await;
            if processes.contains_key(process_id.as_str()) {
                return Err(HostError::new(
                    HostErrorCode::Conflict,
                    format!("process id already exists: {process_id}"),
                ));
            }
            processes.insert(process_id.to_string(), entry.clone());
        }
        entry
            .readers
            .lock()
            .await
            .push(read_pty_stream(entry.clone(), reader));
        if let Some(timeout_ms) = params.timeout_ms {
            tokio::spawn(timeout_process(
                entry.clone(),
                Duration::from_millis(timeout_ms),
            ));
        }
        tokio::spawn(watch_exit(entry));
        Ok(StartProcessResponse { process_id })
    }

    pub async fn read_process(
        &self,
        params: ReadProcessParams,
    ) -> Result<ReadProcessResponse, HostError> {
        let Some(entry) = self.entry(&params.process_id).await else {
            return Err(HostError::new(
                HostErrorCode::NotFound,
                format!("unknown process id: {}", params.process_id),
            ));
        };
        let after_seq = params.after_seq.unwrap_or(0);
        let deadline = params
            .wait_ms
            .map(|wait_ms| Instant::now() + Duration::from_millis(wait_ms));

        loop {
            let (response, schedule_cleanup) = {
                let mut state = entry.state.lock().await;
                if let Some(pgid) = update_exit_status(&mut state)? {
                    spawn_group_sweep(entry.clone(), pgid);
                }
                let response = response_from_state(&state, after_seq, params.max_bytes);
                let has_chunks = !response.chunks.is_empty();
                let done = response.exited || response.closed;
                let should_return = if params.wait_ms.is_some() {
                    has_chunks
                        || done
                        || deadline.is_some_and(|deadline| Instant::now() >= deadline)
                } else {
                    done
                };
                let schedule_cleanup = done && !state.cleanup_scheduled;
                if should_return {
                    if schedule_cleanup {
                        state.cleanup_scheduled = true;
                    }
                    (Some(response), schedule_cleanup)
                } else {
                    (None, false)
                }
            };
            if let Some(response) = response {
                if schedule_cleanup {
                    self.schedule_terminal_cleanup(params.process_id.clone(), entry.clone());
                }
                return Ok(response);
            }

            if let Some(deadline) = deadline {
                tokio::select! {
                    _ = entry.notify.notified() => {}
                    _ = tokio::time::sleep_until(deadline) => {}
                }
            } else {
                entry.notify.notified().await;
            }
        }
    }

    pub async fn write_process(
        &self,
        params: WriteProcessParams,
    ) -> Result<WriteProcessResponse, HostError> {
        let Some(entry) = self.entry(&params.process_id).await else {
            return Ok(WriteProcessResponse {
                status: WriteProcessStatus::UnknownProcess,
            });
        };
        let mut state = entry.state.lock().await;
        if let Some(pgid) = update_exit_status(&mut state)? {
            spawn_group_sweep(entry.clone(), pgid);
        }
        if state.exited {
            return Ok(WriteProcessResponse {
                status: WriteProcessStatus::StdinClosed,
            });
        }
        let Some(stdin) = state.stdin.as_mut() else {
            return Ok(WriteProcessResponse {
                status: WriteProcessStatus::StdinClosed,
            });
        };
        if let Some(chunk) = params.chunk {
            match stdin {
                ProcessInput::Pipe(stdin) => stdin.write_all(chunk.as_slice()).await,
                ProcessInput::Pty(stdin) => stdin.write_all(chunk.as_slice()),
            }
            .map_err(|error| HostError::new(HostErrorCode::ProcessFailed, error.to_string()))?;
        }
        if params.close_stdin {
            state.stdin.take();
        }
        Ok(WriteProcessResponse {
            status: WriteProcessStatus::Accepted,
        })
    }

    pub async fn terminate_process(
        &self,
        params: TerminateProcessParams,
    ) -> Result<TerminateProcessResponse, HostError> {
        let Some(entry) = self.entry(&params.process_id).await else {
            return Ok(TerminateProcessResponse { running: false });
        };
        let mut state = entry.state.lock().await;
        if let Some(pgid) = update_exit_status(&mut state)? {
            spawn_group_sweep(entry.clone(), pgid);
        }
        if state.exited {
            return Ok(TerminateProcessResponse { running: false });
        }
        if let Some(pgid) = state.pgid {
            process_group::kill_group(pgid);
        }
        kill_child(&mut state.child).await?;
        state.exited = true;
        state.exit_code = None;
        state.failure = Some("process terminated".to_owned());
        spawn_reader_drain(entry.clone());
        entry.notify.notify_waiters();
        Ok(TerminateProcessResponse { running: true })
    }

    pub async fn resize_process(
        &self,
        params: ResizeProcessParams,
    ) -> Result<ResizeProcessResponse, HostError> {
        let Some(entry) = self.entry(&params.process_id).await else {
            return Err(HostError::new(
                HostErrorCode::NotFound,
                format!("unknown process id: {}", params.process_id),
            ));
        };
        let state = entry.state.lock().await;
        let Some(master) = state.pty_master.as_ref() else {
            return Err(HostError::new(
                HostErrorCode::InvalidRequest,
                "process is not attached to a PTY",
            ));
        };
        master
            .resize(PtySize {
                rows: params.size.rows,
                cols: params.size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(process_error("resize PTY"))?;
        Ok(ResizeProcessResponse {})
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
                    "process cwd is outside bridge fs root: {} (root {})",
                    normalized.display(),
                    self.fs_root.display()
                ),
            ));
        }
        Ok(normalized)
    }

    async fn entry(&self, process_id: &ProcessId) -> Option<Arc<ProcessEntry>> {
        self.processes
            .lock()
            .await
            .get(process_id.as_str())
            .cloned()
    }

    fn schedule_terminal_cleanup(&self, process_id: ProcessId, entry: Arc<ProcessEntry>) {
        let processes = self.processes.clone();
        tokio::spawn(async move {
            tokio::time::sleep(TERMINAL_PROCESS_RETENTION).await;
            let removed = {
                let mut processes = processes.lock().await;
                if processes
                    .get(process_id.as_str())
                    .is_some_and(|current| Arc::ptr_eq(current, &entry))
                {
                    processes.remove(process_id.as_str());
                    true
                } else {
                    false
                }
            };
            if removed {
                // Readers stuck on pipes held by escaped descendants must not
                // keep accumulating output for a pruned process.
                for reader in entry.readers.lock().await.drain(..) {
                    reader.abort();
                }
            }
        });
    }
}

async fn read_stream<R>(entry: Arc<ProcessEntry>, mut reader: R, stream: ProcessOutputStream)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0; 8192];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => {
                entry.notify.notify_waiters();
                return;
            }
            Ok(read) => read,
            Err(error) => {
                let mut state = entry.state.lock().await;
                if state.failure.is_none() {
                    state.failure = Some(error.to_string());
                }
                entry.notify.notify_waiters();
                return;
            }
        };
        let mut state = entry.state.lock().await;
        let chunk = redact_bytes(&buffer[..read], &state.redactions);
        let seq = state.next_seq;
        state.next_seq += 1;
        state.chunks.push(ProcessOutputChunk {
            seq,
            stream,
            chunk: ByteChunk::from(chunk),
        });
        entry.notify.notify_waiters();
    }
}

fn read_pty_stream(
    entry: Arc<ProcessEntry>,
    mut reader: Box<dyn std::io::Read + Send>,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut buffer = vec![0; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    entry.notify.notify_waiters();
                    return;
                }
                Ok(read) => {
                    let entry = entry.clone();
                    let bytes = buffer[..read].to_vec();
                    tokio::runtime::Handle::current().block_on(async move {
                        let mut state = entry.state.lock().await;
                        let chunk = redact_bytes(&bytes, &state.redactions);
                        let seq = state.next_seq;
                        state.next_seq += 1;
                        state.chunks.push(ProcessOutputChunk {
                            seq,
                            stream: ProcessOutputStream::Pty,
                            chunk: ByteChunk::from(chunk),
                        });
                        entry.notify.notify_waiters();
                    });
                }
                Err(error) => {
                    let entry = entry.clone();
                    tokio::runtime::Handle::current().block_on(async move {
                        let mut state = entry.state.lock().await;
                        if state.failure.is_none() && !state.exited {
                            state.failure = Some(error.to_string());
                        }
                        entry.notify.notify_waiters();
                    });
                    return;
                }
            }
        }
    })
}

/// Observes the child's exit independently of pipe events so a silent child
/// whose descendants hold the pipes open still completes promptly.
async fn watch_exit(entry: Arc<ProcessEntry>) {
    loop {
        tokio::time::sleep(EXIT_POLL_INTERVAL).await;
        let (done, sweep) = {
            let mut state = entry.state.lock().await;
            if state.exited {
                (true, None)
            } else {
                match update_exit_status(&mut state) {
                    Ok(sweep) => (state.exited, sweep),
                    Err(_) => (true, None),
                }
            }
        };
        if let Some(pgid) = sweep {
            spawn_group_sweep(entry.clone(), pgid);
        }
        if done {
            entry.notify.notify_waiters();
            return;
        }
    }
}

async fn timeout_process(entry: Arc<ProcessEntry>, timeout: Duration) {
    tokio::time::sleep(timeout).await;
    {
        let mut state = entry.state.lock().await;
        if state.exited {
            return;
        }
        if let Some(pgid) = state.pgid {
            process_group::kill_group(pgid);
        }
        let _ = kill_child(&mut state.child).await;
        state.exited = true;
        state.exit_code = None;
        state.failure = Some("process timed out".to_owned());
    }
    drain_or_abort_readers(&entry).await;
    entry.notify.notify_waiters();
}

/// Polls the child for exit. When the exit is newly observed while
/// descendants are still alive in the process group, returns the group id so
/// the caller can spawn a sweep.
fn update_exit_status(state: &mut ProcessState) -> Result<Option<u32>, HostError> {
    if state.exited {
        return Ok(None);
    }
    let result = match &mut state.child {
        ProcessChild::Pipe(child) => child
            .try_wait()
            .map(|status| status.map(|status| status.code())),
        ProcessChild::Pty(child) => child
            .try_wait()
            .map(|status| status.map(|status| Some(status.exit_code() as i32))),
    };
    match result {
        Ok(Some(status)) => {
            state.exited = true;
            state.exit_code = status;
            state.stdin.take();
            let sweep = state.pgid.filter(|pgid| process_group::group_alive(*pgid));
            if sweep.is_some() {
                state.orphaned_descendants = true;
            }
            Ok(sweep)
        }
        Ok(None) => Ok(None),
        Err(error) => {
            state.failure = Some(error.to_string());
            Err(HostError::new(
                HostErrorCode::ProcessFailed,
                format!("poll process exit: {error}"),
            ))
        }
    }
}

async fn kill_child(child: &mut ProcessChild) -> Result<(), HostError> {
    let result = match child {
        ProcessChild::Pipe(child) => child.kill().await,
        ProcessChild::Pty(child) => child.kill(),
    };
    result.map_err(|error| HostError::new(HostErrorCode::ProcessFailed, error.to_string()))
}

fn process_error<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> HostError {
    move |error| HostError::new(HostErrorCode::ProcessFailed, format!("{context}: {error}"))
}

/// Sweeps descendants left in the group after the root process exited, then
/// drains or abandons the output readers so late orphan output cannot
/// accumulate unread.
fn spawn_group_sweep(entry: Arc<ProcessEntry>, pgid: u32) {
    tokio::spawn(async move {
        process_group::sweep_group(pgid).await;
        drain_or_abort_readers(&entry).await;
        entry.notify.notify_waiters();
    });
}

fn spawn_reader_drain(entry: Arc<ProcessEntry>) {
    tokio::spawn(async move {
        drain_or_abort_readers(&entry).await;
        entry.notify.notify_waiters();
    });
}

async fn drain_or_abort_readers(entry: &ProcessEntry) {
    let mut readers = std::mem::take(&mut *entry.readers.lock().await);
    let deadline = Instant::now() + process_group::OUTPUT_DRAIN_GRACE;
    for reader in &mut readers {
        if tokio::time::timeout_at(deadline, &mut *reader)
            .await
            .is_err()
        {
            reader.abort();
        }
    }
}

fn response_from_state(
    state: &ProcessState,
    after_seq: u64,
    max_bytes: Option<usize>,
) -> ReadProcessResponse {
    let mut chunks = Vec::new();
    let mut bytes = 0usize;
    let mut next_seq = after_seq;

    for chunk in state.chunks.iter().filter(|chunk| chunk.seq >= after_seq) {
        let chunk_bytes = chunk.chunk.as_slice();
        if let Some(max_bytes) = max_bytes {
            if bytes >= max_bytes {
                break;
            }
            let remaining = max_bytes - bytes;
            if chunk_bytes.len() > remaining {
                chunks.push(ProcessOutputChunk {
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

    if chunks.is_empty() && next_seq < state.next_seq {
        next_seq = state.next_seq;
    }

    ReadProcessResponse {
        chunks,
        next_seq,
        exited: state.exited,
        exit_code: state.exit_code,
        closed: state.exited,
        failure: state.failure.clone(),
        orphaned_descendants: state.orphaned_descendants,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use host_protocol::shared::SecretString;

    #[tokio::test(flavor = "current_thread")]
    async fn process_reports_stdout_stderr_and_exit_code() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = ProcessManager::new(root.clone(), root);
        let process_id = ProcessId::new("proc-1");
        manager
            .start_process(StartProcessParams {
                process_id: process_id.clone(),
                argv: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "printf out; printf err >&2".to_owned(),
                ],
                cwd: None,
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                stdin: None,
                timeout_ms: Some(5_000),
                tty: false,
                pipe_stdin: false,
            })
            .await
            .expect("start");

        let output = manager
            .read_process(ReadProcessParams {
                process_id,
                after_seq: None,
                max_bytes: None,
                wait_ms: None,
            })
            .await
            .expect("read");

        assert!(output.exited);
        assert_eq!(output.exit_code, Some(0));
        let stdout = output
            .chunks
            .iter()
            .filter(|chunk| chunk.stream == ProcessOutputStream::Stdout)
            .flat_map(|chunk| chunk.chunk.as_slice().to_vec())
            .collect::<Vec<_>>();
        let stderr = output
            .chunks
            .iter()
            .filter(|chunk| chunk.stream == ProcessOutputStream::Stderr)
            .flat_map(|chunk| chunk.chunk.as_slice().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(stdout, b"out");
        assert_eq!(stderr, b"err");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_process_records_are_pruned() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = ProcessManager::new(root.clone(), root);
        let process_id = ProcessId::new("proc-reused");
        let params = StartProcessParams {
            process_id: process_id.clone(),
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "true".to_owned()],
            cwd: None,
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            stdin: None,
            timeout_ms: Some(5_000),
            tty: false,
            pipe_stdin: false,
        };

        manager
            .start_process(params.clone())
            .await
            .expect("start first");
        let output = manager
            .read_process(ReadProcessParams {
                process_id: process_id.clone(),
                after_seq: None,
                max_bytes: None,
                wait_ms: None,
            })
            .await
            .expect("read terminal output");
        assert!(output.exited);

        tokio::time::sleep(TERMINAL_PROCESS_RETENTION * 2).await;

        manager
            .start_process(params)
            .await
            .expect("start reused id");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn process_accepts_stdin_and_close() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = ProcessManager::new(root.clone(), root);
        let process_id = ProcessId::new("proc-stdin");
        manager
            .start_process(StartProcessParams {
                process_id: process_id.clone(),
                argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "cat".to_owned()],
                cwd: None,
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                stdin: None,
                timeout_ms: Some(5_000),
                tty: false,
                pipe_stdin: true,
            })
            .await
            .expect("start");
        manager
            .write_process(WriteProcessParams {
                process_id: process_id.clone(),
                chunk: Some(ByteChunk::from(b"hello".as_slice())),
                close_stdin: true,
            })
            .await
            .expect("write");
        let output = manager
            .read_process(ReadProcessParams {
                process_id,
                after_seq: None,
                max_bytes: None,
                wait_ms: None,
            })
            .await
            .expect("read");
        let stdout = output
            .chunks
            .iter()
            .filter(|chunk| chunk.stream == ProcessOutputStream::Stdout)
            .flat_map(|chunk| chunk.chunk.as_slice().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(stdout, b"hello");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pty_process_accepts_input_output_and_resize() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = ProcessManager::new(root.clone(), root);
        let process_id = ProcessId::new("proc-pty");
        manager
            .start_process(StartProcessParams {
                process_id: process_id.clone(),
                argv: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "read line; printf '%s' \"$line\"".to_owned(),
                ],
                cwd: None,
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                stdin: None,
                timeout_ms: Some(5_000),
                tty: true,
                pipe_stdin: true,
            })
            .await
            .expect("start PTY");
        manager
            .resize_process(ResizeProcessParams {
                process_id: process_id.clone(),
                size: host_protocol::data::process::TerminalSize {
                    rows: 40,
                    cols: 120,
                },
            })
            .await
            .expect("resize PTY");
        manager
            .write_process(WriteProcessParams {
                process_id: process_id.clone(),
                chunk: Some(ByteChunk::from(b"hello\n".as_slice())),
                close_stdin: false,
            })
            .await
            .expect("write PTY");
        let output = manager
            .read_process(ReadProcessParams {
                process_id,
                after_seq: None,
                max_bytes: None,
                wait_ms: None,
            })
            .await
            .expect("read PTY");
        assert!(output.exited);
        assert!(
            output
                .chunks
                .iter()
                .all(|chunk| chunk.stream == ProcessOutputStream::Pty)
        );
        let bytes = output
            .chunks
            .iter()
            .flat_map(|chunk| chunk.chunk.as_slice().to_vec())
            .collect::<Vec<_>>();
        assert!(String::from_utf8_lossy(&bytes).contains("hello"));
    }

    #[cfg(unix)]
    async fn wait_for_pid_file(path: &std::path::Path) -> i32 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(content) = std::fs::read_to_string(path)
                && let Ok(pid) = content.trim().parse()
            {
                return pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pid file was not written"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    async fn wait_for_process_gone(pid: i32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if unsafe { libc::kill(pid, 0) } != 0 {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "orphaned descendant {pid} was not terminated"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn process_terminate_kills_descendants_in_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = ProcessManager::new(root.clone(), root.clone());
        let process_id = ProcessId::new("proc-group");
        manager
            .start_process(StartProcessParams {
                process_id: process_id.clone(),
                argv: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "sleep 30 & echo $! > orphan.pid; sleep 30".to_owned(),
                ],
                cwd: None,
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                stdin: None,
                timeout_ms: Some(30_000),
                tty: false,
                pipe_stdin: false,
            })
            .await
            .expect("start");

        let pid = wait_for_pid_file(&root.join("orphan.pid")).await;
        let terminated = manager
            .terminate_process(TerminateProcessParams {
                process_id: process_id.clone(),
            })
            .await
            .expect("terminate");
        assert!(terminated.running);
        wait_for_process_gone(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn process_exit_sweeps_orphaned_descendants() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = ProcessManager::new(root.clone(), root.clone());
        let process_id = ProcessId::new("proc-orphan");
        manager
            .start_process(StartProcessParams {
                process_id: process_id.clone(),
                argv: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "sleep 30 & echo $! > orphan.pid; exit 0".to_owned(),
                ],
                cwd: None,
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                stdin: None,
                timeout_ms: Some(30_000),
                tty: false,
                pipe_stdin: false,
            })
            .await
            .expect("start");

        let output = manager
            .read_process(ReadProcessParams {
                process_id,
                after_seq: None,
                max_bytes: None,
                wait_ms: None,
            })
            .await
            .expect("read");
        assert!(output.exited);
        assert_eq!(output.exit_code, Some(0));
        assert!(output.orphaned_descendants);

        wait_for_process_gone(wait_for_pid_file(&root.join("orphan.pid")).await).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn process_injects_secret_env_and_redacts_output() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = ProcessManager::new(root.clone(), root);
        let process_id = ProcessId::new("proc-secret");
        manager
            .start_process(StartProcessParams {
                process_id: process_id.clone(),
                argv: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "printf \"$SECRET_TOKEN\"".to_owned(),
                ],
                cwd: None,
                env: BTreeMap::new(),
                secret_env: BTreeMap::from([(
                    "SECRET_TOKEN".to_owned(),
                    SecretString::new("super-secret-token"),
                )]),
                stdin: None,
                timeout_ms: Some(5_000),
                tty: false,
                pipe_stdin: false,
            })
            .await
            .expect("start");

        let output = manager
            .read_process(ReadProcessParams {
                process_id,
                after_seq: None,
                max_bytes: None,
                wait_ms: None,
            })
            .await
            .expect("read");
        let stdout = output
            .chunks
            .iter()
            .filter(|chunk| chunk.stream == ProcessOutputStream::Stdout)
            .flat_map(|chunk| chunk.chunk.as_slice().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(stdout, b"<redacted>");
    }
}

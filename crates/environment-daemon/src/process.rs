//! Interactive process execution.
//!
//! Every command starts as the leader of its own process group. Its natural
//! exit never sweeps the group: whatever the command left running keeps the
//! environment's lifetime, is sampled once at exit and reported to the
//! caller, and keeps somewhere to write because the output readers stay on
//! the pipes until end of file. Only a timeout, an explicit kill, or the
//! daemon's own cancellation paths kill the group.
//!
//! Output is delivered through a daemon-owned cursor so a handle used from a
//! later connection continues where the previous read stopped, and unread
//! output is retained as a bounded head-and-tail buffer.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{Read as _, Write as _},
    path::{Component, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use environment_protocol::{
    data::process::{
        LeftoverProcess, ProcessOutputChunk, ProcessOutputStream, ProcessSignal,
        ProcessTermination, ReadProcessParams, ReadProcessResponse, ResizeProcessParams,
        ResizeProcessResponse, StartProcessParams, StartProcessResponse, TerminateProcessParams,
        TerminateProcessResponse, WriteProcessParams, WriteProcessResponse, WriteProcessStatus,
    },
    error::{EnvironmentProtocolError, EnvironmentProtocolErrorCode},
    shared::{ByteChunk, EnvironmentPath, ProcessId},
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{Mutex, Notify},
    task::JoinHandle,
    time::Instant,
};

use crate::process_group;

/// How long a finished process entry stays readable after the first read
/// that observed its exit.
#[cfg(not(test))]
pub(crate) const TERMINAL_PROCESS_RETENTION: Duration = Duration::from_secs(60);
#[cfg(test)]
pub(crate) const TERMINAL_PROCESS_RETENTION: Duration = Duration::from_millis(10);

/// How often the exit watcher polls a running child. Readers only notify on
/// pipe events, and leftovers holding the pipes can suppress EOF, so exit
/// observation must not depend on them.
#[cfg(not(test))]
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(test)]
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Unread output retained per process. Beyond it the middle is dropped and
/// the next read reports how many bytes went missing; the oldest half and
/// the newest chunks are kept.
#[cfg(not(test))]
const RETAINED_OUTPUT_CAP: usize = 1024 * 1024;
#[cfg(test)]
const RETAINED_OUTPUT_CAP: usize = 16 * 1024;

const READ_BUFFER_BYTES: usize = 8192;

#[derive(Clone)]
pub struct ProcessManager {
    cwd: PathBuf,
    fs_root: PathBuf,
    processes: Arc<Mutex<BTreeMap<String, Arc<ProcessEntry>>>>,
    /// Daemon configuration variables no child may inherit.
    scrubbed_env: Arc<Vec<String>>,
    /// Process groups whose root exited while members were still alive.
    /// Pruned lazily once the group is empty.
    leftover_groups: Arc<StdMutex<BTreeSet<u32>>>,
}

struct ProcessEntry {
    state: Mutex<ProcessState>,
    notify: Notify,
    readers: Mutex<Vec<JoinHandle<()>>>,
}

struct ProcessState {
    child: ProcessChild,
    pid: Option<u32>,
    pgid: Option<u32>,
    stdin: Option<ProcessInput>,
    pty_master: Option<Box<dyn MasterPty + Send>>,
    redactions: Vec<Vec<u8>>,
    output: OutputBuffer,
    /// Output readers still attached to a pipe or PTY.
    readers_open: u32,
    exited: bool,
    exit_observed_at: Option<Instant>,
    exit_code: Option<i32>,
    termination: Option<ProcessTermination>,
    leftover_processes: Vec<LeftoverProcess>,
    failure: Option<String>,
    cleanup_scheduled: bool,
    /// The root exited and the drain grace passed while leftovers still held
    /// the pipes: readers keep reading so nothing blocks the leftovers, but
    /// the bytes are dropped.
    discarding: bool,
}

enum ProcessChild {
    Pipe(Child),
    Pty(Box<dyn portable_pty::Child + Send + Sync>),
}

enum ProcessInput {
    Pty(Box<dyn std::io::Write + Send>),
}

/// Retained output between the cursor and the newest chunk.
#[derive(Default)]
struct OutputBuffer {
    chunks: VecDeque<RetainedChunk>,
    retained_bytes: usize,
    next_seq: u64,
    delivered_seq: u64,
}

struct RetainedChunk {
    chunk: ProcessOutputChunk,
    /// Bytes dropped from the buffer immediately before this chunk.
    omitted_before: u64,
}

impl OutputBuffer {
    fn push(&mut self, stream: ProcessOutputStream, bytes: Vec<u8>) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.retained_bytes += bytes.len();
        self.chunks.push_back(RetainedChunk {
            chunk: ProcessOutputChunk {
                seq,
                stream,
                chunk: ByteChunk::from(bytes),
            },
            omitted_before: 0,
        });
        self.enforce_cap();
    }

    fn unread_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Drops chunks from just past the head until the buffer fits. The head
    /// is the oldest half of the cap; the newest chunk is never dropped.
    fn enforce_cap(&mut self) {
        if self.retained_bytes <= RETAINED_OUTPUT_CAP {
            return;
        }
        let head_cap = RETAINED_OUTPUT_CAP / 2;
        let mut head_bytes = 0usize;
        let mut head_len = 0usize;
        for retained in &self.chunks {
            // The head never grows past an omission: the chunk that follows
            // dropped bytes stays the boundary, so every later drop extends
            // the same gap instead of opening a second one. Pipe reads on
            // Linux deliver uneven chunks, where a small survivor would
            // otherwise be absorbed into the head.
            if retained.omitted_before > 0 {
                break;
            }
            let len = retained.chunk.chunk.as_slice().len();
            if head_bytes + len > head_cap {
                break;
            }
            head_bytes += len;
            head_len += 1;
        }
        let mut omitted = 0u64;
        while self.retained_bytes > RETAINED_OUTPUT_CAP && self.chunks.len() > head_len + 1 {
            let Some(dropped) = self.chunks.remove(head_len) else {
                break;
            };
            let len = dropped.chunk.chunk.as_slice().len();
            self.retained_bytes -= len;
            omitted += len as u64 + dropped.omitted_before;
        }
        if omitted > 0
            && let Some(next) = self.chunks.get_mut(head_len)
        {
            next.omitted_before += omitted;
        }
    }

    /// Delivers from the cursor, advancing it. Whole chunks are taken up to
    /// `max_bytes`; when the very first chunk alone exceeds the budget it is
    /// split and its remainder stays at the cursor under the same sequence.
    fn deliver(&mut self, max_bytes: Option<usize>) -> (Vec<ProcessOutputChunk>, u64) {
        let mut delivered = Vec::new();
        let mut omitted = 0u64;
        let mut bytes = 0usize;
        while let Some(front) = self.chunks.front_mut() {
            let len = front.chunk.chunk.as_slice().len();
            if let Some(max_bytes) = max_bytes
                && bytes + len > max_bytes
            {
                if delivered.is_empty() && max_bytes > 0 {
                    let (head, rest) = front.chunk.chunk.as_slice().split_at(max_bytes);
                    delivered.push(ProcessOutputChunk {
                        seq: front.chunk.seq,
                        stream: front.chunk.stream,
                        chunk: ByteChunk::from(head.to_vec()),
                    });
                    omitted += front.omitted_before;
                    front.omitted_before = 0;
                    front.chunk.chunk = ByteChunk::from(rest.to_vec());
                    self.retained_bytes -= max_bytes;
                }
                break;
            }
            let retained = self.chunks.pop_front().expect("front chunk exists");
            self.retained_bytes -= len;
            bytes += len;
            omitted += retained.omitted_before;
            delivered.push(retained.chunk);
        }
        self.delivered_seq = self
            .chunks
            .front()
            .map_or(self.next_seq, |front| front.chunk.seq);
        (delivered, omitted)
    }

    /// Re-reads retained chunks from a sequence number without moving the
    /// cursor. Best effort: delivered chunks are gone.
    fn reread(&self, after_seq: u64, max_bytes: Option<usize>) -> (Vec<ProcessOutputChunk>, u64) {
        let mut chunks = Vec::new();
        let mut omitted = 0u64;
        let mut bytes = 0usize;
        for retained in self
            .chunks
            .iter()
            .filter(|retained| retained.chunk.seq >= after_seq)
        {
            let slice = retained.chunk.chunk.as_slice();
            if let Some(max_bytes) = max_bytes {
                if bytes >= max_bytes {
                    break;
                }
                let remaining = max_bytes - bytes;
                if slice.len() > remaining {
                    chunks.push(ProcessOutputChunk {
                        seq: retained.chunk.seq,
                        stream: retained.chunk.stream,
                        chunk: ByteChunk::from(slice[..remaining].to_vec()),
                    });
                    omitted += retained.omitted_before;
                    break;
                }
            }
            bytes += slice.len();
            omitted += retained.omitted_before;
            chunks.push(retained.chunk.clone());
        }
        (chunks, omitted)
    }
}

impl ProcessManager {
    pub fn new(cwd: PathBuf, fs_root: PathBuf) -> Self {
        Self {
            cwd: normalize_path(cwd),
            fs_root: normalize_path(fs_root),
            processes: Arc::new(Mutex::new(BTreeMap::new())),
            scrubbed_env: Arc::new(Vec::new()),
            leftover_groups: Arc::new(StdMutex::new(BTreeSet::new())),
        }
    }

    pub fn with_scrubbed_env(mut self, names: Vec<String>) -> Self {
        self.scrubbed_env = Arc::new(names);
        self
    }

    /// Processes started here that have not exited yet.
    pub async fn running_count(&self) -> u32 {
        let entries: Vec<Arc<ProcessEntry>> =
            self.processes.lock().await.values().cloned().collect();
        let mut running = 0u32;
        for entry in entries {
            if !entry.state.lock().await.exited {
                running += 1;
            }
        }
        running
    }

    /// Process groups of finished commands that still have a live member.
    /// Leftovers are not running work: a service waiting for requests is
    /// idle, so this never feeds the quiescence check.
    pub fn leftover_group_count(&self) -> u32 {
        let mut groups = self
            .leftover_groups
            .lock()
            .expect("leftover groups poisoned");
        groups.retain(|pgid| process_group::group_alive(*pgid));
        u32::try_from(groups.len()).unwrap_or(u32::MAX)
    }

    pub async fn start_process(
        &self,
        params: StartProcessParams,
    ) -> Result<StartProcessResponse, EnvironmentProtocolError> {
        if params.argv.is_empty() {
            return Err(EnvironmentProtocolError::new(
                EnvironmentProtocolErrorCode::InvalidRequest,
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
                return Err(EnvironmentProtocolError::new(
                    EnvironmentProtocolErrorCode::InvalidRequest,
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
        for name in self.scrubbed_env.iter() {
            command.env_remove(name);
        }
        for (name, value) in &params.secret_env {
            command.env(name, value.expose());
        }
        // Plain pipes never keep an input open: a one-shot payload is
        // written and closed at start, otherwise the child reads EOF at
        // once. Interactive input needs a PTY.
        if params.stdin.is_some() {
            command.stdin(Stdio::piped());
        } else {
            command.stdin(Stdio::null());
        }
        process_group::spawn_in_own_group(&mut command);

        let mut child = command.spawn().map_err(|error| {
            EnvironmentProtocolError::new(
                EnvironmentProtocolErrorCode::ProcessFailed,
                format!("spawn process {:?}: {error}", params.argv),
            )
        })?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();
        if let Some(input) = params.stdin {
            let Some(mut writer) = stdin else {
                return Err(EnvironmentProtocolError::new(
                    EnvironmentProtocolErrorCode::ProcessFailed,
                    "process stdin was not available",
                ));
            };
            writer.write_all(input.as_slice()).await.map_err(|error| {
                EnvironmentProtocolError::new(
                    EnvironmentProtocolErrorCode::ProcessFailed,
                    error.to_string(),
                )
            })?;
            drop(writer);
        }

        let process_id = params.process_id;
        let redactions = redactions_for_secret_env(&params.secret_env);
        let readers_open = u32::from(stdout.is_some()) + u32::from(stderr.is_some());
        let entry = Arc::new(ProcessEntry {
            state: Mutex::new(ProcessState::new(
                ProcessChild::Pipe(child),
                pid,
                pid,
                None,
                None,
                redactions,
                readers_open,
            )),
            notify: Notify::new(),
            readers: Mutex::new(Vec::new()),
        });
        self.insert_entry(&process_id, entry.clone()).await?;

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
        self.spawn_watchers(entry, params.timeout_ms);
        Ok(StartProcessResponse { process_id })
    }

    async fn start_pty_process(
        &self,
        params: StartProcessParams,
        cwd: PathBuf,
    ) -> Result<StartProcessResponse, EnvironmentProtocolError> {
        let pty = native_pty_system()
            .openpty(PtySize::default())
            .map_err(process_error("open PTY"))?;
        let mut command = CommandBuilder::new(&params.argv[0]);
        command.args(&params.argv[1..]);
        command.cwd(&cwd);
        for name in self.scrubbed_env.iter() {
            command.env_remove(name);
        }
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
        // The PTY child is a session leader, so its pid is its group id.
        let pid = child.process_id();
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
        let process_id = params.process_id;
        let redactions = redactions_for_secret_env(&params.secret_env);
        let entry = Arc::new(ProcessEntry {
            state: Mutex::new(ProcessState::new(
                ProcessChild::Pty(child),
                pid,
                pid,
                Some(ProcessInput::Pty(stdin)),
                Some(pty.master),
                redactions,
                1,
            )),
            notify: Notify::new(),
            readers: Mutex::new(Vec::new()),
        });
        self.insert_entry(&process_id, entry.clone()).await?;
        entry
            .readers
            .lock()
            .await
            .push(read_pty_stream(entry.clone(), reader));
        self.spawn_watchers(entry, params.timeout_ms);
        Ok(StartProcessResponse { process_id })
    }

    async fn insert_entry(
        &self,
        process_id: &ProcessId,
        entry: Arc<ProcessEntry>,
    ) -> Result<(), EnvironmentProtocolError> {
        let mut processes = self.processes.lock().await;
        if processes.contains_key(process_id.as_str()) {
            return Err(EnvironmentProtocolError::new(
                EnvironmentProtocolErrorCode::Conflict,
                format!("process id already exists: {process_id}"),
            ));
        }
        processes.insert(process_id.to_string(), entry);
        Ok(())
    }

    fn spawn_watchers(&self, entry: Arc<ProcessEntry>, timeout_ms: Option<u64>) {
        if let Some(timeout_ms) = timeout_ms {
            tokio::spawn(timeout_process(
                entry.clone(),
                Duration::from_millis(timeout_ms),
            ));
        }
        tokio::spawn(watch_exit(entry, self.leftover_groups.clone()));
    }

    pub async fn read_process(
        &self,
        params: ReadProcessParams,
    ) -> Result<ReadProcessResponse, EnvironmentProtocolError> {
        let Some(entry) = self.entry(&params.process_id).await else {
            return Err(EnvironmentProtocolError::new(
                EnvironmentProtocolErrorCode::NotFound,
                format!("unknown process id: {}", params.process_id),
            ));
        };
        let deadline = params
            .wait_ms
            .map(|wait_ms| Instant::now() + Duration::from_millis(wait_ms));

        loop {
            let (response, schedule_cleanup, wake_at) = {
                let mut state = entry.state.lock().await;
                if let Some(pgid) = update_exit_status(&mut state)? {
                    self.observe_exit(&entry, pgid);
                }
                let now = Instant::now();
                let (done, drain_deadline) = exit_settled(&state, now);
                let should_return = if params.wait_ms.is_some() {
                    done || deadline.is_some_and(|deadline| now >= deadline)
                        || params
                            .max_bytes
                            .is_some_and(|max_bytes| state.output.unread_bytes() >= max_bytes)
                } else {
                    done
                };
                if should_return {
                    let schedule_cleanup = state.exited && !state.cleanup_scheduled;
                    if schedule_cleanup {
                        state.cleanup_scheduled = true;
                    }
                    (
                        Some(response_from_state(
                            &mut state,
                            params.after_seq,
                            params.max_bytes,
                        )),
                        schedule_cleanup,
                        None,
                    )
                } else {
                    let wake_at = match (deadline, drain_deadline) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    };
                    (None, false, wake_at)
                }
            };
            if let Some(response) = response {
                if schedule_cleanup {
                    self.schedule_terminal_cleanup(params.process_id.clone(), entry.clone());
                }
                return Ok(response);
            }

            if let Some(wake_at) = wake_at {
                tokio::select! {
                    _ = entry.notify.notified() => {}
                    _ = tokio::time::sleep_until(wake_at) => {}
                }
            } else {
                entry.notify.notified().await;
            }
        }
    }

    pub async fn write_process(
        &self,
        params: WriteProcessParams,
    ) -> Result<WriteProcessResponse, EnvironmentProtocolError> {
        let Some(entry) = self.entry(&params.process_id).await else {
            return Ok(WriteProcessResponse {
                status: WriteProcessStatus::UnknownProcess,
            });
        };
        let mut state = entry.state.lock().await;
        if let Some(pgid) = update_exit_status(&mut state)? {
            self.observe_exit(&entry, pgid);
        }
        let chunk = params.chunk.filter(|chunk| !chunk.as_slice().is_empty());
        // No input and nothing to close is a wait; the caller's read follows.
        if chunk.is_none() && !params.close_stdin {
            return Ok(WriteProcessResponse {
                status: WriteProcessStatus::Accepted,
            });
        }
        if state.exited {
            return Ok(WriteProcessResponse {
                status: if chunk.is_some() {
                    WriteProcessStatus::StdinClosed
                } else {
                    WriteProcessStatus::Accepted
                },
            });
        }
        let Some(stdin) = state.stdin.as_mut() else {
            return Ok(WriteProcessResponse {
                status: if chunk.is_some() {
                    WriteProcessStatus::StdinClosed
                } else {
                    WriteProcessStatus::Accepted
                },
            });
        };
        if let Some(chunk) = chunk {
            match stdin {
                ProcessInput::Pty(stdin) => stdin
                    .write_all(chunk.as_slice())
                    .and_then(|()| stdin.flush()),
            }
            .map_err(|error| {
                EnvironmentProtocolError::new(
                    EnvironmentProtocolErrorCode::ProcessFailed,
                    error.to_string(),
                )
            })?;
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
    ) -> Result<TerminateProcessResponse, EnvironmentProtocolError> {
        let Some(entry) = self.entry(&params.process_id).await else {
            return Ok(TerminateProcessResponse { running: false });
        };
        let mut state = entry.state.lock().await;
        if let Some(pgid) = update_exit_status(&mut state)? {
            self.observe_exit(&entry, pgid);
        }
        if state.exited {
            // The root is gone; the signal still reaches whatever it left in
            // its group, which is how a handle stops the service it started.
            if let Some(pgid) = state.pgid {
                match params.signal {
                    ProcessSignal::Interrupt => {
                        process_group::interrupt_group(pgid);
                    }
                    ProcessSignal::Kill => {
                        process_group::kill_group(pgid);
                    }
                }
            }
            return Ok(TerminateProcessResponse { running: false });
        }
        match params.signal {
            ProcessSignal::Interrupt => {
                if let Some(pgid) = state.pgid {
                    process_group::interrupt_group(pgid);
                } else {
                    interrupt_child(&mut state.child);
                }
            }
            ProcessSignal::Kill => {
                if let Some(pgid) = state.pgid {
                    process_group::kill_group(pgid);
                }
                kill_child(&mut state.child).await?;
                state.exited = true;
                state.exit_observed_at = Some(Instant::now());
                state.exit_code = None;
                state.termination = Some(ProcessTermination::Killed);
                state.stdin.take();
                spawn_reader_drain(entry.clone());
                entry.notify.notify_waiters();
            }
        }
        Ok(TerminateProcessResponse { running: true })
    }

    pub async fn resize_process(
        &self,
        params: ResizeProcessParams,
    ) -> Result<ResizeProcessResponse, EnvironmentProtocolError> {
        let Some(entry) = self.entry(&params.process_id).await else {
            return Err(EnvironmentProtocolError::new(
                EnvironmentProtocolErrorCode::NotFound,
                format!("unknown process id: {}", params.process_id),
            ));
        };
        let state = entry.state.lock().await;
        let Some(master) = state.pty_master.as_ref() else {
            return Err(EnvironmentProtocolError::new(
                EnvironmentProtocolErrorCode::InvalidRequest,
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

    /// Records that a newly exited root left its group alive, for the idle
    /// report, and starts the drain. The readers are left on the pipes; the
    /// drain task switches them to discard mode once the grace passes.
    fn observe_exit(&self, entry: &Arc<ProcessEntry>, pgid: u32) {
        record_leftover_group(pgid, &self.leftover_groups);
        spawn_group_drain(entry.clone());
    }

    fn resolve_cwd(&self, path: &EnvironmentPath) -> Result<PathBuf, EnvironmentProtocolError> {
        let candidate = if path.is_absolute() {
            PathBuf::from(path.as_str())
        } else if path.as_str() == "." {
            self.cwd.clone()
        } else {
            self.cwd.join(path.as_str())
        };
        let normalized = normalize_path(candidate);
        if !normalized.starts_with(&self.fs_root) {
            return Err(EnvironmentProtocolError::new(
                EnvironmentProtocolErrorCode::Forbidden,
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

    /// Drops the entry after the retention period. Readers are never
    /// aborted here: those a sweep abandoned are gone already, and those
    /// draining a pipe a leftover still holds must stay on it until end of
    /// file so the leftover never sees `EPIPE`.
    fn schedule_terminal_cleanup(&self, process_id: ProcessId, entry: Arc<ProcessEntry>) {
        let processes = self.processes.clone();
        tokio::spawn(async move {
            tokio::time::sleep(TERMINAL_PROCESS_RETENTION).await;
            let mut processes = processes.lock().await;
            if processes
                .get(process_id.as_str())
                .is_some_and(|current| Arc::ptr_eq(current, &entry))
            {
                processes.remove(process_id.as_str());
            }
        });
    }
}

impl ProcessState {
    fn new(
        child: ProcessChild,
        pid: Option<u32>,
        pgid: Option<u32>,
        stdin: Option<ProcessInput>,
        pty_master: Option<Box<dyn MasterPty + Send>>,
        redactions: Vec<Vec<u8>>,
        readers_open: u32,
    ) -> Self {
        Self {
            child,
            pid,
            pgid,
            stdin,
            pty_master,
            redactions,
            output: OutputBuffer::default(),
            readers_open,
            exited: false,
            exit_observed_at: None,
            exit_code: None,
            termination: None,
            leftover_processes: Vec::new(),
            failure: None,
            cleanup_scheduled: false,
            discarding: false,
        }
    }

    fn push_output(&mut self, stream: ProcessOutputStream, bytes: &[u8]) {
        if self.discarding {
            return;
        }
        let bytes = redact_bytes(bytes, &self.redactions);
        self.output.push(stream, bytes);
    }

    fn reader_closed(&mut self) {
        self.readers_open = self.readers_open.saturating_sub(1);
    }
}

/// Whether a read may return for an exited root: the readers reached end of
/// file, or the drain grace since the exit passed and leftovers still hold
/// the pipes. Also returns when to wake up for the grace when it has not.
fn exit_settled(state: &ProcessState, now: Instant) -> (bool, Option<Instant>) {
    if !state.exited {
        return (false, None);
    }
    if state.readers_open == 0 || state.discarding {
        return (true, None);
    }
    let Some(exit_observed_at) = state.exit_observed_at else {
        return (true, None);
    };
    let drain_deadline = exit_observed_at + process_group::OUTPUT_DRAIN_GRACE;
    (now >= drain_deadline, Some(drain_deadline))
}

async fn read_stream<R>(entry: Arc<ProcessEntry>, mut reader: R, stream: ProcessOutputStream)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0; READ_BUFFER_BYTES];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let mut state = entry.state.lock().await;
                state.push_output(stream, &buffer[..read]);
                entry.notify.notify_waiters();
            }
            Err(error) => {
                let mut state = entry.state.lock().await;
                if state.failure.is_none() && !state.exited {
                    state.failure = Some(error.to_string());
                }
                break;
            }
        }
    }
    entry.state.lock().await.reader_closed();
    entry.notify.notify_waiters();
}

fn read_pty_stream(
    entry: Arc<ProcessEntry>,
    mut reader: Box<dyn std::io::Read + Send>,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let handle = tokio::runtime::Handle::current();
        let mut buffer = vec![0; READ_BUFFER_BYTES];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let entry = entry.clone();
                    let bytes = buffer[..read].to_vec();
                    handle.block_on(async move {
                        let mut state = entry.state.lock().await;
                        state.push_output(ProcessOutputStream::Pty, &bytes);
                        entry.notify.notify_waiters();
                    });
                }
                Err(error) => {
                    let entry = entry.clone();
                    handle.block_on(async move {
                        let mut state = entry.state.lock().await;
                        if state.failure.is_none() && !state.exited {
                            state.failure = Some(error.to_string());
                        }
                    });
                    break;
                }
            }
        }
        handle.block_on(async move {
            entry.state.lock().await.reader_closed();
            entry.notify.notify_waiters();
        });
    })
}

/// Observes the child's exit independently of pipe events so a silent child
/// whose leftovers hold the pipes open still completes promptly.
async fn watch_exit(entry: Arc<ProcessEntry>, leftover_groups: Arc<StdMutex<BTreeSet<u32>>>) {
    loop {
        tokio::time::sleep(EXIT_POLL_INTERVAL).await;
        let done = {
            let mut state = entry.state.lock().await;
            if state.exited {
                true
            } else {
                match update_exit_status(&mut state) {
                    Ok(Some(pgid)) => {
                        record_leftover_group(pgid, &leftover_groups);
                        spawn_group_drain(entry.clone());
                        true
                    }
                    Ok(None) => state.exited,
                    Err(_) => true,
                }
            }
        };
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
        state.exit_observed_at = Some(Instant::now());
        state.exit_code = None;
        state.termination = Some(ProcessTermination::TimedOut);
        state.stdin.take();
    }
    drain_or_abort_readers(&entry).await;
    entry.notify.notify_waiters();
}

/// Polls the child for exit. When the exit is newly observed while members
/// of its process group are still alive, returns the group id so the caller
/// can record the leftovers.
fn update_exit_status(state: &mut ProcessState) -> Result<Option<u32>, EnvironmentProtocolError> {
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
            state.exit_observed_at = Some(Instant::now());
            state.exit_code = status;
            state.stdin.take();
            Ok(state.pgid.filter(|pgid| process_group::group_alive(*pgid)))
        }
        Ok(None) => Ok(None),
        Err(error) => {
            state.failure = Some(error.to_string());
            Err(EnvironmentProtocolError::new(
                EnvironmentProtocolErrorCode::ProcessFailed,
                format!("poll process exit: {error}"),
            ))
        }
    }
}

fn record_leftover_group(pgid: u32, leftover_groups: &StdMutex<BTreeSet<u32>>) {
    leftover_groups
        .lock()
        .expect("leftover groups poisoned")
        .insert(pgid);
}

async fn kill_child(child: &mut ProcessChild) -> Result<(), EnvironmentProtocolError> {
    let result = match child {
        ProcessChild::Pipe(child) => child.kill().await,
        ProcessChild::Pty(child) => child.kill(),
    };
    result.map_err(|error| {
        EnvironmentProtocolError::new(
            EnvironmentProtocolErrorCode::ProcessFailed,
            error.to_string(),
        )
    })
}

fn interrupt_child(child: &mut ProcessChild) {
    let pid = match child {
        ProcessChild::Pipe(child) => child.id(),
        ProcessChild::Pty(child) => child.process_id(),
    };
    #[cfg(unix)]
    if let Some(pid) = pid
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::INT);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

fn process_error<E: std::fmt::Display>(
    context: &'static str,
) -> impl FnOnce(E) -> EnvironmentProtocolError {
    move |error| {
        EnvironmentProtocolError::new(
            EnvironmentProtocolErrorCode::ProcessFailed,
            format!("{context}: {error}"),
        )
    }
}

/// After a natural exit, gives the readers the drain grace to reach end of
/// file so bytes the root wrote just before exiting are kept, then leaves
/// them on the pipes in discard mode. The pipe's own end of file, when the
/// last leftover holding it exits, ends them. The group is never signalled.
fn spawn_group_drain(entry: Arc<ProcessEntry>) {
    tokio::spawn(async move {
        tokio::time::sleep(process_group::OUTPUT_DRAIN_GRACE).await;
        let mut state = entry.state.lock().await;
        if state.readers_open > 0 {
            state.discarding = true;
        }
        drop(state);
        entry.notify.notify_waiters();
    });
}

fn spawn_reader_drain(entry: Arc<ProcessEntry>) {
    tokio::spawn(async move {
        drain_or_abort_readers(&entry).await;
        entry.notify.notify_waiters();
    });
}

/// After the group was killed: waits the drain grace for the readers, then
/// abandons any still stuck on a pipe an escaped descendant holds.
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
    let mut state = entry.state.lock().await;
    state.readers_open = 0;
    state.discarding = true;
}

fn response_from_state(
    state: &mut ProcessState,
    after_seq: Option<u64>,
    max_bytes: Option<usize>,
) -> ReadProcessResponse {
    let (chunks, omitted_bytes, next_seq) = match after_seq {
        None => {
            let (chunks, omitted) = state.output.deliver(max_bytes);
            (chunks, omitted, state.output.delivered_seq)
        }
        Some(after_seq) => {
            let (chunks, omitted) = state.output.reread(after_seq, max_bytes);
            let next_seq = chunks
                .last()
                .map_or(after_seq.max(state.output.delivered_seq), |chunk| {
                    chunk.seq + 1
                });
            (chunks, omitted, next_seq)
        }
    };
    // Leftovers are sampled when the caller looks, so the list reflects
    // what is alive now rather than the instant the root exited, when a
    // background child may still have been mid-exec.
    if state.exited {
        state.leftover_processes = match state.pgid.filter(|pgid| process_group::group_alive(*pgid))
        {
            Some(pgid) => sample_group_members(pgid),
            None => Vec::new(),
        };
    }
    ReadProcessResponse {
        chunks,
        next_seq,
        exited: state.exited,
        exit_code: state.exit_code,
        closed: state.exited,
        failure: state.failure.clone(),
        termination: state.termination,
        pid: state.pid,
        omitted_bytes,
        leftover_processes: state.leftover_processes.clone(),
    }
}

fn redactions_for_secret_env(
    secret_env: &BTreeMap<String, environment_protocol::shared::SecretString>,
) -> Vec<Vec<u8>> {
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

/// Live, non-zombie members of a process group with their command lines,
/// best effort: `/proc` on Linux, libproc on macOS, empty elsewhere.
#[cfg(target_os = "linux")]
fn sample_group_members(pgid: u32) -> Vec<LeftoverProcess> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut members = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // "pid (comm) state ppid pgrp ..."; comm may contain spaces and
        // parentheses, so split at the last closing parenthesis.
        let Some(open) = stat.find('(') else {
            continue;
        };
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let mut fields = stat[close + 1..].split_whitespace();
        let state = fields.next();
        let _ppid = fields.next();
        let pgrp = fields.next().and_then(|value| value.parse::<u32>().ok());
        if pgrp != Some(pgid) || state == Some("Z") {
            continue;
        }
        let command = std::fs::read(entry.path().join("cmdline"))
            .ok()
            .map(|bytes| {
                bytes
                    .split(|byte| *byte == 0)
                    .filter(|part| !part.is_empty())
                    .map(|part| String::from_utf8_lossy(part).into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|command| !command.is_empty())
            .unwrap_or_else(|| stat[open + 1..close].to_owned());
        members.push(LeftoverProcess { pid, command });
    }
    members.sort_by_key(|member| member.pid);
    members
}

#[cfg(target_os = "macos")]
fn sample_group_members(pgid: u32) -> Vec<LeftoverProcess> {
    use std::{ffi::c_void, mem};

    let Ok(group) = libc::pid_t::try_from(pgid) else {
        return Vec::new();
    };
    let pid_size = mem::size_of::<libc::pid_t>();
    // libproc's group listing returns pid counts, not bytes: with a null
    // buffer the count of every process on the system, which is an upper
    // bound for the group, and with a buffer the count it filled.
    let upper_bound = unsafe { libc::proc_listpgrppids(group, std::ptr::null_mut(), 0) };
    if upper_bound <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0 as libc::pid_t; upper_bound as usize + 16];
    let count = unsafe {
        libc::proc_listpgrppids(
            group,
            pids.as_mut_ptr() as *mut c_void,
            (pids.len() * pid_size) as libc::c_int,
        )
    };
    if count <= 0 {
        return Vec::new();
    }
    pids.truncate(count as usize);
    let mut members = Vec::new();
    for pid in pids {
        if pid <= 0 {
            continue;
        }
        let mut info: libc::proc_bsdinfo = unsafe { mem::zeroed() };
        let info_size = mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut libc::proc_bsdinfo as *mut c_void,
                info_size,
            )
        };
        if read == info_size && info.pbi_status == libc::SZOMB {
            continue;
        }
        let mut path = vec![0u8; 4096];
        let len =
            unsafe { libc::proc_pidpath(pid, path.as_mut_ptr() as *mut c_void, path.len() as u32) };
        let command = if len > 0 {
            String::from_utf8_lossy(&path[..len as usize]).into_owned()
        } else {
            String::new()
        };
        members.push(LeftoverProcess {
            pid: pid as u32,
            command,
        });
    }
    members.sort_by_key(|member| member.pid);
    members
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn sample_group_members(_pgid: u32) -> Vec<LeftoverProcess> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment_protocol::shared::SecretString;

    fn manager() -> (tempfile::TempDir, PathBuf, ProcessManager) {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let manager = ProcessManager::new(root.clone(), root.clone());
        (temp, root, manager)
    }

    fn params(process_id: &str, script: &str) -> StartProcessParams {
        StartProcessParams {
            process_id: ProcessId::new(process_id),
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd: None,
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            stdin: None,
            timeout_ms: Some(30_000),
            tty: false,
        }
    }

    fn read_params(process_id: &str, wait_ms: Option<u64>) -> ReadProcessParams {
        ReadProcessParams {
            process_id: ProcessId::new(process_id),
            after_seq: None,
            max_bytes: None,
            wait_ms,
        }
    }

    fn stream_bytes(response: &ReadProcessResponse, stream: ProcessOutputStream) -> Vec<u8> {
        response
            .chunks
            .iter()
            .filter(|chunk| chunk.stream == stream)
            .flat_map(|chunk| chunk.chunk.as_slice().to_vec())
            .collect()
    }

    fn all_bytes(response: &ReadProcessResponse) -> Vec<u8> {
        response
            .chunks
            .iter()
            .flat_map(|chunk| chunk.chunk.as_slice().to_vec())
            .collect()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn process_reports_stdout_stderr_exit_code_and_pid() {
        let (_temp, _root, manager) = manager();
        manager
            .start_process(params("proc-1", "printf out; printf err >&2"))
            .await
            .expect("start");

        let output = manager
            .read_process(read_params("proc-1", None))
            .await
            .expect("read");

        assert!(output.exited);
        assert_eq!(output.exit_code, Some(0));
        assert!(output.pid.is_some());
        assert_eq!(output.termination, None);
        assert!(output.leftover_processes.is_empty());
        assert_eq!(stream_bytes(&output, ProcessOutputStream::Stdout), b"out");
        assert_eq!(stream_bytes(&output, ProcessOutputStream::Stderr), b"err");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_process_records_are_pruned_and_reusable() {
        let (_temp, _root, manager) = manager();
        let params = params("proc-reused", "true");

        manager
            .start_process(params.clone())
            .await
            .expect("start first");
        let output = manager
            .read_process(read_params("proc-reused", None))
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
    async fn plain_pipe_process_reads_eof_on_stdin_at_once() {
        let (_temp, _root, manager) = manager();
        manager
            .start_process(params("proc-cat", "cat; echo done"))
            .await
            .expect("start");
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            manager.read_process(read_params("proc-cat", None)),
        )
        .await
        .expect("cat exits without waiting for input")
        .expect("read");
        assert!(output.exited);
        assert_eq!(
            stream_bytes(&output, ProcessOutputStream::Stdout),
            b"done\n"
        );

        let write = manager
            .write_process(WriteProcessParams {
                process_id: ProcessId::new("proc-cat"),
                chunk: Some(ByteChunk::from(b"late".as_slice())),
                close_stdin: false,
            })
            .await
            .expect("write");
        assert_eq!(write.status, WriteProcessStatus::StdinClosed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_shot_stdin_is_written_and_closed_at_start() {
        let (_temp, _root, manager) = manager();
        let mut params = params("proc-stdin", "cat");
        params.stdin = Some(ByteChunk::from(b"hello".as_slice()));
        manager.start_process(params).await.expect("start");
        let output = manager
            .read_process(read_params("proc-stdin", None))
            .await
            .expect("read");
        assert!(output.exited);
        assert_eq!(stream_bytes(&output, ProcessOutputStream::Stdout), b"hello");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pty_process_accepts_input_echoes_and_resizes() {
        let (_temp, _root, manager) = manager();
        let mut params = params("proc-pty", "read line; printf '%s' \"$line\"");
        params.tty = true;
        manager.start_process(params).await.expect("start PTY");
        manager
            .resize_process(ResizeProcessParams {
                process_id: ProcessId::new("proc-pty"),
                size: environment_protocol::data::process::TerminalSize {
                    rows: 40,
                    cols: 120,
                },
            })
            .await
            .expect("resize PTY");
        let write = manager
            .write_process(WriteProcessParams {
                process_id: ProcessId::new("proc-pty"),
                chunk: Some(ByteChunk::from(b"hello\n".as_slice())),
                close_stdin: false,
            })
            .await
            .expect("write PTY");
        assert_eq!(write.status, WriteProcessStatus::Accepted);
        let output = manager
            .read_process(read_params("proc-pty", None))
            .await
            .expect("read PTY");
        assert!(output.exited);
        assert!(
            output
                .chunks
                .iter()
                .all(|chunk| chunk.stream == ProcessOutputStream::Pty)
        );
        assert!(String::from_utf8_lossy(&all_bytes(&output)).contains("hello"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_collects_for_the_whole_window_or_until_exit() {
        let (_temp, _root, manager) = manager();
        manager
            .start_process(params(
                "proc-window",
                "for i in 1 2 3 4 5 6 7 8 9 10; do echo line$i; sleep 0.02; done",
            ))
            .await
            .expect("start");

        let started = std::time::Instant::now();
        let output = manager
            .read_process(read_params("proc-window", Some(10_000)))
            .await
            .expect("read");
        assert!(output.exited, "a long wait returns at exit");
        assert!(started.elapsed() < Duration::from_secs(5));
        let text = String::from_utf8_lossy(&all_bytes(&output)).into_owned();
        assert_eq!(text.lines().count(), 10, "every line arrives in one read");

        manager
            .start_process(params(
                "proc-short",
                "for i in 1 2 3 4 5 6 7 8 9 10; do echo line$i; sleep 0.05; done",
            ))
            .await
            .expect("start short");
        let started = std::time::Instant::now();
        let output = manager
            .read_process(read_params("proc-short", Some(120)))
            .await
            .expect("read short");
        let elapsed = started.elapsed();
        assert!(!output.exited);
        assert!(
            elapsed >= Duration::from_millis(100) && elapsed < Duration::from_millis(400),
            "returned at the window, not the first chunk: {elapsed:?}"
        );
        let lines = String::from_utf8_lossy(&all_bytes(&output)).lines().count();
        assert!(
            (1..10).contains(&lines),
            "partial output at the window: {lines} lines"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cursor_reads_return_disjoint_output_and_rereads_do_not_move_it() {
        let (_temp, _root, manager) = manager();
        manager
            .start_process(params(
                "proc-cursor",
                "echo first; sleep 0.2; echo second; sleep 0.2; echo third",
            ))
            .await
            .expect("start");

        let first = manager
            .read_process(read_params("proc-cursor", Some(80)))
            .await
            .expect("first read");
        assert_eq!(all_bytes(&first), b"first\n");
        assert!(!first.exited);

        let reread = manager
            .read_process(ReadProcessParams {
                process_id: ProcessId::new("proc-cursor"),
                after_seq: Some(0),
                max_bytes: None,
                wait_ms: Some(0),
            })
            .await
            .expect("reread");
        assert!(
            reread.chunks.is_empty(),
            "delivered chunks are not retained"
        );

        let rest = manager
            .read_process(read_params("proc-cursor", Some(10_000)))
            .await
            .expect("second read");
        assert!(rest.exited);
        assert_eq!(all_bytes(&rest), b"second\nthird\n");
        assert!(
            rest.chunks
                .iter()
                .all(|chunk| chunk.seq > first.chunks[0].seq),
            "the second read never repeats the first"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn max_bytes_splits_the_first_chunk_and_keeps_the_remainder() {
        let (_temp, _root, manager) = manager();
        manager
            .start_process(params("proc-split", "printf abcdefgh"))
            .await
            .expect("start");
        let head = manager
            .read_process(ReadProcessParams {
                process_id: ProcessId::new("proc-split"),
                after_seq: None,
                max_bytes: Some(3),
                wait_ms: None,
            })
            .await
            .expect("head");
        assert_eq!(all_bytes(&head), b"abc");
        let tail = manager
            .read_process(ReadProcessParams {
                process_id: ProcessId::new("proc-split"),
                after_seq: None,
                max_bytes: Some(100),
                wait_ms: None,
            })
            .await
            .expect("tail");
        assert_eq!(all_bytes(&tail), b"defgh");
        assert_eq!(head.chunks[0].seq, tail.chunks[0].seq);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unread_output_is_capped_to_head_and_tail() {
        let (_temp, _root, manager) = manager();
        // 64 KiB of numbered lines against a 16 KiB test cap.
        manager
            .start_process(params(
                "proc-cap",
                "i=0; while [ $i -lt 4096 ]; do printf '%015d\\n' $i; i=$((i+1)); done",
            ))
            .await
            .expect("start");
        let output = manager
            .read_process(read_params("proc-cap", None))
            .await
            .expect("read");
        assert!(output.exited);
        let delivered = all_bytes(&output);
        assert!(
            delivered.len() <= RETAINED_OUTPUT_CAP,
            "{}",
            delivered.len()
        );
        assert!(output.omitted_bytes > 0);
        assert_eq!(
            delivered.len() as u64 + output.omitted_bytes,
            4096 * 16,
            "head + omitted + tail account for every byte"
        );
        let text = String::from_utf8_lossy(&delivered).into_owned();
        assert!(text.starts_with("000000000000000\n"), "head kept");
        assert!(text.ends_with("000000000004095\n"), "tail kept");
        let gaps = output
            .chunks
            .windows(2)
            .filter(|pair| pair[1].seq != pair[0].seq + 1)
            .count();
        assert_eq!(gaps, 1, "one gap where the middle was");

        let again = manager
            .read_process(read_params("proc-cap", Some(0)))
            .await
            .expect("second read");
        assert!(again.chunks.is_empty());
        assert_eq!(again.omitted_bytes, 0, "the omission is reported once");
    }

    #[test]
    fn cap_keeps_one_contiguous_gap_when_chunk_sizes_are_uneven() {
        // 16 KiB test cap, 8 KiB head. Seven 1000-byte chunks fill the head;
        // the 1500-byte chunk after them is the first to be dropped, which
        // leaves a 500-byte survivor right behind the head. A head recomputed
        // purely by bytes would absorb that survivor on the next drop and
        // open a second gap behind it.
        let mut buffer = OutputBuffer::default();
        let mut push = |len: usize| buffer.push(ProcessOutputStream::Stdout, vec![b'x'; len]);
        for _ in 0..7 {
            push(1000);
        }
        push(1500);
        push(500);
        for _ in 0..5 {
            push(1500);
        }
        push(1500);
        let total = 7 * 1000 + 1500 + 500 + 5 * 1500 + 1500;

        let (chunks, omitted) = buffer.deliver(None);

        let delivered: usize = chunks
            .iter()
            .map(|chunk| chunk.chunk.as_slice().len())
            .sum();
        assert!(delivered <= RETAINED_OUTPUT_CAP, "{delivered}");
        assert!(omitted > 0);
        assert_eq!(delivered as u64 + omitted, total as u64);
        let gaps = chunks
            .windows(2)
            .filter(|pair| pair[1].seq != pair[0].seq + 1)
            .count();
        assert_eq!(gaps, 1, "one contiguous gap after the head");
        assert_eq!(chunks[6].seq, 6, "the whole head survives");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_write_is_a_wait_until_the_entry_is_pruned() {
        let (_temp, _root, manager) = manager();
        manager
            .start_process(params("proc-wait", "echo bye; exit 7"))
            .await
            .expect("start");
        for _ in 0..200 {
            if manager.running_count().await == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let write = manager
            .write_process(WriteProcessParams {
                process_id: ProcessId::new("proc-wait"),
                chunk: None,
                close_stdin: false,
            })
            .await
            .expect("empty write");
        assert_eq!(write.status, WriteProcessStatus::Accepted);
        let output = manager
            .read_process(read_params("proc-wait", Some(1_000)))
            .await
            .expect("read");
        assert!(output.exited);
        assert_eq!(output.exit_code, Some(7));
        assert_eq!(all_bytes(&output), b"bye\n");

        tokio::time::sleep(TERMINAL_PROCESS_RETENTION * 3).await;
        let write = manager
            .write_process(WriteProcessParams {
                process_id: ProcessId::new("proc-wait"),
                chunk: None,
                close_stdin: false,
            })
            .await
            .expect("late write");
        assert_eq!(write.status, WriteProcessStatus::UnknownProcess);
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
    fn process_alive(pid: i32) -> bool {
        rustix::process::Pid::from_raw(pid)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
    }

    #[cfg(unix)]
    async fn wait_for_process_gone(pid: i32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if !process_alive(pid) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "descendant {pid} was not terminated"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn kill_terminates_descendants_in_group() {
        let (_temp, root, manager) = manager();
        manager
            .start_process(params(
                "proc-group",
                "sleep 30 & echo $! > orphan.pid; sleep 30",
            ))
            .await
            .expect("start");

        let pid = wait_for_pid_file(&root.join("orphan.pid")).await;
        let terminated = manager
            .terminate_process(TerminateProcessParams {
                process_id: ProcessId::new("proc-group"),
                signal: ProcessSignal::Kill,
            })
            .await
            .expect("terminate");
        assert!(terminated.running);
        wait_for_process_gone(pid).await;
        let output = manager
            .read_process(read_params("proc-group", Some(1_000)))
            .await
            .expect("read");
        assert!(output.exited);
        assert_eq!(output.termination, Some(ProcessTermination::Killed));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn timeout_terminates_descendants_in_group() {
        let (_temp, root, manager) = manager();
        let mut params = params("proc-timeout", "sleep 30 & echo $! > orphan.pid; sleep 30");
        params.timeout_ms = Some(200);
        manager.start_process(params).await.expect("start");

        let pid = wait_for_pid_file(&root.join("orphan.pid")).await;
        let output = manager
            .read_process(read_params("proc-timeout", None))
            .await
            .expect("read");
        assert!(output.exited);
        assert_eq!(output.termination, Some(ProcessTermination::TimedOut));
        wait_for_process_gone(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn interrupt_reaches_the_group_and_the_next_read_observes_the_trap() {
        let (_temp, _root, manager) = manager();
        manager
            .start_process(params(
                "proc-int",
                "trap 'echo caught; exit 3' INT; sleep 100 & wait $!",
            ))
            .await
            .expect("start");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let signalled = manager
            .terminate_process(TerminateProcessParams {
                process_id: ProcessId::new("proc-int"),
                signal: ProcessSignal::Interrupt,
            })
            .await
            .expect("interrupt");
        assert!(signalled.running);
        let output = manager
            .read_process(read_params("proc-int", Some(5_000)))
            .await
            .expect("read");
        assert!(output.exited);
        assert_eq!(output.exit_code, Some(3));
        assert_eq!(output.termination, None);
        assert_eq!(all_bytes(&output), b"caught\n");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn process_exit_keeps_leftover_descendants_and_reports_them() {
        let (_temp, root, manager) = manager();
        manager
            .start_process(params(
                "proc-leftover",
                "nohup sleep 30 >/dev/null 2>&1 & echo $! > orphan.pid; exit 0",
            ))
            .await
            .expect("start");

        let pid = wait_for_pid_file(&root.join("orphan.pid")).await;
        // Let the background child finish exec'ing so the sample names it.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = std::time::Instant::now();
        let output = manager
            .read_process(read_params("proc-leftover", None))
            .await
            .expect("read");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(output.exited);
        assert_eq!(output.exit_code, Some(0));
        assert!(process_alive(pid), "the leftover survives the call");
        assert_eq!(
            output
                .leftover_processes
                .iter()
                .map(|member| member.pid)
                .collect::<Vec<_>>(),
            vec![pid as u32],
            "{:?}",
            output.leftover_processes
        );
        assert!(
            output.leftover_processes[0].command.contains("sleep"),
            "{:?}",
            output.leftover_processes
        );
        assert_eq!(manager.leftover_group_count(), 1);

        tokio::time::sleep(TERMINAL_PROCESS_RETENTION * 3).await;
        assert!(
            process_alive(pid),
            "the leftover survives the entry's pruning"
        );
        let stale = manager
            .read_process(read_params("proc-leftover", Some(0)))
            .await;
        assert!(stale.is_err(), "the entry was pruned");

        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid).expect("valid child pid"),
            rustix::process::Signal::KILL,
        );
        wait_for_process_gone(pid).await;
        assert_eq!(manager.leftover_group_count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn leftover_writing_to_the_pipe_after_exit_is_neither_blocked_nor_killed() {
        let (_temp, root, manager) = manager();
        manager
            .start_process(params(
                "proc-chatty",
                "(while true; do echo x; sleep 0.01; done) & echo $! > orphan.pid; exit 0",
            ))
            .await
            .expect("start");
        let entry = manager
            .entry(&ProcessId::new("proc-chatty"))
            .await
            .expect("entry exists while running");
        let output = manager
            .read_process(read_params("proc-chatty", None))
            .await
            .expect("read");
        assert!(output.exited);
        let pid = wait_for_pid_file(&root.join("orphan.pid")).await;

        // Past the drain grace the readers discard; nothing accumulates and
        // the writer keeps running. The entry handle outlives the pruning
        // that the read started.
        tokio::time::sleep(process_group::OUTPUT_DRAIN_GRACE * 2).await;
        let (discarding, retained) = {
            let state = entry.state.lock().await;
            (state.discarding, state.output.unread_bytes())
        };
        assert!(discarding);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            entry.state.lock().await.output.unread_bytes(),
            retained,
            "stored output does not grow after the drain grace"
        );
        assert!(process_alive(pid), "the writer was not killed by EPIPE");

        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid).expect("valid child pid"),
            rustix::process::Signal::KILL,
        );
        wait_for_process_gone(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn kill_after_exit_stops_the_leftover_group() {
        let (_temp, root, manager) = manager();
        manager
            .start_process(params(
                "proc-service",
                "nohup sleep 30 >/dev/null 2>&1 & echo $! > orphan.pid; exit 0",
            ))
            .await
            .expect("start");
        let pid = wait_for_pid_file(&root.join("orphan.pid")).await;
        assert!(process_alive(pid));
        for _ in 0..200 {
            if manager.running_count().await == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(manager.leftover_group_count(), 1);

        let terminated = manager
            .terminate_process(TerminateProcessParams {
                process_id: ProcessId::new("proc-service"),
                signal: ProcessSignal::Kill,
            })
            .await
            .expect("kill");
        assert!(!terminated.running, "the root had already exited");
        wait_for_process_gone(pid).await;
        assert_eq!(manager.leftover_group_count(), 0);
        let output = manager
            .read_process(read_params("proc-service", Some(0)))
            .await
            .expect("first read after the kill");
        assert!(output.exited);
        assert!(
            output.leftover_processes.is_empty(),
            "a dead group is no longer reported"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn process_injects_secret_env_and_redacts_output() {
        let (_temp, _root, manager) = manager();
        let mut params = params("proc-secret", "printf \"$SECRET_TOKEN\"");
        params.secret_env = BTreeMap::from([(
            "SECRET_TOKEN".to_owned(),
            SecretString::new("super-secret-token"),
        )]);
        manager.start_process(params).await.expect("start");

        let output = manager
            .read_process(read_params("proc-secret", None))
            .await
            .expect("read");
        assert_eq!(
            stream_bytes(&output, ProcessOutputStream::Stdout),
            b"<redacted>"
        );
    }
}

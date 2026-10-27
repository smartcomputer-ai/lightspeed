//! One running root process: output capture, exit, deadline and signals.

use std::{
    io::Read as _,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use environment_protocol::{
    data::exec::{ExitStatus, LeftoverDisposition, LeftoverReport, OutputStream, StoppedBy},
    error::ProtocolError,
};
use portable_pty::MasterPty;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _},
    process::{Child, ChildStderr, ChildStdout},
    sync::watch,
    task::JoinHandle,
};

use super::{input::Input, outcome};
use crate::{
    process_group::{self, Group, GroupSignal, OUTPUT_DRAIN_GRACE},
    redaction::redact_bytes,
};

const READ_BUFFER_BYTES: usize = 8192;

/// Where captured, redacted output goes.
pub(crate) trait OutputSink: Send + 'static {
    fn append(&mut self, stream: OutputStream, data: &[u8]);
}

/// What happens to the output readers once the root exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AfterExit {
    /// Processes: readers drain for a grace period, then stay on the pipes
    /// discarding, so leftovers holding them never see `EPIPE`.
    Keep,
    /// Jobs: the owner sweeps the group first, then calls
    /// [`Execution::settle`].
    Manual,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExecOptions {
    /// `timeoutMs`: stop with `SIGTERM`, the grace, then `SIGKILL`.
    pub timeout: Option<Duration>,
    /// Grace between `SIGTERM` and `SIGKILL` for terminate and timeout.
    pub terminate_grace: Duration,
    pub after_exit: AfterExit,
}

/// A spawned root process. Cheap to clone.
pub(crate) struct Execution<S> {
    inner: Arc<Inner<S>>,
}

impl<S> Clone for Execution<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct Inner<S> {
    pid: u32,
    /// The root's process group, which it leads.
    group: Group,
    /// Latched once the group was seen empty (or its id reused) after the
    /// root exited: nothing of it is signalled or sampled again.
    group_gone: AtomicBool,
    state: Mutex<State<S>>,
    /// Bumped on every change: output, exit, settlement, stop.
    version: watch::Sender<u64>,
    /// Pipe reader tasks, aborted by a settle that abandons them.
    readers: Mutex<Vec<JoinHandle<()>>>,
    /// The PTY master, for the terminal's foreground group.
    pty: Option<Mutex<Box<dyn MasterPty + Send>>>,
    input: Input,
    terminate_grace: Duration,
}

/// What an execution has observed so far.
pub(crate) struct State<S> {
    pub sink: S,
    redactions: Vec<Vec<u8>>,
    readers_open: u32,
    discarding: bool,
    exited: bool,
    exit: Option<ExitStatus>,
    error: Option<ProtocolError>,
    stopped_by: Option<StoppedBy>,
    settled_at: Option<Instant>,
}

impl<S> State<S> {
    /// The root process exited.
    pub fn exited(&self) -> bool {
        self.exited
    }

    /// The root exited and its output settled: every reader reached end of
    /// file or was discarded, so the output's end is final.
    pub fn settled(&self) -> bool {
        self.settled_at.is_some()
    }

    pub fn settled_at(&self) -> Option<Instant> {
        self.settled_at
    }

    pub fn exit(&self) -> Option<&ExitStatus> {
        self.exit.as_ref()
    }

    /// Why output capture or the wait failed, if either did.
    pub fn error(&self) -> Option<&ProtocolError> {
        self.error.as_ref()
    }

    pub fn stopped_by(&self) -> Option<StoppedBy> {
        self.stopped_by
    }
}

impl<S: OutputSink> Execution<S> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_pipes(
        pid: u32,
        child: Child,
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
        input: Input,
        sink: S,
        redactions: Vec<Vec<u8>>,
        options: ExecOptions,
    ) -> Self {
        let readers_open = u32::from(stdout.is_some()) + u32::from(stderr.is_some());
        let execution = Self::new(pid, None, input, sink, redactions, readers_open, options);
        let inner = &execution.inner;
        let mut readers = Vec::new();
        if let Some(stdout) = stdout {
            readers.push(tokio::spawn(read_pipe(
                inner.clone(),
                stdout,
                OutputStream::Stdout,
            )));
        }
        if let Some(stderr) = stderr {
            readers.push(tokio::spawn(read_pipe(
                inner.clone(),
                stderr,
                OutputStream::Stderr,
            )));
        }
        *lock(&inner.readers) = readers;
        let watched = execution.clone();
        tokio::spawn(async move {
            let status = {
                let mut child = child;
                child.wait().await
            };
            watched.observe_exit(status, options.after_exit).await;
        });
        execution.start_deadline(options.timeout);
        execution
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_pty(
        pid: u32,
        child: Box<dyn portable_pty::Child + Send + Sync>,
        master: Box<dyn MasterPty + Send>,
        reader: Box<dyn std::io::Read + Send>,
        input: Input,
        sink: S,
        redactions: Vec<Vec<u8>>,
        options: ExecOptions,
    ) -> Self {
        let execution = Self::new(pid, Some(master), input, sink, redactions, 1, options);
        // PTY reads and waits block. They run on threads of their own, not
        // the runtime's blocking pool, so a PTY that leftovers keep open can
        // never hold up the daemon's shutdown.
        let inner = execution.inner.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("envd-pty-reader".to_owned())
            .spawn(move || read_pty(inner, reader))
        {
            execution.inner.capture_failed(OutputStream::Pty, error);
            execution.inner.reader_closed();
        }
        // portable-pty's own exit status reports every signal death as
        // code 1; the std child underneath keeps the signal.
        let (exited, status) = tokio::sync::oneshot::channel();
        let waiter = match child.into_any().downcast::<std::process::Child>() {
            Ok(mut child) => std::thread::Builder::new()
                .name("envd-pty-wait".to_owned())
                .spawn(move || {
                    let _ = exited.send(child.wait());
                })
                .map(drop),
            Err(_) => {
                let _ = exited.send(Err(std::io::Error::other(
                    "the PTY child cannot be waited on",
                )));
                Ok(())
            }
        };
        let watched = execution.clone();
        tokio::spawn(async move {
            let status = match waiter {
                Ok(()) => status
                    .await
                    .unwrap_or_else(|_| Err(std::io::Error::other("the PTY wait thread ended"))),
                Err(error) => Err(error),
            };
            watched.observe_exit(status, options.after_exit).await;
        });
        execution.start_deadline(options.timeout);
        execution
    }

    fn new(
        pid: u32,
        pty: Option<Box<dyn MasterPty + Send>>,
        input: Input,
        sink: S,
        redactions: Vec<Vec<u8>>,
        readers_open: u32,
        options: ExecOptions,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                pid,
                group: Group::capture(pid),
                group_gone: AtomicBool::new(false),
                state: Mutex::new(State {
                    sink,
                    redactions,
                    readers_open,
                    discarding: false,
                    exited: false,
                    exit: None,
                    error: None,
                    stopped_by: None,
                    settled_at: None,
                }),
                version: watch::Sender::new(0),
                readers: Mutex::new(Vec::new()),
                pty: pty.map(Mutex::new),
                input,
                terminate_grace: options.terminate_grace,
            }),
        }
    }

    async fn observe_exit(
        &self,
        status: std::io::Result<std::process::ExitStatus>,
        after_exit: AfterExit,
    ) {
        {
            let mut state = self.state();
            state.exited = true;
            match status {
                Ok(status) => state.exit = Some(outcome::exit_status(status)),
                Err(error) => {
                    state
                        .error
                        .get_or_insert(ProtocolError::process_failed(format!(
                            "wait for process {}: {error}",
                            self.inner.pid
                        )));
                }
            }
        }
        self.inner.input.shut();
        self.bump();
        if after_exit == AfterExit::Keep {
            self.settle(false).await;
        }
    }

    fn start_deadline(&self, timeout: Option<Duration>) {
        let Some(timeout) = timeout else {
            return;
        };
        let execution = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            if let Some(inner) = execution.upgrade() {
                let execution = Execution { inner };
                if !execution.state().exited {
                    execution.stop(StoppedBy::Timeout);
                }
            }
        });
    }
}

impl<S> Execution<S> {
    /// OS pid of the root, which is also its process group id.
    pub fn pid(&self) -> u32 {
        self.inner.pid
    }

    /// The root's process group.
    pub fn group(&self) -> Group {
        self.inner.group
    }

    /// The root runs, or its group still has members of its own. After the
    /// root exited, an emptied group latches as gone, so a later group that
    /// reuses the id is never mistaken for this one.
    pub fn group_alive(&self) -> bool {
        if !self.state().exited {
            return true;
        }
        if self.inner.group_gone.load(Ordering::Acquire) {
            return false;
        }
        let alive = self.inner.group.alive();
        if !alive {
            self.inner.group_gone.store(true, Ordering::Release);
        }
        alive
    }

    /// What is alive in the group now, as a report with `disposition`.
    /// Absent when nothing is left or the platform cannot enumerate groups.
    pub fn sample_leftovers(&self, disposition: LeftoverDisposition) -> Option<LeftoverReport> {
        if !self.group_alive() {
            return None;
        }
        outcome::leftover_report(process_group::group_members(self.inner.pid), disposition)
    }

    /// Runs `f` on the current state under the execution's lock.
    pub fn with_state<R>(&self, f: impl FnOnce(&mut State<S>) -> R) -> R {
        f(&mut self.state())
    }

    /// Notified on every change.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.version.subscribe()
    }

    /// Starts the input writer: the last step of a start.
    pub fn start_input(&self) {
        self.inner.input.start();
    }

    /// Queues input; see [`Input::offer`].
    pub fn offer_input(&self, data: &[u8], close: bool) -> Result<u64, ProtocolError> {
        self.inner.input.offer(data, close)
    }

    /// Waits until `done` holds.
    pub async fn wait_until(&self, done: impl Fn(&State<S>) -> bool) {
        let mut changes = self.subscribe();
        loop {
            changes.borrow_and_update();
            if done(&self.state()) {
                return;
            }
            if changes.changed().await.is_err() {
                return;
            }
        }
    }

    /// Settles the output after the root exited: waits up to the drain
    /// grace for the readers to reach end of file, then discards what the
    /// rest still read, and with `abort_readers` also abandons the pipes.
    pub async fn settle(&self, abort_readers: bool) {
        let mut changes = self.subscribe();
        let deadline = tokio::time::Instant::now() + OUTPUT_DRAIN_GRACE;
        loop {
            changes.borrow_and_update();
            if self.state().readers_open == 0 {
                break;
            }
            tokio::select! {
                changed = changes.changed() => if changed.is_err() { break },
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        if abort_readers {
            for reader in std::mem::take(&mut *lock(&self.inner.readers)) {
                reader.abort();
            }
        }
        {
            let mut state = self.state();
            if state.readers_open > 0 {
                state.discarding = true;
            }
            // Nothing is captured any more, so the secret values go.
            state.redactions = Vec::new();
            state.settled_at.get_or_insert_with(Instant::now);
        }
        self.bump();
    }

    /// Stops the group: `kill` at once, `terminate` and `timeout` with
    /// `SIGTERM`, the grace, then `SIGKILL`. Records `stoppedBy` only while
    /// the root still runs; after its exit the signal still reaches its
    /// leftovers. Returns whether the root was running.
    pub fn stop(&self, reason: StoppedBy) -> bool {
        let running = {
            let mut state = self.state();
            let running = !state.exited;
            if running && (state.stopped_by.is_none() || reason == StoppedBy::Kill) {
                state.stopped_by = Some(reason);
            }
            running
        };
        if running || self.group_alive() {
            let group = self.inner.group;
            match reason {
                StoppedBy::Kill => {
                    group.signal(GroupSignal::Kill);
                }
                StoppedBy::Terminate | StoppedBy::Timeout => {
                    if group.signal(GroupSignal::Terminate) {
                        tokio::spawn(group.kill_after_grace(self.inner.terminate_grace));
                    }
                }
            }
        }
        self.bump();
        running
    }

    /// `SIGINT` to the foreground group: the terminal's under a PTY,
    /// otherwise the process group. Returns whether the root was running.
    pub fn interrupt(&self) -> bool {
        let running = !self.state().exited;
        if !self.group_alive() {
            return false;
        }
        let foreground = self.inner.pty.as_ref().and_then(|master| {
            lock(master)
                .process_group_leader()
                .and_then(|leader| u32::try_from(leader).ok())
        });
        match foreground {
            Some(foreground) => process_group::interrupt_foreground(foreground),
            None => self.inner.group.signal(GroupSignal::Interrupt),
        };
        running
    }

    fn state(&self) -> MutexGuard<'_, State<S>> {
        lock(&self.inner.state)
    }

    fn bump(&self) {
        self.inner
            .version
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}

impl<S: OutputSink> Inner<S> {
    fn push(&self, stream: OutputStream, data: &[u8]) {
        {
            let mut state = lock(&self.state);
            if state.discarding {
                return;
            }
            if state.redactions.is_empty() {
                state.sink.append(stream, data);
            } else {
                let redacted = redact_bytes(data, &state.redactions);
                state.sink.append(stream, &redacted);
            }
        }
        self.bump();
    }

    fn capture_failed(&self, stream: OutputStream, error: std::io::Error) {
        let mut state = lock(&self.state);
        if !state.exited {
            state
                .error
                .get_or_insert(ProtocolError::process_failed(format!(
                    "capture {stream:?} output: {error}"
                )));
        }
    }

    fn reader_closed(&self) {
        {
            let mut state = lock(&self.state);
            state.readers_open = state.readers_open.saturating_sub(1);
        }
        self.bump();
    }

    fn bump(&self) {
        self.version
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}

async fn read_pipe<S: OutputSink, R: AsyncRead + Unpin>(
    inner: Arc<Inner<S>>,
    mut reader: R,
    stream: OutputStream,
) {
    let mut buffer = vec![0; READ_BUFFER_BYTES];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => inner.push(stream, &buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                inner.capture_failed(stream, error);
                break;
            }
        }
    }
    inner.reader_closed();
}

fn read_pty<S: OutputSink>(inner: Arc<Inner<S>>, mut reader: Box<dyn std::io::Read + Send>) {
    let mut buffer = vec![0; READ_BUFFER_BYTES];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => inner.push(OutputStream::Pty, &buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            // Linux answers EIO once every slave descriptor is closed: the
            // PTY's end of file.
            Err(_) => break,
        }
    }
    inner.reader_closed();
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

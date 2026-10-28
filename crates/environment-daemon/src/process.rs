//! `process/*`: interactive processes on the shared execution primitive.
//!
//! Every command starts as the leader of its own process group. Its natural
//! exit never sweeps the group: whatever the command left running keeps the
//! environment's lifetime and is reported as leftovers, sampled when a read
//! returns the settled exit. Only a timeout or an explicit terminate or kill
//! signals the group.
//!
//! Output is one byte stream per process, read through a daemon-owned
//! cursor that advances only after a response was written, or at explicit
//! offsets that never consume. Retention is bounded per process, per exited
//! process and across exited processes; an evicted handle leaves a
//! tombstone that answers `notFound {expired}`. Spawn records let a
//! restarted daemon adopt the groups an earlier instance left running and
//! answer their handles with `notFound {daemonRestarted}`.

use std::{
    collections::{BTreeMap, VecDeque},
    fmt::Write as _,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use environment_protocol::{
    data::{
        exec::{
            ExecInput, ExecState, LeftoverDisposition, OutputPage, OutputStream, PtySize, ReturnOn,
            StoppedBy,
        },
        handshake::ProcessLimits,
        process::{
            ProcessSignal, ReadProcessParams, ReadProcessResponse, StartProcessParams,
            StartProcessResponse, TerminateProcessParams, TerminateProcessResponse,
            WriteProcessParams, WriteProcessResponse,
        },
    },
    error::{Capability, NotFoundReason, ProtocolError},
    ids::{DaemonInstanceId, ProcessId},
    limits::MAX_PAGE_CHUNKS,
};
use serde::{Deserialize, Serialize};

use crate::{
    dispatch::{Reply, RequestContext},
    exec::{
        AfterExit, ExecOptions, Execution, OutputSink, PreparedCommand,
        log::OutputLog,
        outcome::describe_exit,
        spawn_record::{SpawnRecord, write_json_atomic},
        validate_command,
    },
    filesystem::Confinement,
    process_group::{self, Group},
};

/// How long a tombstone answers `notFound {expired}`.
const TOMBSTONE_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// Most tombstones kept; the oldest go first.
const MAX_TOMBSTONES: usize = 4096;

/// Everything a [`ProcessManager`] needs from the daemon.
pub struct ProcessConfig {
    /// Canonical default `cwd`.
    pub cwd: PathBuf,
    /// Resolves every `cwd`.
    pub confinement: Arc<Confinement>,
    /// Spawn records live in `<state_dir>/processes`.
    pub state_dir: PathBuf,
    /// Daemon variables no child inherits.
    pub scrubbed_env: Vec<String>,
    pub instance_id: DaemonInstanceId,
    pub limits: ProcessLimits,
    /// `limits.maxWaitMs`, the `waitMs` ceiling.
    pub max_wait_ms: u64,
    /// `capabilities.process.pty`.
    pub pty_supported: bool,
}

/// The daemon's processes. Cheap to clone.
#[derive(Clone)]
pub struct ProcessManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    config: ProcessConfig,
    records_dir: PathBuf,
    table: Mutex<Table>,
    /// Groups no live entry owns any more that still had members when last
    /// looked at: groups of evicted entries and groups adopted from an
    /// earlier daemon instance. Each remembers its spawn record, deleted
    /// once the group is empty.
    leftover_groups: Mutex<BTreeMap<u32, (Group, PathBuf)>>,
}

#[derive(Default)]
struct Table {
    slots: BTreeMap<ProcessId, Slot>,
    /// Tombstoned ids in eviction order.
    tombstones: VecDeque<(ProcessId, Instant)>,
}

enum Slot {
    /// A start is spawning under this id.
    Reserved,
    Live(Arc<Entry>),
    /// Evicted; remembers how it ended.
    Tombstone(String),
    /// A handle of an earlier daemon instance.
    Restarted,
}

struct Entry {
    execution: Execution<ProcessOutput>,
    record_path: PathBuf,
    /// When a reply returning the settled exit with nothing left unread was
    /// written.
    fully_read_at: Mutex<Option<Instant>>,
}

/// A process's retained output and its daemon cursor.
struct ProcessOutput {
    log: OutputLog,
    cursor: u64,
    cap: u64,
}

impl OutputSink for ProcessOutput {
    fn append(&mut self, stream: OutputStream, data: &[u8]) {
        self.log.append(stream, data);
        self.log.enforce_cap(self.cap, self.cursor);
    }
}

/// The file of one spawn record.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredSpawn {
    process_id: ProcessId,
    spawn: SpawnRecord,
}

impl ProcessManager {
    /// Reconciles the spawn records an earlier instance left: groups of
    /// this boot that still run are adopted as leftovers, and their ids
    /// answer `notFound {daemonRestarted}`; every other record is deleted.
    pub fn new(config: ProcessConfig) -> anyhow::Result<Self> {
        let records_dir = config.state_dir.join("processes");
        std::fs::create_dir_all(&records_dir)?;
        let boot = process_group::boot_id();
        let mut table = Table::default();
        let mut leftover_groups = BTreeMap::new();
        for entry in std::fs::read_dir(&records_dir)? {
            let path = entry?.path();
            let stored = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<StoredSpawn>(&bytes).ok());
            let Some(stored) = stored else {
                eprintln!(
                    "lightspeed-envd: removing unreadable spawn record {}",
                    path.display()
                );
                let _ = std::fs::remove_file(&path);
                continue;
            };
            if !stored.spawn.same_boot(boot.as_deref()) {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            table.slots.insert(stored.process_id, Slot::Restarted);
            if stored.spawn.group_survives() {
                leftover_groups.insert(stored.spawn.pgid, (stored.spawn.group(), path));
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
        Ok(Self {
            inner: Arc::new(ManagerInner {
                config,
                records_dir,
                table: Mutex::new(table),
                leftover_groups: Mutex::new(leftover_groups),
            }),
        })
    }

    /// Processes whose root has not exited.
    pub async fn running_count(&self) -> u32 {
        self.maintain();
        let table = self.table();
        let running = table
            .slots
            .values()
            .filter(|slot| match slot {
                Slot::Live(entry) => !entry.execution.with_state(|state| state.exited()),
                _ => false,
            })
            .count();
        u32::try_from(running).unwrap_or(u32::MAX)
    }

    /// Process groups whose root exited while members still run, adopted
    /// groups of an earlier instance included. They hold the idle clock
    /// like running work.
    pub fn leftover_group_count(&self) -> u32 {
        let live = {
            let table = self.table();
            table
                .slots
                .values()
                .filter(|slot| match slot {
                    Slot::Live(entry) => {
                        entry.execution.with_state(|state| state.exited())
                            && entry.execution.group_alive()
                    }
                    _ => false,
                })
                .count()
        };
        let mut groups = lock(&self.inner.leftover_groups);
        groups.retain(|_, (group, record)| {
            let alive = group.alive();
            if !alive {
                let _ = std::fs::remove_file(record);
            }
            alive
        });
        u32::try_from(live + groups.len()).unwrap_or(u32::MAX)
    }

    pub async fn start(
        &self,
        _ctx: &RequestContext,
        params: StartProcessParams,
    ) -> Result<StartProcessResponse, ProtocolError> {
        let StartProcessParams {
            process_id,
            command,
            pty,
        } = params;
        let config = &self.inner.config;
        validate_command(&command, "command")?;
        if let Some(size) = pty {
            validate_pty(size, &command.input)?;
            if !config.pty_supported {
                return Err(ProtocolError::capability_unavailable(
                    Capability::ProcessPty,
                    "this daemon cannot run processes under a PTY",
                ));
            }
        }
        let cwd = match &command.cwd {
            Some(path) => config.confinement.resolve_directory(path)?,
            None => config.cwd.clone(),
        };
        self.maintain();
        self.reserve(&process_id)?;

        let options = ExecOptions {
            timeout: command.timeout_ms.map(Duration::from_millis),
            terminate_grace: Duration::from_millis(config.limits.terminate_grace_ms),
            after_exit: AfterExit::Keep,
        };
        let sink = ProcessOutput {
            log: OutputLog::default(),
            cursor: 0,
            cap: config.limits.max_retained_output_bytes,
        };
        let prepared = PreparedCommand::new(command, cwd, &config.scrubbed_env, pty);
        let execution = match prepared.spawn(sink, options, config.limits.max_input_queue_bytes) {
            Ok(execution) => execution,
            Err(error) => {
                self.release(&process_id);
                return Err(error);
            }
        };
        let pid = execution.pid();
        let record_path = self
            .inner
            .records_dir
            .join(format!("{}.json", process_id.as_str()));
        let entry = Arc::new(Entry {
            execution: execution.clone(),
            record_path: record_path.clone(),
            fully_read_at: Mutex::new(None),
        });
        self.table()
            .slots
            .insert(process_id.clone(), Slot::Live(entry));
        let stored = StoredSpawn {
            process_id,
            spawn: SpawnRecord::new(execution.group(), &config.instance_id),
        };
        if let Err(error) = write_json_atomic(&record_path, &stored) {
            eprintln!(
                "lightspeed-envd: could not persist the spawn record of {}: {error}",
                stored.process_id
            );
        }
        execution.start_input();
        Ok(StartProcessResponse { pid })
    }

    pub async fn read(
        &self,
        ctx: &RequestContext,
        params: ReadProcessParams,
    ) -> Result<Reply<ReadProcessResponse>, ProtocolError> {
        let config = &self.inner.config;
        if params.max_bytes > config.limits.max_read_bytes {
            return Err(ProtocolError::above_ceiling(
                "maxBytes",
                params.max_bytes,
                config.limits.max_read_bytes,
            ));
        }
        if params.wait_ms > config.max_wait_ms {
            return Err(ProtocolError::above_ceiling(
                "waitMs",
                params.wait_ms,
                config.max_wait_ms,
            ));
        }
        if params.after.is_some() && params.tail_bytes.is_some() {
            return Err(ProtocolError::invalid_field(
                "tailBytes",
                "after and tailBytes exclude each other",
            ));
        }
        self.maintain();
        let entry = self.lookup(&params.process_id)?;
        let execution = &entry.execution;

        let (start, end) = execution.with_state(|state| {
            let output = &mut state.sink;
            if let Some(ack) = params.ack {
                output.cursor = output.cursor.max(ack.min(output.log.end()));
            }
            let end = output.log.end();
            let start = match (params.after, params.tail_bytes) {
                (Some(after), _) => after,
                (None, Some(tail)) => end.saturating_sub(tail),
                (None, None) => output.cursor,
            };
            (start, end)
        });
        if start > end {
            return Err(ProtocolError::invalid_field(
                "after",
                format!("after {start} is past the end of the output ({end})"),
            ));
        }

        let deadline = tokio::time::Instant::now() + Duration::from_millis(params.wait_ms);
        let mut changes = execution.subscribe();
        loop {
            changes.borrow_and_update();
            let ready = execution.with_state(|state| {
                state.settled()
                    || match params.return_on {
                        ReturnOn::AnyOutput => state.sink.log.end() > start,
                        ReturnOn::Window => state.sink.log.end() - start >= params.max_bytes,
                        ReturnOn::Exit => false,
                    }
            });
            if ready {
                break;
            }
            tokio::select! {
                () = ctx.cancel.cancelled() => return Err(ProtocolError::cancelled()),
                () = tokio::time::sleep_until(deadline) => break,
                changed = changes.changed() => if changed.is_err() { break },
            }
        }

        let (mut response, settled) = execution.with_state(|state| {
            let page = state
                .sink
                .log
                .page(start, params.max_bytes, MAX_PAGE_CHUNKS);
            let settled = state.settled();
            let response = ReadProcessResponse {
                output: OutputPage {
                    chunks: page.chunks,
                    next: page.next,
                    end: state.sink.log.end(),
                    gaps: page.gaps,
                },
                state: if settled {
                    ExecState::Exited
                } else {
                    ExecState::Running
                },
                exit: state.exit().filter(|_| settled).cloned(),
                stopped_by: state.stopped_by().filter(|_| settled),
                error: state.error().cloned(),
                pid: execution.pid(),
                leftovers: None,
            };
            (response, settled)
        });
        if settled {
            response.leftovers = execution.sample_leftovers(LeftoverDisposition::Kept);
        }

        let consuming = params.after.is_none() && params.tail_bytes.is_none();
        let complete = settled && response.output.next == response.output.end;
        if !consuming && !complete {
            return Ok(Reply::new(response));
        }
        let next = response.output.next;
        let entry = entry.clone();
        Ok(Reply::with_commit(response, move || {
            if consuming {
                entry.execution.with_state(|state| {
                    state.sink.cursor = state.sink.cursor.max(next);
                });
            }
            if complete {
                lock(&entry.fully_read_at).get_or_insert_with(Instant::now);
            }
        }))
    }

    pub async fn write(
        &self,
        _ctx: &RequestContext,
        params: WriteProcessParams,
    ) -> Result<WriteProcessResponse, ProtocolError> {
        self.maintain();
        let entry = self.lookup(&params.process_id)?;
        let accepted_bytes = entry
            .execution
            .offer_input(params.data.as_slice(), params.close_input)?;
        Ok(WriteProcessResponse { accepted_bytes })
    }

    pub async fn terminate(
        &self,
        _ctx: &RequestContext,
        params: TerminateProcessParams,
    ) -> Result<TerminateProcessResponse, ProtocolError> {
        self.maintain();
        let entry = self.lookup(&params.process_id)?;
        let running = match params.signal {
            ProcessSignal::Interrupt => entry.execution.interrupt(),
            ProcessSignal::Terminate => entry.execution.stop(StoppedBy::Terminate),
            ProcessSignal::Kill => entry.execution.stop(StoppedBy::Kill),
        };
        Ok(TerminateProcessResponse { running })
    }

    /// Claims `process_id` before any side effect: a running, retained,
    /// tombstoned or earlier-instance id is `alreadyExists`.
    fn reserve(&self, process_id: &ProcessId) -> Result<(), ProtocolError> {
        let mut table = self.table();
        if table.slots.contains_key(process_id) {
            return Err(ProtocolError::already_exists(format!(
                "process id {process_id} is already in use"
            )));
        }
        table.slots.insert(process_id.clone(), Slot::Reserved);
        Ok(())
    }

    fn release(&self, process_id: &ProcessId) {
        let mut table = self.table();
        if matches!(table.slots.get(process_id), Some(Slot::Reserved)) {
            table.slots.remove(process_id);
        }
    }

    fn lookup(&self, process_id: &ProcessId) -> Result<Arc<Entry>, ProtocolError> {
        match self.table().slots.get(process_id) {
            Some(Slot::Live(entry)) => Ok(entry.clone()),
            Some(Slot::Tombstone(ended)) => Err(ProtocolError::not_found_because(
                NotFoundReason::Expired,
                format!("process {process_id} {ended} and its output is no longer retained"),
            )),
            Some(Slot::Restarted) => Err(ProtocolError::not_found_because(
                NotFoundReason::DaemonRestarted,
                format!("process {process_id} belongs to an earlier daemon instance"),
            )),
            Some(Slot::Reserved) | None => Err(ProtocolError::not_found(format!(
                "unknown process id: {process_id}"
            ))),
        }
    }

    /// Evicts settled entries past their retention or beyond the exited
    /// byte budget, oldest exit first, and prunes old tombstones.
    fn maintain(&self) {
        let limits = &self.inner.config.limits;
        let now = Instant::now();
        let after_read = Duration::from_millis(limits.retention_after_exit_ms);
        let unread = Duration::from_millis(limits.unread_retention_ms);
        let mut table = self.table();

        let mut settled = Vec::new();
        let mut evict = Vec::new();
        for (process_id, slot) in &table.slots {
            let Slot::Live(entry) = slot else {
                continue;
            };
            let Some((settled_at, retained)) = entry.execution.with_state(|state| {
                state
                    .settled_at()
                    .map(|at| (at, state.sink.log.retained_bytes()))
            }) else {
                continue;
            };
            let read_at = *lock(&entry.fully_read_at);
            let expired =
                read_at.is_some_and(|at| now >= at + after_read) || now >= settled_at + unread;
            if expired {
                evict.push(process_id.clone());
            } else {
                settled.push((settled_at, retained, process_id.clone()));
            }
        }
        settled.sort();
        let mut total: u64 = settled.iter().map(|(_, retained, _)| retained).sum();
        for (_, retained, process_id) in settled {
            if total <= limits.max_exited_retained_bytes {
                break;
            }
            total -= retained;
            evict.push(process_id);
        }

        for process_id in evict {
            let Some(Slot::Live(entry)) = table.slots.remove(&process_id) else {
                continue;
            };
            let ended = entry.execution.with_state(|state| {
                let mut ended = state
                    .exit()
                    .map_or_else(|| "ended".to_owned(), describe_exit);
                if let Some(stopped_by) = state.stopped_by() {
                    let _ = write!(ended, " (stopped by {stopped_by:?})");
                }
                // Readers a leftover keeps on the pipes outlive the entry;
                // they discard, and the retained output goes now.
                state.sink.log = OutputLog::default();
                ended
            });
            let group = entry.execution.group();
            if entry.execution.group_alive() {
                lock(&self.inner.leftover_groups)
                    .insert(group.pgid, (group, entry.record_path.clone()));
            } else {
                let _ = std::fs::remove_file(&entry.record_path);
            }
            table
                .slots
                .insert(process_id.clone(), Slot::Tombstone(ended));
            table.tombstones.push_back((process_id, now));
        }

        while let Some((_, evicted_at)) = table.tombstones.front() {
            if table.tombstones.len() <= MAX_TOMBSTONES && now < *evicted_at + TOMBSTONE_RETENTION {
                break;
            }
            let (process_id, _) = table.tombstones.pop_front().expect("front exists");
            if matches!(table.slots.get(&process_id), Some(Slot::Tombstone(_))) {
                table.slots.remove(&process_id);
            }
        }
    }

    fn table(&self) -> MutexGuard<'_, Table> {
        lock(&self.inner.table)
    }
}

fn validate_pty(size: PtySize, input: &ExecInput) -> Result<(), ProtocolError> {
    let valid = 1..=PtySize::MAX_DIMENSION;
    if !valid.contains(&size.rows) || !valid.contains(&size.cols) {
        return Err(ProtocolError::invalid_field(
            "pty",
            format!("PTY rows and cols must be 1 to {}", PtySize::MAX_DIMENSION),
        ));
    }
    if matches!(input, ExecInput::Pipe {}) {
        return Err(ProtocolError::invalid_field(
            "command.input",
            "a PTY is the input; pipe input cannot be combined with pty",
        ));
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use environment_protocol::{
        bytes::{ByteChunk, SecretString},
        data::exec::{CommandSpec, ExitStatus, GapReason, SignalName},
        error::{ErrorData, PreconditionReason, ProtocolErrorKind},
    };

    use super::*;

    fn limits() -> ProcessLimits {
        ProcessLimits {
            max_read_bytes: 4 * 1024 * 1024,
            max_retained_output_bytes: 4 * 1024 * 1024,
            retention_after_exit_ms: 60_000,
            unread_retention_ms: 7_200_000,
            max_exited_retained_bytes: 64 * 1024 * 1024,
            terminate_grace_ms: 200,
            max_input_queue_bytes: 64 * 1024,
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        state_dir: PathBuf,
        manager: ProcessManager,
    }

    fn fixture_with(limits: ProcessLimits) -> Fixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let state_dir = root.join("state");
        let manager = manager_at(&root, &state_dir, limits, "envd_first");
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

    fn manager_at(
        root: &std::path::Path,
        state_dir: &std::path::Path,
        limits: ProcessLimits,
        instance: &str,
    ) -> ProcessManager {
        let confinement =
            Confinement::new(root.to_owned(), state_dir.to_owned()).expect("confinement");
        ProcessManager::new(ProcessConfig {
            cwd: root.to_owned(),
            confinement: Arc::new(confinement),
            state_dir: state_dir.to_owned(),
            scrubbed_env: Vec::new(),
            instance_id: DaemonInstanceId::new(instance).unwrap(),
            limits,
            max_wait_ms: 60_000,
            pty_supported: true,
        })
        .expect("manager")
    }

    fn ctx() -> RequestContext {
        RequestContext::detached()
    }

    fn id(value: &str) -> ProcessId {
        ProcessId::new(value).unwrap()
    }

    fn shell(process_id: &str, script: &str) -> StartProcessParams {
        StartProcessParams {
            process_id: id(process_id),
            command: CommandSpec {
                argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
                cwd: None,
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                input: ExecInput::None {},
                timeout_ms: Some(30_000),
            },
            pty: None,
        }
    }

    fn read_params(process_id: &str, return_on: ReturnOn, wait_ms: u64) -> ReadProcessParams {
        ReadProcessParams {
            process_id: id(process_id),
            after: None,
            tail_bytes: None,
            ack: None,
            max_bytes: 4 * 1024 * 1024,
            wait_ms,
            return_on,
        }
    }

    fn written(reply: Reply<ReadProcessResponse>) -> ReadProcessResponse {
        if let Some(hook) = reply.on_written {
            hook();
        }
        reply.value
    }

    fn data(response: &ReadProcessResponse) -> Vec<u8> {
        response
            .output
            .chunks
            .iter()
            .flat_map(|chunk| chunk.data.as_slice().to_vec())
            .collect()
    }

    /// Cursor reads until the settled exit with nothing left unread.
    async fn read_to_end(
        manager: &ProcessManager,
        process_id: &str,
    ) -> (Vec<u8>, ReadProcessResponse) {
        let mut bytes = Vec::new();
        loop {
            let reply = manager
                .read(&ctx(), read_params(process_id, ReturnOn::AnyOutput, 10_000))
                .await
                .expect("read");
            let response = written(reply);
            bytes.extend(data(&response));
            if response.state == ExecState::Exited && response.output.next == response.output.end {
                return (bytes, response);
            }
        }
    }

    fn not_found_reason(error: &ProtocolError) -> Option<NotFoundReason> {
        match &error.data {
            ErrorData::NotFound { reason } => *reason,
            _ => panic!("expected notFound, got {error:?}"),
        }
    }

    #[cfg(unix)]
    async fn wait_for_pid_file(path: &std::path::Path) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(content) = std::fs::read_to_string(path)
                && let Ok(pid) = content.trim().parse()
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "pid file was not written");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    fn process_alive(pid: i32) -> bool {
        rustix::process::Pid::from_raw(pid)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
    }

    #[cfg(unix)]
    fn kill(pid: i32) {
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid).expect("pid"),
            rustix::process::Signal::KILL,
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reports_both_streams_exit_code_and_pid() {
        let fixture = fixture();
        let started = fixture
            .manager
            .start(&ctx(), shell("p1", "printf out; printf err >&2; exit 3"))
            .await
            .expect("start");
        let (bytes, response) = read_to_end(&fixture.manager, "p1").await;
        assert_eq!(response.pid, started.pid);
        assert_eq!(response.exit, Some(ExitStatus::Code { code: 3 }));
        assert_eq!(response.stopped_by, None);
        assert!(response.leftovers.is_none());
        // The two streams interleave in arrival order.
        let mut sorted = bytes;
        sorted.sort_unstable();
        let mut expected = b"outerr".to_vec();
        expected.sort_unstable();
        assert_eq!(sorted, expected);
        assert_eq!(response.output.end, 6);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn large_bytes_input_never_blocks_the_start() {
        // More input than any pipe buffer, to a child that writes before it
        // has read everything: the start must not wait for the input.
        let fixture = fixture();
        let input: Vec<u8> = (0..1024 * 1024u32).map(|i| b'a' + (i % 26) as u8).collect();
        let mut params = shell("cat", "cat");
        params.command.input = ExecInput::Bytes {
            data: ByteChunk::from(input.clone()),
        };
        tokio::time::timeout(
            Duration::from_secs(5),
            fixture.manager.start(&ctx(), params),
        )
        .await
        .expect("the start answers at once")
        .expect("start");
        let (bytes, response) = read_to_end(&fixture.manager, "cat").await;
        assert_eq!(response.exit, Some(ExitStatus::Code { code: 0 }));
        assert_eq!(bytes, input, "every byte went through");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_large_pty_write_never_freezes_the_daemon() {
        // A PTY child that never reads: a write must not block the runtime
        // thread under the entry lock until the child is killed.
        let fixture = fixture();
        let mut params = shell("pty", "sleep 30");
        params.pty = Some(PtySize { rows: 24, cols: 80 });
        fixture.manager.start(&ctx(), params).await.expect("start");
        let chunk = vec![b'x'; 16 * 1024];
        let mut accepted = 0;
        for _ in 0..64 {
            let write = tokio::time::timeout(
                Duration::from_secs(1),
                fixture.manager.write(
                    &ctx(),
                    WriteProcessParams {
                        process_id: id("pty"),
                        data: ByteChunk::from(chunk.clone()),
                        close_input: false,
                    },
                ),
            )
            .await
            .expect("a write answers without blocking")
            .expect("write");
            accepted += write.accepted_bytes;
        }
        // Whether the terminal blocks or discards input a child never reads
        // differs by platform; either way no write may exceed what was sent.
        assert!(accepted <= 64 * 16 * 1024);
        // The runtime thread is free, and a kill still works.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let terminated = fixture
            .manager
            .terminate(
                &ctx(),
                TerminateProcessParams {
                    process_id: id("pty"),
                    signal: ProcessSignal::Kill,
                },
            )
            .await
            .expect("kill");
        assert!(terminated.running);
        let (_, response) = read_to_end(&fixture.manager, "pty").await;
        assert_eq!(response.stopped_by, Some(StoppedBy::Kill));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pipe_input_streams_until_closed() {
        let fixture = fixture();
        let mut params = shell("pipe", "cat; echo done");
        params.command.input = ExecInput::Pipe {};
        fixture.manager.start(&ctx(), params).await.expect("start");
        let write = |data: &'static [u8], close_input| WriteProcessParams {
            process_id: id("pipe"),
            data: ByteChunk::from(data),
            close_input,
        };
        assert_eq!(
            fixture
                .manager
                .write(&ctx(), write(b"hello ", false))
                .await
                .unwrap()
                .accepted_bytes,
            6
        );
        assert_eq!(
            fixture
                .manager
                .write(&ctx(), write(b"world\n", true))
                .await
                .unwrap()
                .accepted_bytes,
            6
        );
        let (bytes, response) = read_to_end(&fixture.manager, "pipe").await;
        assert_eq!(bytes, b"hello world\ndone\n");
        assert_eq!(response.exit, Some(ExitStatus::Code { code: 0 }));
        let error = fixture
            .manager
            .write(&ctx(), write(b"late", false))
            .await
            .unwrap_err();
        assert!(matches!(
            error.data,
            ErrorData::FailedPrecondition {
                reason: PreconditionReason::InputClosed
            }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_cursor_moves_only_once_the_reply_is_written() {
        let fixture = fixture();
        fixture
            .manager
            .start(&ctx(), shell("cursor", "printf first; sleep 30"))
            .await
            .expect("start");
        let lost = fixture
            .manager
            .read(&ctx(), read_params("cursor", ReturnOn::AnyOutput, 5_000))
            .await
            .expect("read");
        assert_eq!(data(&lost.value), b"first");
        // The connection dropped before the frame went out: no hook runs.
        drop(lost);
        let again = written(
            fixture
                .manager
                .read(&ctx(), read_params("cursor", ReturnOn::AnyOutput, 5_000))
                .await
                .expect("read again"),
        );
        assert_eq!(data(&again), b"first", "nothing was consumed");
        let after = fixture
            .manager
            .read(&ctx(), read_params("cursor", ReturnOn::AnyOutput, 0))
            .await
            .expect("read after commit")
            .value;
        assert!(after.output.chunks.is_empty());
        assert_eq!(after.output.next, 5);

        // A cancelled wait answers aborted and consumes nothing either.
        let cancelled = ctx();
        cancelled.cancel.cancel();
        let error = fixture
            .manager
            .read(&cancelled, read_params("cursor", ReturnOn::Exit, 5_000))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::Aborted);
        fixture
            .manager
            .terminate(
                &ctx(),
                TerminateProcessParams {
                    process_id: id("cursor"),
                    signal: ProcessSignal::Kill,
                },
            )
            .await
            .expect("kill");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_end_of_a_large_output_survives_and_exit_waits_for_it() {
        // A command that prints far more than is retained and then its
        // summary: the read at exit shows the head, one gap and the end.
        let fixture = fixture_with(ProcessLimits {
            max_retained_output_bytes: 64 * 1024,
            ..limits()
        });
        fixture
            .manager
            .start(
                &ctx(),
                shell(
                    "build",
                    "i=0; while [ $i -lt 20000 ]; do echo line-$i; i=$((i+1)); done; echo 'FAILED: 3 tests'",
                ),
            )
            .await
            .expect("start");
        let response = written(
            fixture
                .manager
                .read(&ctx(), read_params("build", ReturnOn::Exit, 30_000))
                .await
                .expect("read"),
        );
        assert_eq!(response.state, ExecState::Exited);
        assert_eq!(
            response.output.next, response.output.end,
            "nothing left unread"
        );
        assert_eq!(response.output.gaps.len(), 1);
        assert_eq!(response.output.gaps[0].reason, GapReason::Overflow);
        let text = String::from_utf8(data(&response)).unwrap();
        assert!(text.starts_with("line-0\n"));
        assert!(
            text.ends_with("FAILED: 3 tests\n"),
            "the summary at the end survives"
        );
        let gap = response.output.gaps[0].omitted_bytes;
        assert_eq!(text.len() as u64 + gap, response.output.end);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pages_at_explicit_offsets_never_consume_and_tail_reads_start_at_the_end() {
        let fixture = fixture();
        fixture
            .manager
            .start(&ctx(), shell("pages", "printf abcdefgh"))
            .await
            .expect("start");
        let mut params = read_params("pages", ReturnOn::Exit, 10_000);
        // `after` past the output produced so far is refused before any
        // wait, so settle the process first with a non-consuming read.
        params.after = Some(0);
        let settled = written(fixture.manager.read(&ctx(), params.clone()).await.unwrap());
        assert_eq!(settled.state, ExecState::Exited);
        assert_eq!(settled.output.end, 8);
        params.after = Some(2);
        params.max_bytes = 3;
        let page = written(fixture.manager.read(&ctx(), params.clone()).await.unwrap());
        assert_eq!(data(&page), b"cde");
        assert_eq!(page.output.next, 5);
        params.after = None;
        params.tail_bytes = Some(2);
        let tail = written(fixture.manager.read(&ctx(), params.clone()).await.unwrap());
        assert_eq!(data(&tail), b"gh");
        params.tail_bytes = None;
        params.after = Some(9);
        let error = fixture.manager.read(&ctx(), params).await.unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::InvalidParams);
        let (all, _) = read_to_end(&fixture.manager, "pages").await;
        assert_eq!(all, b"abcdefgh", "offset reads left the cursor at 0");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_duplicate_id_runs_nothing() {
        let fixture = fixture();
        let script = "echo run >> runs.txt";
        fixture
            .manager
            .start(&ctx(), shell("dup", script))
            .await
            .expect("start");
        let error = fixture
            .manager
            .start(&ctx(), shell("dup", script))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::AlreadyExists);
        read_to_end(&fixture.manager, "dup").await;
        let runs = std::fs::read_to_string(fixture.root.join("runs.txt")).unwrap();
        assert_eq!(runs, "run\n", "the command ran once");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unread_exited_processes_expire_into_tombstones() {
        let fixture = fixture_with(ProcessLimits {
            unread_retention_ms: 50,
            ..limits()
        });
        fixture
            .manager
            .start(&ctx(), shell("old", "true"))
            .await
            .expect("start");
        for _ in 0..200 {
            if fixture.manager.running_count().await == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(process_group::OUTPUT_DRAIN_GRACE + Duration::from_millis(100)).await;
        fixture.manager.running_count().await;
        let error = fixture
            .manager
            .read(&ctx(), read_params("old", ReturnOn::Exit, 0))
            .await
            .unwrap_err();
        assert_eq!(not_found_reason(&error), Some(NotFoundReason::Expired));
        let error = fixture
            .manager
            .start(&ctx(), shell("old", "true"))
            .await
            .unwrap_err();
        assert_eq!(
            error.kind(),
            ProtocolErrorKind::AlreadyExists,
            "a tombstoned id is not reused"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_handles_are_not_found_on_every_method() {
        let fixture = fixture();
        let read = fixture
            .manager
            .read(&ctx(), read_params("nope", ReturnOn::Exit, 0))
            .await
            .unwrap_err();
        assert_eq!(not_found_reason(&read), None);
        let write = fixture
            .manager
            .write(
                &ctx(),
                WriteProcessParams {
                    process_id: id("nope"),
                    data: ByteChunk::default(),
                    close_input: true,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(write.kind(), ProtocolErrorKind::NotFound);
        let terminate = fixture
            .manager
            .terminate(
                &ctx(),
                TerminateProcessParams {
                    process_id: id("nope"),
                    signal: ProcessSignal::Kill,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(terminate.kind(), ProtocolErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn signal_deaths_are_named_on_pipes_and_ptys() {
        let fixture = fixture();
        fixture
            .manager
            .start(&ctx(), shell("segv", "kill -SEGV $$"))
            .await
            .expect("start");
        let mut pty = shell("pty-term", "kill -TERM $$");
        pty.pty = Some(PtySize { rows: 24, cols: 80 });
        fixture.manager.start(&ctx(), pty).await.expect("start PTY");
        let (_, segv) = read_to_end(&fixture.manager, "segv").await;
        assert!(matches!(
            segv.exit,
            Some(ExitStatus::Signal { ref signal, .. }) if signal == &SignalName::new("SIGSEGV").unwrap()
        ));
        let (_, term) = read_to_end(&fixture.manager, "pty-term").await;
        assert_eq!(
            term.exit,
            Some(ExitStatus::Signal {
                signal: SignalName::new("SIGTERM").unwrap(),
                core_dumped: false,
            }),
            "a PTY signal death is not exit code 1"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn terminate_escalates_to_kill_after_the_grace() {
        let fixture = fixture();
        fixture
            .manager
            .start(&ctx(), shell("stubborn", "trap '' TERM; sleep 30"))
            .await
            .expect("start");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let terminated = fixture
            .manager
            .terminate(
                &ctx(),
                TerminateProcessParams {
                    process_id: id("stubborn"),
                    signal: ProcessSignal::Terminate,
                },
            )
            .await
            .expect("terminate");
        assert!(terminated.running);
        let (_, response) = read_to_end(&fixture.manager, "stubborn").await;
        assert_eq!(response.stopped_by, Some(StoppedBy::Terminate));
        assert_eq!(
            response.exit,
            Some(ExitStatus::Signal {
                signal: SignalName::new("SIGKILL").unwrap(),
                core_dumped: false,
            })
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_timeout_stops_the_group() {
        let fixture = fixture();
        let mut params = shell("slow", "sleep 30 & echo $! > orphan.pid; sleep 30");
        params.command.timeout_ms = Some(200);
        fixture.manager.start(&ctx(), params).await.expect("start");
        let orphan = wait_for_pid_file(&fixture.root.join("orphan.pid")).await;
        let (_, response) = read_to_end(&fixture.manager, "slow").await;
        assert_eq!(response.stopped_by, Some(StoppedBy::Timeout));
        for _ in 0..100 {
            if !process_alive(orphan) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!process_alive(orphan), "the group was stopped");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn natural_exit_keeps_and_reports_leftovers() {
        let fixture = fixture();
        fixture
            .manager
            .start(
                &ctx(),
                shell(
                    "service",
                    "nohup sleep 30 >/dev/null 2>&1 & echo $! > orphan.pid; exit 0",
                ),
            )
            .await
            .expect("start");
        let orphan = wait_for_pid_file(&fixture.root.join("orphan.pid")).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (_, response) = read_to_end(&fixture.manager, "service").await;
        let leftovers = response.leftovers.expect("leftovers reported");
        assert_eq!(leftovers.disposition, LeftoverDisposition::Kept);
        assert_eq!(leftovers.processes[0].pid, orphan as u32);
        assert!(process_alive(orphan), "never swept on natural exit");
        assert_eq!(fixture.manager.leftover_group_count(), 1);
        assert_eq!(fixture.manager.running_count().await, 0);
        kill(orphan);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(flavor = "current_thread")]
    async fn a_restarted_daemon_adopts_surviving_groups() {
        let fixture = fixture();
        fixture
            .manager
            .start(
                &ctx(),
                shell(
                    "survivor",
                    "nohup sleep 30 >/dev/null 2>&1 & echo $! > orphan.pid; exit 0",
                ),
            )
            .await
            .expect("start");
        let orphan = wait_for_pid_file(&fixture.root.join("orphan.pid")).await;
        fixture
            .manager
            .start(&ctx(), shell("gone", "true"))
            .await
            .expect("start");
        read_to_end(&fixture.manager, "survivor").await;
        read_to_end(&fixture.manager, "gone").await;

        let restarted = manager_at(&fixture.root, &fixture.state_dir, limits(), "envd_second");
        let error = restarted
            .read(&ctx(), read_params("survivor", ReturnOn::Exit, 0))
            .await
            .unwrap_err();
        assert_eq!(
            not_found_reason(&error),
            Some(NotFoundReason::DaemonRestarted)
        );
        assert_eq!(
            restarted.leftover_group_count(),
            1,
            "the surviving group is adopted"
        );
        assert!(process_alive(orphan), "adopted, never killed");
        kill(orphan);
        for _ in 0..100 {
            if restarted.leftover_group_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(restarted.leftover_group_count(), 0);
        let records = std::fs::read_dir(fixture.state_dir.join("processes"))
            .unwrap()
            .count();
        assert_eq!(records, 0, "records of dead groups are deleted");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn secrets_are_redacted_and_ptys_need_the_capability() {
        let fixture = fixture();
        let mut params = shell("secret", "printf \"$TOKEN\"");
        params.command.secret_env =
            BTreeMap::from([("TOKEN".to_owned(), SecretString::new("hunter2"))]);
        fixture.manager.start(&ctx(), params).await.expect("start");
        let (bytes, _) = read_to_end(&fixture.manager, "secret").await;
        assert_eq!(bytes, b"<redacted>");

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let confinement = Confinement::new(root.clone(), root.join("state")).unwrap();
        let without_pty = ProcessManager::new(ProcessConfig {
            cwd: root.clone(),
            confinement: Arc::new(confinement),
            state_dir: root.join("state"),
            scrubbed_env: Vec::new(),
            instance_id: DaemonInstanceId::new("envd_nopty").unwrap(),
            limits: limits(),
            max_wait_ms: 1_000,
            pty_supported: false,
        })
        .unwrap();
        let mut pty = shell("pty", "true");
        pty.pty = Some(PtySize { rows: 24, cols: 80 });
        let error = without_pty.start(&ctx(), pty).await.unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::CapabilityUnavailable);
        let mut wait = read_params("pty", ReturnOn::Exit, 1_001);
        wait.max_bytes = 0;
        let error = without_pty.read(&ctx(), wait).await.unwrap_err();
        assert_eq!(
            error.kind(),
            ProtocolErrorKind::InvalidParams,
            "waitMs above the ceiling"
        );
    }
}

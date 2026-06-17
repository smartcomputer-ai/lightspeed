use std::{
    collections::BTreeMap,
    path::{Component, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use host_protocol::{
    data::process::{
        ProcessOutputChunk, ProcessOutputStream, ReadProcessParams, ReadProcessResponse,
        StartProcessParams, StartProcessResponse, TerminateProcessParams, TerminateProcessResponse,
        WriteProcessParams, WriteProcessResponse, WriteProcessStatus,
    },
    error::{HostError, HostErrorCode},
    shared::{ByteChunk, HostPath, ProcessId},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, Notify},
    time::Instant,
};

#[derive(Clone)]
pub struct ProcessManager {
    cwd: PathBuf,
    fs_root: PathBuf,
    processes: Arc<Mutex<BTreeMap<String, Arc<ProcessEntry>>>>,
}

struct ProcessEntry {
    state: Mutex<ProcessState>,
    notify: Notify,
}

struct ProcessState {
    child: Child,
    stdin: Option<ChildStdin>,
    chunks: Vec<ProcessOutputChunk>,
    next_seq: u64,
    exited: bool,
    exit_code: Option<i32>,
    failure: Option<String>,
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
        if params.tty {
            return Err(HostError::new(
                HostErrorCode::Unsupported,
                "PTY process execution is not supported by host-bridge",
            ));
        }
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

        let mut command = Command::new(&params.argv[0]);
        command
            .args(&params.argv[1..])
            .current_dir(&cwd)
            .envs(params.env.iter())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if params.stdin.is_some() || params.pipe_stdin {
            command.stdin(Stdio::piped());
        } else {
            command.stdin(Stdio::null());
        }

        let mut child = command.spawn().map_err(|error| {
            HostError::new(
                HostErrorCode::ProcessFailed,
                format!("spawn process {:?}: {error}", params.argv),
            )
        })?;
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
        let entry = Arc::new(ProcessEntry {
            state: Mutex::new(ProcessState {
                child,
                stdin,
                chunks: Vec::new(),
                next_seq: 0,
                exited: false,
                exit_code: None,
                failure: None,
            }),
            notify: Notify::new(),
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

        if let Some(stdout) = stdout {
            tokio::spawn(read_stream(
                entry.clone(),
                stdout,
                ProcessOutputStream::Stdout,
            ));
        }
        if let Some(stderr) = stderr {
            tokio::spawn(read_stream(
                entry.clone(),
                stderr,
                ProcessOutputStream::Stderr,
            ));
        }
        if let Some(timeout_ms) = params.timeout_ms {
            tokio::spawn(timeout_process(
                entry.clone(),
                Duration::from_millis(timeout_ms),
            ));
        }

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
            let response = {
                let mut state = entry.state.lock().await;
                update_exit_status(&mut state)?;
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
                if should_return { Some(response) } else { None }
            };
            if let Some(response) = response {
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
        update_exit_status(&mut state)?;
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
            stdin
                .write_all(chunk.as_slice())
                .await
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
        update_exit_status(&mut state)?;
        if state.exited {
            return Ok(TerminateProcessResponse { running: false });
        }
        state
            .child
            .kill()
            .await
            .map_err(|error| HostError::new(HostErrorCode::ProcessFailed, error.to_string()))?;
        state.exited = true;
        state.exit_code = None;
        state.failure = Some("process terminated".to_owned());
        entry.notify.notify_waiters();
        Ok(TerminateProcessResponse { running: true })
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
        let seq = state.next_seq;
        state.next_seq += 1;
        state.chunks.push(ProcessOutputChunk {
            seq,
            stream,
            chunk: ByteChunk::from(buffer[..read].to_vec()),
        });
        entry.notify.notify_waiters();
    }
}

async fn timeout_process(entry: Arc<ProcessEntry>, timeout: Duration) {
    tokio::time::sleep(timeout).await;
    let mut state = entry.state.lock().await;
    if state.exited {
        return;
    }
    let _ = state.child.kill().await;
    state.exited = true;
    state.exit_code = None;
    state.failure = Some("process timed out".to_owned());
    entry.notify.notify_waiters();
}

fn update_exit_status(state: &mut ProcessState) -> Result<(), HostError> {
    if state.exited {
        return Ok(());
    }
    match state.child.try_wait() {
        Ok(Some(status)) => {
            state.exited = true;
            state.exit_code = status.code();
            state.stdin.take();
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(error) => {
            state.failure = Some(error.to_string());
            Err(HostError::new(
                HostErrorCode::ProcessFailed,
                format!("poll process exit: {error}"),
            ))
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
    }
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
}

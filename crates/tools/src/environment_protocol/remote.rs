//! Remote environment adapters backed by `environment-client`.

use std::{
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use environment_client::{EnvironmentClientError, EnvironmentDataClient, JsonRpcTransport};
use environment_protocol::{
    data::{
        fs as remote_fs, jobs as remote_jobs,
        process::{
            self as remote_process, ProcessOutputStream, ProcessTermination, ReadProcessResponse,
            WriteProcessStatus,
        },
    },
    error::EnvironmentProtocolErrorCode,
    shared::{ByteChunk, EnvironmentCapabilities, EnvironmentPath, ProcessId},
};
use harness::storage::BlobStore;
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    environment::{
        EnvironmentToolContext,
        jobs::{JobError, JobExecResult, JobExecutor},
        process::{
            ContinueProcessRequest, LeftoverProcess, ProcessError, ProcessExecResult,
            ProcessExecutor, ProcessHandle, ProcessOutput, ProcessRequest, ProcessSignal,
            ProcessStatus, StreamOutput,
        },
    },
    fs::{
        CopyOptions, CreateDirectoryOptions, FileAccessPolicy, FileMetadata, FileSystem, FsError,
        FsGlobRequest, FsGlobResponse, FsPath, FsRangedRead, FsResult, FsSearchStop,
        FsTextSearchMatch, FsTextSearchRequest, FsTextSearchResponse, FsToolContext,
        ReadDirectoryEntry, RemoveOptions, ranged_read_from_full,
    },
};

static NEXT_PROCESS_EXECUTOR_ID: AtomicU64 = AtomicU64::new(1);
static PROCESS_ID_NONCE: LazyLock<String> = LazyLock::new(|| {
    let started_at_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}-{started_at_nanos}", std::process::id())
});

pub struct RemoteEnvironmentConnection<T> {
    client: Arc<AsyncMutex<EnvironmentDataClient<T>>>,
    capabilities: EnvironmentCapabilities,
    cwd: Option<FsPath>,
}

impl<T> Clone for RemoteEnvironmentConnection<T> {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            capabilities: self.capabilities.clone(),
            cwd: self.cwd.clone(),
        }
    }
}

#[async_trait]
impl<T: JsonRpcTransport + Send + 'static> crate::transfer::EnvironmentTransfer
    for RemoteEnvironmentConnection<T>
{
    async fn request(
        &self,
        request: environment_protocol::data::transfer_session::TransferRequest,
    ) -> crate::error::ToolResult<environment_protocol::data::transfer_session::TransferResponse>
    {
        if !self.capabilities.filesystem_transfer {
            return Err(crate::error::ToolError::UnsupportedCapability {
                message: "environment transfer unavailable".into(),
            });
        }
        self.client
            .lock()
            .await
            .transfer(&request)
            .await
            .map_err(|e| crate::error::ToolError::InvalidRequest {
                message: e.to_string(),
            })
    }
}

impl<T> RemoteEnvironmentConnection<T>
where
    T: JsonRpcTransport + Send + 'static,
{
    pub fn new(client: EnvironmentDataClient<T>, capabilities: EnvironmentCapabilities) -> Self {
        Self {
            client: Arc::new(AsyncMutex::new(client)),
            capabilities,
            cwd: None,
        }
    }

    pub fn with_cwd(mut self, cwd: FsPath) -> Self {
        self.cwd = Some(cwd);
        self
    }

    /// Gracefully shut down the shared data-plane transport after all tool
    /// adapters derived from this connection have finished using it.
    pub async fn close(&self) -> Result<(), EnvironmentClientError> {
        self.client.lock().await.close().await
    }

    pub fn file_system(&self) -> RemoteEnvironmentFileSystem<T> {
        RemoteEnvironmentFileSystem {
            client: self.client.clone(),
            access_policy: access_policy_for_capabilities(&self.capabilities),
            filesystem_search: self.capabilities.filesystem_search,
            filesystem_glob: self.capabilities.filesystem_glob,
            filesystem_ranged_read: self.capabilities.filesystem_ranged_read,
        }
    }

    pub fn process_executor(&self) -> Option<RemoteProcessExecutor<T>> {
        if !self.capabilities.process_start {
            return None;
        }
        let process_id_prefix = NEXT_PROCESS_EXECUTOR_ID.fetch_add(1, Ordering::Relaxed);
        Some(RemoteProcessExecutor {
            client: self.client.clone(),
            process_id_prefix,
            next_process_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn job_executor(&self) -> Option<RemoteJobExecutor<T>> {
        if !self.capabilities.job_start
            && !self.capabilities.job_list
            && !self.capabilities.job_read
            && !self.capabilities.job_cancel
        {
            return None;
        }
        Some(RemoteJobExecutor {
            client: self.client.clone(),
            capabilities: self.capabilities.clone(),
        })
    }

    pub fn into_contexts(
        self,
        blobs: Arc<dyn BlobStore>,
    ) -> (FsToolContext, EnvironmentToolContext) {
        let fs = Arc::new(self.file_system());
        let process = self
            .process_executor()
            .map(|executor| Arc::new(executor) as Arc<dyn ProcessExecutor>);
        let jobs = self
            .job_executor()
            .map(|executor| Arc::new(executor) as Arc<dyn JobExecutor>);
        let mut fs_ctx = FsToolContext::new(fs, blobs.clone());
        let mut env_ctx = EnvironmentToolContext::new(process, blobs);
        if self.capabilities.filesystem_transfer {
            env_ctx.transfer = Some(Arc::new(self.clone()));
        }
        if let Some(jobs) = jobs {
            env_ctx = env_ctx.with_jobs(jobs);
        }
        if let Some(cwd) = self.cwd {
            fs_ctx = fs_ctx.with_cwd(cwd.clone());
            env_ctx = env_ctx.with_process_cwd(cwd);
        }
        // Filesystem tools are exposed only when the daemon negotiated read
        // access; write access is enforced by the file system's access policy.
        if self.capabilities.filesystem_read {
            env_ctx = env_ctx.with_filesystem(fs_ctx.clone());
        }
        (fs_ctx, env_ctx)
    }
}

#[derive(Clone)]
pub struct RemoteEnvironmentFileSystem<T> {
    client: Arc<AsyncMutex<EnvironmentDataClient<T>>>,
    access_policy: FileAccessPolicy,
    filesystem_search: bool,
    filesystem_glob: bool,
    filesystem_ranged_read: bool,
}

#[async_trait]
impl<T> FileSystem for RemoteEnvironmentFileSystem<T>
where
    T: JsonRpcTransport + Send + 'static,
{
    fn access_policy(&self) -> FileAccessPolicy {
        self.access_policy.clone()
    }

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
        let response = self
            .client
            .lock()
            .await
            .read_file(&remote_fs::ReadFileParams {
                path: environment_path(path)?,
                offset: None,
                max_bytes: None,
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        Ok(response.data.into_inner())
    }

    async fn read_file_range(
        &self,
        path: &FsPath,
        offset: u64,
        max_bytes: Option<u64>,
    ) -> FsResult<FsRangedRead> {
        if !self.filesystem_ranged_read {
            let bytes = self.read_file(path).await?;
            return Ok(ranged_read_from_full(bytes, offset, max_bytes));
        }
        let response = self
            .client
            .lock()
            .await
            .read_file(&remote_fs::ReadFileParams {
                path: environment_path(path)?,
                offset: Some(offset),
                max_bytes,
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        let bytes = response.data.into_inner();
        let file_size = response
            .file_size
            .unwrap_or_else(|| offset.saturating_add(bytes.len() as u64));
        Ok(FsRangedRead {
            file_size,
            truncated: response.truncated,
            bytes,
        })
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
        self.client
            .lock()
            .await
            .write_file(&remote_fs::WriteFileParams {
                path: environment_path(path)?,
                data: ByteChunk::from(contents),
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        Ok(())
    }

    async fn create_directory(
        &self,
        path: &FsPath,
        options: CreateDirectoryOptions,
    ) -> FsResult<()> {
        self.client
            .lock()
            .await
            .create_directory(&remote_fs::CreateDirectoryParams {
                path: environment_path(path)?,
                recursive: Some(options.recursive),
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        Ok(())
    }

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata> {
        let response = self
            .client
            .lock()
            .await
            .get_metadata(&remote_fs::GetMetadataParams {
                path: environment_path(path)?,
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        Ok(FileMetadata {
            is_directory: response.is_directory,
            is_file: response.is_file,
            is_symlink: response.is_symlink,
            created_at_ms: response.created_at_ms,
            modified_at_ms: response.modified_at_ms,
        })
    }

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
        let response = self
            .client
            .lock()
            .await
            .read_directory(&remote_fs::ReadDirectoryParams {
                path: environment_path(path)?,
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        Ok(response
            .entries
            .into_iter()
            .map(|entry| ReadDirectoryEntry {
                file_name: entry.file_name,
                is_directory: entry.is_directory,
                is_file: entry.is_file,
            })
            .collect())
    }

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()> {
        self.client
            .lock()
            .await
            .remove(&remote_fs::RemoveParams {
                path: environment_path(path)?,
                recursive: Some(options.recursive),
                force: Some(options.force),
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        Ok(())
    }

    async fn copy(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()> {
        self.client
            .lock()
            .await
            .copy(&remote_fs::CopyParams {
                source_path: environment_path(source_path)?,
                destination_path: environment_path(destination_path)?,
                recursive: options.recursive,
            })
            .await
            .map_err(|error| map_fs_error(error, destination_path))?;
        Ok(())
    }

    async fn search_text(
        &self,
        request: &FsTextSearchRequest,
    ) -> FsResult<Option<FsTextSearchResponse>> {
        if !self.filesystem_search {
            return Ok(None);
        }
        let params = remote_fs::SearchTextParams {
            root: environment_path(&request.root)?,
            pattern: request.pattern.clone(),
            include: request.include.clone(),
            case_sensitive: request.case_sensitive,
            max_depth: request
                .max_depth
                .map(|depth| u32::try_from(depth).unwrap_or(u32::MAX)),
            limits: remote_fs::SearchTextLimits {
                max_matches: request.limits.max_matches,
                max_files: request.limits.max_files,
                max_bytes: request.limits.max_bytes,
                max_duration_ms: request.limits.max_duration_ms,
            },
        };
        let response = self
            .client
            .lock()
            .await
            .search_text(&params)
            .await
            .map_err(|error| map_fs_error(error, &request.root))?;
        let matches = response
            .matches
            .into_iter()
            .map(|item| {
                Ok(FsTextSearchMatch {
                    path: FsPath::new(item.path.as_str()).map_err(FsError::from)?,
                    line_number: item.line_number,
                    line: item.line,
                })
            })
            .collect::<FsResult<Vec<_>>>()?;
        Ok(Some(FsTextSearchResponse {
            matches,
            files_searched: response.files_searched,
            bytes_searched: response.bytes_searched,
            stopped: response.stopped.map(|stop| match stop {
                remote_fs::SearchTextStop::MatchLimit => FsSearchStop::MatchLimit,
                remote_fs::SearchTextStop::FileLimit => FsSearchStop::FileLimit,
                remote_fs::SearchTextStop::ByteLimit => FsSearchStop::ByteLimit,
                remote_fs::SearchTextStop::TimeLimit => FsSearchStop::TimeLimit,
            }),
        }))
    }

    async fn glob_files(&self, request: &FsGlobRequest) -> FsResult<Option<FsGlobResponse>> {
        if !self.filesystem_glob {
            return Ok(None);
        }
        let params = remote_fs::GlobFilesParams {
            root: environment_path(&request.root)?,
            pattern: request.pattern.clone(),
            max_depth: request
                .max_depth
                .map(|depth| u32::try_from(depth).unwrap_or(u32::MAX)),
            limits: remote_fs::GlobFilesLimits {
                max_matches: request.limits.max_matches,
                max_entries: request.limits.max_entries,
                max_duration_ms: request.limits.max_duration_ms,
            },
        };
        let response = self
            .client
            .lock()
            .await
            .glob_files(&params)
            .await
            .map_err(|error| map_fs_error(error, &request.root))?;
        let matches = response
            .matches
            .into_iter()
            .map(|path| FsPath::new(path.as_str()).map_err(FsError::from))
            .collect::<FsResult<Vec<_>>>()?;
        Ok(Some(FsGlobResponse {
            matches,
            entries_visited: response.entries_visited,
            stopped: response.stopped.map(|stop| match stop {
                remote_fs::GlobFilesStop::MatchLimit => FsSearchStop::MatchLimit,
                remote_fs::GlobFilesStop::EntryLimit => FsSearchStop::FileLimit,
                remote_fs::GlobFilesStop::TimeLimit => FsSearchStop::TimeLimit,
            }),
        }))
    }
}

/// Process executor over the data plane. It keeps no per-handle state: the
/// daemon owns the read cursor, so a handle continued from a later batch or
/// connection reads only what was not delivered yet.
#[derive(Clone)]
pub struct RemoteProcessExecutor<T> {
    client: Arc<AsyncMutex<EnvironmentDataClient<T>>>,
    process_id_prefix: u64,
    next_process_id: Arc<AtomicU64>,
}

#[async_trait]
impl<T> ProcessExecutor for RemoteProcessExecutor<T>
where
    T: JsonRpcTransport + Send + 'static,
{
    async fn run_process(&self, request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
        let process_id = self.next_process_id();
        let response = {
            let mut client = self.client.lock().await;
            client
                .start_process(&remote_process::StartProcessParams {
                    process_id: process_id.clone(),
                    argv: request.argv,
                    cwd: request
                        .cwd
                        .as_ref()
                        .map(process_environment_path)
                        .transpose()?,
                    env: request.env,
                    secret_env: request.secret_env,
                    stdin: request.stdin.map(ByteChunk::from),
                    timeout_ms: request.timeout_ms,
                    tty: request.tty,
                })
                .await
                .map_err(map_process_error)?;
            client
                .read_process(&remote_process::ReadProcessParams {
                    process_id: process_id.clone(),
                    after_seq: None,
                    max_bytes: request.max_output_bytes.map(|value| value as usize),
                    wait_ms: request.yield_ms,
                })
                .await
                .map_err(map_process_error)?
        };
        Ok(output_from_read(process_id, response))
    }

    async fn continue_process(
        &self,
        request: ContinueProcessRequest,
    ) -> ProcessExecResult<ProcessOutput> {
        let handle = request.handle;
        let process_id = ProcessId::new(handle.as_str().to_owned());
        let input = request.input.filter(|input| !input.is_empty());
        let response = {
            let mut client = self.client.lock().await;
            if let Some(signal) = request.signal {
                client
                    .terminate_process(&remote_process::TerminateProcessParams {
                        process_id: process_id.clone(),
                        signal: match signal {
                            ProcessSignal::Interrupt => remote_process::ProcessSignal::Interrupt,
                            ProcessSignal::Kill => remote_process::ProcessSignal::Kill,
                        },
                    })
                    .await
                    .map_err(map_process_error)?;
            } else if input.is_some() || request.close_stdin {
                let write = client
                    .write_process(&remote_process::WriteProcessParams {
                        process_id: process_id.clone(),
                        chunk: input.map(ByteChunk::from),
                        close_stdin: request.close_stdin,
                    })
                    .await
                    .map_err(map_process_error)?;
                match write.status {
                    WriteProcessStatus::Accepted | WriteProcessStatus::Starting => {}
                    WriteProcessStatus::UnknownProcess => {
                        return Err(ProcessError::UnknownHandle { handle });
                    }
                    WriteProcessStatus::StdinClosed => {
                        return Err(ProcessError::StdinClosed { handle });
                    }
                }
            }
            client
                .read_process(&remote_process::ReadProcessParams {
                    process_id: process_id.clone(),
                    after_seq: None,
                    max_bytes: request.max_output_bytes.map(|value| value as usize),
                    wait_ms: request.wait_ms,
                })
                .await
                .map_err(|error| match error {
                    EnvironmentClientError::Protocol(error)
                        if error.code == EnvironmentProtocolErrorCode::NotFound =>
                    {
                        ProcessError::UnknownHandle {
                            handle: handle.clone(),
                        }
                    }
                    other => map_process_error(other),
                })?
        };
        Ok(output_from_read(process_id, response))
    }
}

impl<T> RemoteProcessExecutor<T> {
    fn next_process_id(&self) -> ProcessId {
        let id = self.next_process_id.fetch_add(1, Ordering::Relaxed);
        ProcessId::new(format!(
            "proc-{}-{}-{id}",
            PROCESS_ID_NONCE.as_str(),
            self.process_id_prefix
        ))
    }
}

fn output_from_read(process_id: ProcessId, response: ReadProcessResponse) -> ProcessOutput {
    let (stdout, stderr) = split_output(response.chunks, response.omitted_bytes);
    let status = match response.termination {
        Some(ProcessTermination::TimedOut) => ProcessStatus::TimedOut,
        Some(ProcessTermination::Killed) => ProcessStatus::Killed,
        None if response.failure.is_some() => ProcessStatus::Failed,
        None if response.exited => match response.exit_code {
            Some(0) => ProcessStatus::Succeeded,
            _ => ProcessStatus::Failed,
        },
        None => ProcessStatus::Running,
    };
    let handle = if response.closed || response.exited {
        None
    } else {
        Some(ProcessHandle::new(process_id.0))
    };
    ProcessOutput {
        status,
        handle,
        pid: response.pid,
        exit_code: response.exit_code,
        failure: response.failure,
        stdout,
        stderr,
        omitted_bytes: response.omitted_bytes,
        leftover_processes: response
            .leftover_processes
            .into_iter()
            .map(|member| LeftoverProcess {
                pid: member.pid,
                command: member.command,
            })
            .collect(),
    }
}

#[derive(Clone)]
pub struct RemoteJobExecutor<T> {
    client: Arc<AsyncMutex<EnvironmentDataClient<T>>>,
    capabilities: EnvironmentCapabilities,
}

#[async_trait]
impl<T> JobExecutor for RemoteJobExecutor<T>
where
    T: JsonRpcTransport + Send + 'static,
{
    async fn start_jobs(
        &self,
        request: remote_jobs::StartJobsParams,
    ) -> JobExecResult<remote_jobs::StartJobsResponse> {
        if !self.capabilities.job_start {
            return Err(JobError::Unsupported {
                message: "provider target does not support job/start".to_owned(),
            });
        }
        self.client
            .lock()
            .await
            .start_jobs(&request)
            .await
            .map_err(map_job_error)
    }

    async fn read_jobs(
        &self,
        request: remote_jobs::ReadJobsParams,
    ) -> JobExecResult<remote_jobs::ReadJobsResponse> {
        if !self.capabilities.job_read {
            return Err(JobError::Unsupported {
                message: "provider target does not support job/read".to_owned(),
            });
        }
        self.client
            .lock()
            .await
            .read_jobs(&request)
            .await
            .map_err(map_job_error)
    }

    async fn cancel_jobs(
        &self,
        request: remote_jobs::CancelJobsParams,
    ) -> JobExecResult<remote_jobs::CancelJobsResponse> {
        if !self.capabilities.job_cancel {
            return Err(JobError::Unsupported {
                message: "provider target does not support job/cancel".to_owned(),
            });
        }
        self.client
            .lock()
            .await
            .cancel_jobs(&request)
            .await
            .map_err(map_job_error)
    }
}

fn access_policy_for_capabilities(capabilities: &EnvironmentCapabilities) -> FileAccessPolicy {
    if capabilities.filesystem_write {
        FileAccessPolicy::FullReadWrite
    } else {
        FileAccessPolicy::FullReadOnly
    }
}

fn environment_path(path: &FsPath) -> Result<EnvironmentPath, FsError> {
    EnvironmentPath::new(path.as_str()).map_err(|error| FsError::InvalidInput {
        message: error.to_string(),
    })
}

fn process_environment_path(path: &FsPath) -> Result<EnvironmentPath, ProcessError> {
    EnvironmentPath::new(path.as_str()).map_err(|error| ProcessError::InvalidRequest {
        message: error.to_string(),
    })
}

fn map_fs_error(error: EnvironmentClientError, path: &FsPath) -> FsError {
    match error {
        EnvironmentClientError::Protocol(error) => match error.code {
            EnvironmentProtocolErrorCode::NotFound => FsError::NotFound { path: path.clone() },
            EnvironmentProtocolErrorCode::Conflict => FsError::AlreadyExists { path: path.clone() },
            EnvironmentProtocolErrorCode::Unauthorized
            | EnvironmentProtocolErrorCode::Forbidden => {
                FsError::PermissionDenied { path: path.clone() }
            }
            EnvironmentProtocolErrorCode::Unsupported
            | EnvironmentProtocolErrorCode::CapabilityUnavailable => FsError::Unsupported {
                message: error.message,
            },
            EnvironmentProtocolErrorCode::InvalidRequest => FsError::InvalidInput {
                message: error.message,
            },
            _ => FsError::Failed {
                message: error.message,
            },
        },
        other => FsError::Failed {
            message: other.to_string(),
        },
    }
}

fn map_process_error(error: EnvironmentClientError) -> ProcessError {
    match error {
        EnvironmentClientError::Protocol(error) => match error.code {
            EnvironmentProtocolErrorCode::Unsupported
            | EnvironmentProtocolErrorCode::CapabilityUnavailable => ProcessError::Unsupported {
                message: error.message,
            },
            EnvironmentProtocolErrorCode::InvalidRequest
            | EnvironmentProtocolErrorCode::NotFound => ProcessError::InvalidRequest {
                message: error.message,
            },
            _ => ProcessError::Failed {
                message: error.message,
            },
        },
        other => ProcessError::Failed {
            message: other.to_string(),
        },
    }
}

fn map_job_error(error: EnvironmentClientError) -> JobError {
    match error {
        EnvironmentClientError::Protocol(error) => match error.code {
            EnvironmentProtocolErrorCode::Unsupported
            | EnvironmentProtocolErrorCode::CapabilityUnavailable => JobError::Unsupported {
                message: error.message,
            },
            EnvironmentProtocolErrorCode::InvalidRequest
            | EnvironmentProtocolErrorCode::NotFound
            | EnvironmentProtocolErrorCode::Conflict => JobError::InvalidRequest {
                message: error.message,
            },
            _ => JobError::Failed {
                message: error.message,
            },
        },
        other => JobError::Failed {
            message: other.to_string(),
        },
    }
}

/// Sorts chunks into their streams. The omission marker goes where the
/// daemon dropped the middle: at the first sequence gap, or at the start of
/// the output when the gap precedes everything returned.
fn split_output(
    chunks: Vec<remote_process::ProcessOutputChunk>,
    omitted_bytes: u64,
) -> (StreamOutput, StreamOutput) {
    let mut stdout = StreamOutput::default();
    let mut stderr = StreamOutput::default();
    let mut omission_placed = omitted_bytes == 0;
    let gap_index = chunks
        .windows(2)
        .position(|pair| pair[1].seq != pair[0].seq + 1)
        .map(|index| index + 1)
        .unwrap_or(0);
    for (index, chunk) in chunks.into_iter().enumerate() {
        let stream = match chunk.stream {
            ProcessOutputStream::Stdout | ProcessOutputStream::Pty => &mut stdout,
            ProcessOutputStream::Stderr => &mut stderr,
        };
        if !omission_placed && index == gap_index {
            stream.omitted_at = Some(stream.bytes.len());
            omission_placed = true;
        }
        stream.bytes.extend(chunk.chunk.into_inner());
    }
    if !omission_placed {
        stdout.omitted_at = Some(stdout.bytes.len());
    }
    (stdout, stderr)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{Arc, Mutex},
    };

    use environment_client::EnvironmentClientResult;
    use harness::storage::InMemoryBlobStore;
    use serde_json::{Value, json};

    use super::*;
    use crate::fs::tools::{ReadFileArgs, WriteFileArgs, invoke_read_file, invoke_write_file};

    struct MockTransport {
        sent: Arc<Mutex<Vec<Value>>>,
        recv: Arc<Mutex<VecDeque<Value>>>,
    }

    impl MockTransport {
        fn new(messages: impl IntoIterator<Item = Value>) -> (Self, Arc<Mutex<Vec<Value>>>) {
            let sent = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    sent: sent.clone(),
                    recv: Arc::new(Mutex::new(messages.into_iter().collect())),
                },
                sent,
            )
        }
    }

    #[async_trait]
    impl JsonRpcTransport for MockTransport {
        async fn send(&mut self, message: Value) -> EnvironmentClientResult<()> {
            self.sent.lock().expect("sent lock").push(message);
            Ok(())
        }

        async fn recv(&mut self) -> EnvironmentClientResult<Option<Value>> {
            Ok(self.recv.lock().expect("recv lock").pop_front())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_file_system_maps_file_operations_to_data_plane_methods() {
        let (transport, sent) = MockTransport::new([
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "data": "aGk="
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {}
            }),
        ]);
        let connection = RemoteEnvironmentConnection::new(
            EnvironmentDataClient::new(transport),
            EnvironmentCapabilities::filesystem(true, true),
        );
        let fs = connection.file_system();

        let contents = fs
            .read_file(&FsPath::new("/workspace/file.txt").expect("path"))
            .await
            .expect("read file");
        fs.write_file(
            &FsPath::new("/workspace/file.txt").expect("path"),
            b"updated".to_vec(),
        )
        .await
        .expect("write file");

        assert_eq!(contents, b"hi");
        let sent = sent.lock().expect("sent lock");
        assert_eq!(sent[0]["method"], "fs/readFile");
        assert_eq!(sent[0]["params"]["path"], "/workspace/file.txt");
        assert_eq!(sent[1]["method"], "fs/writeFile");
        assert_eq!(sent[1]["params"]["data"], "dXBkYXRlZA==");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_process_executor_runs_and_continues_through_the_daemon_cursor() {
        let (transport, sent) = MockTransport::new([
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "processId": "proc-1"
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "chunks": [
                        {
                            "chunk": "b2sK",
                            "seq": 1,
                            "stream": "stdout"
                        },
                        {
                            "chunk": "d2Fybgo=",
                            "seq": 2,
                            "stream": "stderr"
                        }
                    ],
                    "closed": false,
                    "exited": false,
                    "nextSeq": 3,
                    "pid": 4242
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {
                    "status": "accepted"
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "result": {
                    "chunks": [
                        {
                            "chunk": "ZG9uZQo=",
                            "seq": 3,
                            "stream": "stdout"
                        }
                    ],
                    "closed": true,
                    "exited": true,
                    "exitCode": 0,
                    "nextSeq": 4,
                    "pid": 4242,
                    "leftoverProcesses": [{ "pid": 4243, "command": "sleep 100" }]
                }
            }),
        ]);
        let connection = RemoteEnvironmentConnection::new(
            EnvironmentDataClient::new(transport),
            EnvironmentCapabilities::filesystem(true, true).with_process(),
        );
        let process = connection.process_executor().expect("process executor");

        let output = process
            .run_process(ProcessRequest {
                argv: vec!["sh".to_owned(), "-lc".to_owned(), "cat".to_owned()],
                cwd: Some(FsPath::new("/workspace").expect("cwd")),
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                stdin: None,
                tty: true,
                timeout_ms: None,
                yield_ms: Some(10),
                max_output_bytes: Some(1024),
            })
            .await
            .expect("run process");

        let process_id = sent.lock().expect("sent lock")[0]["params"]["processId"]
            .as_str()
            .expect("process id")
            .to_owned();
        assert_eq!(output.status, ProcessStatus::Running);
        assert_eq!(output.handle, Some(ProcessHandle::new(process_id.clone())));
        assert_eq!(output.pid, Some(4242));
        assert_eq!(output.stdout.bytes, b"ok\n");
        assert_eq!(output.stderr.bytes, b"warn\n");

        let output = process
            .continue_process(ContinueProcessRequest {
                handle: ProcessHandle::new(process_id.clone()),
                input: Some(b"input\n".to_vec()),
                close_stdin: true,
                signal: None,
                wait_ms: Some(10),
                max_output_bytes: Some(1024),
            })
            .await
            .expect("continue");

        assert_eq!(output.status, ProcessStatus::Succeeded);
        assert_eq!(output.handle, None);
        assert_eq!(output.stdout.bytes, b"done\n");
        assert_eq!(
            output.leftover_processes,
            vec![LeftoverProcess {
                pid: 4243,
                command: "sleep 100".to_owned()
            }]
        );

        let sent = sent.lock().expect("sent lock");
        assert_eq!(sent[0]["method"], "process/start");
        assert_eq!(sent[0]["params"]["processId"], process_id);
        assert_eq!(sent[0]["params"]["tty"], true);
        assert!(sent[0]["params"].get("pipeStdin").is_none());
        assert!(sent[0]["params"].get("timeoutMs").is_none());
        assert_eq!(sent[1]["method"], "process/read");
        assert!(sent[1]["params"].get("afterSeq").is_none());
        assert_eq!(sent[1]["params"]["waitMs"], 10);
        assert_eq!(sent[2]["method"], "process/write");
        assert_eq!(sent[2]["params"]["closeStdin"], true);
        assert_eq!(sent[3]["method"], "process/read");
        assert!(
            sent[3]["params"].get("afterSeq").is_none(),
            "the daemon owns the cursor; the runtime never re-sends a sequence"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_process_executor_maps_signals_and_stdin_refusals() {
        let (transport, sent) = MockTransport::new([
            json!({ "jsonrpc": "2.0", "id": 1, "result": { "running": true } }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "chunks": [],
                    "closed": true,
                    "exited": true,
                    "nextSeq": 0,
                    "termination": "killed"
                }
            }),
            json!({ "jsonrpc": "2.0", "id": 3, "result": { "status": "stdinClosed" } }),
            json!({ "jsonrpc": "2.0", "id": 4, "result": { "status": "unknownProcess" } }),
        ]);
        let process = RemoteEnvironmentConnection::new(
            EnvironmentDataClient::new(transport),
            EnvironmentCapabilities::filesystem(true, true).with_process(),
        )
        .process_executor()
        .expect("process executor");

        let killed = process
            .continue_process(ContinueProcessRequest {
                handle: ProcessHandle::new("proc-9"),
                input: None,
                close_stdin: false,
                signal: Some(ProcessSignal::Kill),
                wait_ms: Some(5),
                max_output_bytes: None,
            })
            .await
            .expect("kill");
        assert_eq!(killed.status, ProcessStatus::Killed);
        assert_eq!(killed.handle, None);

        let refused = process
            .continue_process(ContinueProcessRequest {
                handle: ProcessHandle::new("proc-9"),
                input: Some(b"x".to_vec()),
                close_stdin: false,
                signal: None,
                wait_ms: Some(5),
                max_output_bytes: None,
            })
            .await
            .expect_err("stdin closed");
        assert!(matches!(refused, ProcessError::StdinClosed { .. }));
        assert!(refused.to_string().contains("tty: true"));

        let unknown = process
            .continue_process(ContinueProcessRequest {
                handle: ProcessHandle::new("proc-gone"),
                input: Some(b"x".to_vec()),
                close_stdin: false,
                signal: None,
                wait_ms: Some(5),
                max_output_bytes: None,
            })
            .await
            .expect_err("unknown");
        assert!(matches!(unknown, ProcessError::UnknownHandle { .. }));

        let sent = sent.lock().expect("sent lock");
        assert_eq!(sent[0]["method"], "process/terminate");
        assert_eq!(sent[0]["params"]["signal"], "kill");
        assert_eq!(sent[1]["method"], "process/read");
        assert_eq!(sent[2]["method"], "process/write");
        assert_eq!(sent[3]["method"], "process/write");
    }

    #[test]
    fn split_output_places_the_omission_marker_at_the_sequence_gap() {
        let chunk = |seq: u64, stream: ProcessOutputStream, text: &str| {
            remote_process::ProcessOutputChunk {
                seq,
                stream,
                chunk: ByteChunk::from(text.as_bytes()),
            }
        };
        let (stdout, stderr) = split_output(
            vec![
                chunk(0, ProcessOutputStream::Stdout, "head"),
                chunk(1, ProcessOutputStream::Stderr, "warn"),
                chunk(40, ProcessOutputStream::Stdout, "tail"),
            ],
            500,
        );
        assert_eq!(stdout.bytes, b"headtail");
        assert_eq!(stdout.omitted_at, Some(4));
        assert_eq!(stderr.omitted_at, None);

        let (stdout, _) = split_output(vec![chunk(40, ProcessOutputStream::Stdout, "tail")], 500);
        assert_eq!(stdout.omitted_at, Some(0), "gap before everything returned");

        let (stdout, _) = split_output(vec![chunk(0, ProcessOutputStream::Stdout, "all")], 0);
        assert_eq!(stdout.omitted_at, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_process_executors_use_distinct_process_id_prefixes() {
        let (transport_a, sent_a) = MockTransport::new([
            json!({ "jsonrpc": "2.0", "id": 1, "result": { "processId": "unused" } }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": { "chunks": [], "closed": true, "exited": true, "exitCode": 0, "nextSeq": 0 }
            }),
        ]);
        let (transport_b, sent_b) = MockTransport::new([
            json!({ "jsonrpc": "2.0", "id": 1, "result": { "processId": "unused" } }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": { "chunks": [], "closed": true, "exited": true, "exitCode": 0, "nextSeq": 0 }
            }),
        ]);
        let process_a = RemoteEnvironmentConnection::new(
            EnvironmentDataClient::new(transport_a),
            EnvironmentCapabilities::filesystem(true, true).with_process(),
        )
        .process_executor()
        .expect("process executor a");
        let process_b = RemoteEnvironmentConnection::new(
            EnvironmentDataClient::new(transport_b),
            EnvironmentCapabilities::filesystem(true, true).with_process(),
        )
        .process_executor()
        .expect("process executor b");

        for process in [&process_a, &process_b] {
            process
                .run_process(ProcessRequest {
                    argv: vec!["true".to_owned()],
                    cwd: None,
                    env: BTreeMap::new(),
                    secret_env: BTreeMap::new(),
                    stdin: None,
                    tty: false,
                    timeout_ms: Some(1_000),
                    yield_ms: Some(1),
                    max_output_bytes: Some(128),
                })
                .await
                .expect("run process");
        }

        let process_id_a = sent_a.lock().expect("sent a lock")[0]["params"]["processId"]
            .as_str()
            .expect("process id a")
            .to_owned();
        let process_id_b = sent_b.lock().expect("sent b lock")[0]["params"]["processId"]
            .as_str()
            .expect("process id b")
            .to_owned();

        assert_ne!(process_id_a, process_id_b);
        assert!(process_id_a.starts_with("proc-"));
        assert!(process_id_b.starts_with("proc-"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_connection_builds_tool_contexts() {
        let (transport, _sent) = MockTransport::new([]);
        let connection = RemoteEnvironmentConnection::new(
            EnvironmentDataClient::new(transport),
            EnvironmentCapabilities::filesystem(true, false),
        )
        .with_cwd(FsPath::new("/workspace").expect("cwd"));

        let (fs_ctx, env_ctx) = connection.into_contexts(Arc::new(InMemoryBlobStore::new()));

        assert_eq!(fs_ctx.fs_cwd, Some(FsPath::new("/workspace").expect("cwd")));
        assert_eq!(
            env_ctx.process_cwd,
            Some(FsPath::new("/workspace").expect("cwd"))
        );
        assert!(env_ctx.process.is_none());
        assert_eq!(fs_ctx.fs.access_policy(), FileAccessPolicy::FullReadOnly);
        let filesystem = env_ctx
            .filesystem
            .as_ref()
            .expect("read-only capability keeps a filesystem context");
        assert_eq!(
            filesystem.fs.access_policy(),
            FileAccessPolicy::FullReadOnly
        );
        assert_eq!(
            filesystem.fs_cwd,
            Some(FsPath::new("/workspace").expect("cwd"))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_connection_context_filesystem_follows_negotiated_capabilities() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());

        let (transport, _sent) = MockTransport::new([]);
        let (_, read_write) = RemoteEnvironmentConnection::new(
            EnvironmentDataClient::new(transport),
            EnvironmentCapabilities::filesystem(true, true),
        )
        .into_contexts(blobs.clone());
        assert_eq!(
            read_write
                .filesystem
                .as_ref()
                .expect("read/write filesystem")
                .fs
                .access_policy(),
            FileAccessPolicy::FullReadWrite
        );

        let (transport, _sent) = MockTransport::new([]);
        let (_, none) = RemoteEnvironmentConnection::new(
            EnvironmentDataClient::new(transport),
            EnvironmentCapabilities::filesystem(false, false),
        )
        .into_contexts(blobs);
        assert!(
            none.filesystem.is_none(),
            "no filesystem tools without negotiated read access"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn existing_file_tools_work_through_remote_context() {
        let (transport, sent) = MockTransport::new([
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {}
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {}
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {
                    "data": "aGVsbG8K"
                }
            }),
        ]);
        let connection = RemoteEnvironmentConnection::new(
            EnvironmentDataClient::new(transport),
            EnvironmentCapabilities::filesystem(true, true),
        )
        .with_cwd(FsPath::new("/workspace").expect("cwd"));
        let (fs_ctx, _env_ctx) = connection.into_contexts(Arc::new(InMemoryBlobStore::new()));

        let write = invoke_write_file(
            &fs_ctx,
            WriteFileArgs {
                path: FsPath::new("nested/file.txt").expect("path"),
                content: Some("hello\n".to_owned()),
                content_ref: None,
            },
        )
        .await
        .expect("write file");
        let read = invoke_read_file(
            &fs_ctx,
            ReadFileArgs {
                path: FsPath::new("nested/file.txt").expect("path"),
                offset: None,
                limit: None,
            },
        )
        .await
        .expect("read file");

        assert_eq!(
            write.resolved_path,
            FsPath::new("/workspace/nested/file.txt").expect("path")
        );
        assert_eq!(read.text, "hello");

        let sent = sent.lock().expect("sent lock");
        assert_eq!(sent[0]["method"], "fs/createDirectory");
        assert_eq!(sent[0]["params"]["path"], "/workspace/nested");
        assert_eq!(sent[1]["method"], "fs/writeFile");
        assert_eq!(sent[2]["method"], "fs/readFile");
        assert_eq!(sent[2]["params"]["path"], "/workspace/nested/file.txt");
    }
}

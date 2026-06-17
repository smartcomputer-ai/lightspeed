//! Remote host adapters backed by `host-client`.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use engine::storage::BlobStore;
use host_client::{HostClientError, HostDataClient, JsonRpcTransport};
use host_protocol::{
    data::{
        fs as remote_fs,
        process::{
            self as remote_process, ProcessOutputStream, ReadProcessResponse, WriteProcessStatus,
        },
    },
    error::HostErrorCode,
    shared::{ByteChunk, HostCapabilities, HostPath, ProcessId},
};
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    environment::{
        EnvironmentToolContext,
        process::{
            ProcessError, ProcessExecResult, ProcessExecutor, ProcessHandle, ProcessOutput,
            ProcessRequest, ProcessStatus, StreamOutput, WriteProcessStdinRequest,
        },
    },
    fs::{
        CopyOptions, CreateDirectoryOptions, FileAccessPolicy, FileMetadata, FileSystem, FsError,
        FsPath, FsResult, FsToolContext, ReadDirectoryEntry, RemoveOptions,
    },
};

pub struct RemoteHostConnection<T> {
    client: Arc<AsyncMutex<HostDataClient<T>>>,
    capabilities: HostCapabilities,
    cwd: Option<FsPath>,
}

impl<T> Clone for RemoteHostConnection<T> {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            capabilities: self.capabilities.clone(),
            cwd: self.cwd.clone(),
        }
    }
}

impl<T> RemoteHostConnection<T>
where
    T: JsonRpcTransport + Send + 'static,
{
    pub fn new(client: HostDataClient<T>, capabilities: HostCapabilities) -> Self {
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

    pub fn file_system(&self) -> RemoteHostFileSystem<T> {
        RemoteHostFileSystem {
            client: self.client.clone(),
            access_policy: access_policy_for_capabilities(&self.capabilities),
        }
    }

    pub fn process_executor(&self) -> Option<RemoteProcessExecutor<T>> {
        if !self.capabilities.process_start {
            return None;
        }
        Some(RemoteProcessExecutor {
            client: self.client.clone(),
            next_process_id: Arc::new(AtomicU64::new(1)),
            next_seq_by_process: Arc::new(StdMutex::new(BTreeMap::new())),
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
        let mut fs_ctx = FsToolContext::new(fs, blobs.clone());
        let mut env_ctx = EnvironmentToolContext::new(process, blobs);
        if let Some(cwd) = self.cwd {
            fs_ctx = fs_ctx.with_cwd(cwd.clone());
            env_ctx = env_ctx.with_process_cwd(cwd);
        }
        (fs_ctx, env_ctx)
    }
}

#[derive(Clone)]
pub struct RemoteHostFileSystem<T> {
    client: Arc<AsyncMutex<HostDataClient<T>>>,
    access_policy: FileAccessPolicy,
}

#[async_trait]
impl<T> FileSystem for RemoteHostFileSystem<T>
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
                path: host_path(path)?,
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        Ok(response.data.into_inner())
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
        self.client
            .lock()
            .await
            .write_file(&remote_fs::WriteFileParams {
                path: host_path(path)?,
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
                path: host_path(path)?,
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
                path: host_path(path)?,
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
                path: host_path(path)?,
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
                path: host_path(path)?,
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
                source_path: host_path(source_path)?,
                destination_path: host_path(destination_path)?,
                recursive: options.recursive,
            })
            .await
            .map_err(|error| map_fs_error(error, destination_path))?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct RemoteProcessExecutor<T> {
    client: Arc<AsyncMutex<HostDataClient<T>>>,
    next_process_id: Arc<AtomicU64>,
    next_seq_by_process: Arc<StdMutex<BTreeMap<String, u64>>>,
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
                    cwd: request.cwd.as_ref().map(process_host_path).transpose()?,
                    env: request.env,
                    stdin: request.stdin.map(ByteChunk::from),
                    timeout_ms: request.timeout_ms,
                    tty: false,
                    pipe_stdin: request.yield_time_ms.is_some(),
                })
                .await
                .map_err(map_process_error)?;
            client
                .read_process(&remote_process::ReadProcessParams {
                    process_id: process_id.clone(),
                    after_seq: None,
                    max_bytes: request.max_output_bytes.map(|value| value as usize),
                    wait_ms: request.yield_time_ms,
                })
                .await
                .map_err(map_process_error)?
        };
        Ok(self.output_from_read(process_id, response, request.max_output_bytes))
    }

    async fn write_stdin(
        &self,
        request: WriteProcessStdinRequest,
    ) -> ProcessExecResult<ProcessOutput> {
        let process_id = ProcessId::new(request.handle.as_str().to_owned());
        let after_seq = self.next_seq_for(request.handle.as_str())?;
        let response = {
            let mut client = self.client.lock().await;
            let write = client
                .write_process(&remote_process::WriteProcessParams {
                    process_id: process_id.clone(),
                    chunk: Some(ByteChunk::from(request.input)),
                    close_stdin: request.close_stdin,
                })
                .await
                .map_err(map_process_error)?;
            match write.status {
                WriteProcessStatus::Accepted | WriteProcessStatus::Starting => {}
                WriteProcessStatus::UnknownProcess => {
                    return Err(ProcessError::InvalidRequest {
                        message: format!("unknown process handle {}", request.handle),
                    });
                }
                WriteProcessStatus::StdinClosed => {
                    return Err(ProcessError::InvalidRequest {
                        message: format!("stdin is closed for process {}", request.handle),
                    });
                }
            }
            client
                .read_process(&remote_process::ReadProcessParams {
                    process_id: process_id.clone(),
                    after_seq,
                    max_bytes: request.max_output_bytes.map(|value| value as usize),
                    wait_ms: request.yield_time_ms,
                })
                .await
                .map_err(map_process_error)?
        };
        Ok(self.output_from_read(process_id, response, request.max_output_bytes))
    }
}

impl<T> RemoteProcessExecutor<T> {
    fn next_process_id(&self) -> ProcessId {
        let id = self.next_process_id.fetch_add(1, Ordering::Relaxed);
        ProcessId::new(format!("proc-{id}"))
    }

    fn next_seq_for(&self, process_id: &str) -> ProcessExecResult<Option<u64>> {
        let seqs = self
            .next_seq_by_process
            .lock()
            .map_err(|error| ProcessError::Failed {
                message: format!("remote process sequence lock poisoned: {error}"),
            })?;
        Ok(seqs.get(process_id).copied())
    }

    fn output_from_read(
        &self,
        process_id: ProcessId,
        response: ReadProcessResponse,
        max_output_bytes: Option<u64>,
    ) -> ProcessOutput {
        self.record_next_seq(&process_id, &response);
        let (stdout, stderr) = split_output(response.chunks, max_output_bytes);
        let status = if response.failure.is_some() {
            ProcessStatus::Failed
        } else if response.exited {
            match response.exit_code {
                Some(0) => ProcessStatus::Succeeded,
                _ => ProcessStatus::Failed,
            }
        } else {
            ProcessStatus::Running
        };
        let handle = if response.closed || response.exited {
            None
        } else {
            Some(ProcessHandle::new(process_id.0))
        };
        ProcessOutput {
            status,
            handle,
            exit_code: response.exit_code,
            stdout,
            stderr,
        }
    }

    fn record_next_seq(&self, process_id: &ProcessId, response: &ReadProcessResponse) {
        let Ok(mut seqs) = self.next_seq_by_process.lock() else {
            return;
        };
        if response.closed || response.exited {
            seqs.remove(process_id.as_str());
        } else {
            seqs.insert(process_id.to_string(), response.next_seq);
        }
    }
}

fn access_policy_for_capabilities(capabilities: &HostCapabilities) -> FileAccessPolicy {
    if capabilities.filesystem_write {
        FileAccessPolicy::FullReadWrite
    } else {
        FileAccessPolicy::FullReadOnly
    }
}

fn host_path(path: &FsPath) -> Result<HostPath, FsError> {
    HostPath::new(path.as_str()).map_err(|error| FsError::InvalidInput {
        message: error.to_string(),
    })
}

fn process_host_path(path: &FsPath) -> Result<HostPath, ProcessError> {
    HostPath::new(path.as_str()).map_err(|error| ProcessError::InvalidRequest {
        message: error.to_string(),
    })
}

fn map_fs_error(error: HostClientError, path: &FsPath) -> FsError {
    match error {
        HostClientError::Host(error) => match error.code {
            HostErrorCode::NotFound => FsError::NotFound { path: path.clone() },
            HostErrorCode::Conflict => FsError::AlreadyExists { path: path.clone() },
            HostErrorCode::Unauthorized | HostErrorCode::Forbidden => {
                FsError::PermissionDenied { path: path.clone() }
            }
            HostErrorCode::Unsupported | HostErrorCode::CapabilityUnavailable => {
                FsError::Unsupported {
                    message: error.message,
                }
            }
            HostErrorCode::InvalidRequest => FsError::InvalidInput {
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

fn map_process_error(error: HostClientError) -> ProcessError {
    match error {
        HostClientError::Host(error) => match error.code {
            HostErrorCode::Unsupported | HostErrorCode::CapabilityUnavailable => {
                ProcessError::Unsupported {
                    message: error.message,
                }
            }
            HostErrorCode::InvalidRequest | HostErrorCode::NotFound => {
                ProcessError::InvalidRequest {
                    message: error.message,
                }
            }
            _ => ProcessError::Failed {
                message: error.message,
            },
        },
        other => ProcessError::Failed {
            message: other.to_string(),
        },
    }
}

fn split_output(
    chunks: Vec<remote_process::ProcessOutputChunk>,
    max_output_bytes: Option<u64>,
) -> (StreamOutput, StreamOutput) {
    let mut stdout = StreamOutput::default();
    let mut stderr = StreamOutput::default();
    for chunk in chunks {
        match chunk.stream {
            ProcessOutputStream::Stdout | ProcessOutputStream::Pty => {
                append_stream(&mut stdout, chunk.chunk.into_inner(), max_output_bytes);
            }
            ProcessOutputStream::Stderr => {
                append_stream(&mut stderr, chunk.chunk.into_inner(), max_output_bytes);
            }
        }
    }
    (stdout, stderr)
}

fn append_stream(output: &mut StreamOutput, bytes: Vec<u8>, max_output_bytes: Option<u64>) {
    let Some(limit) = max_output_bytes.map(|value| value as usize) else {
        output.bytes.extend(bytes);
        return;
    };
    let remaining = limit.saturating_sub(output.bytes.len());
    if bytes.len() > remaining {
        output.bytes.extend_from_slice(&bytes[..remaining]);
        output.truncated = true;
    } else {
        output.bytes.extend(bytes);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{Arc, Mutex},
    };

    use engine::storage::InMemoryBlobStore;
    use host_client::HostClientResult;
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
        async fn send(&mut self, message: Value) -> HostClientResult<()> {
            self.sent.lock().expect("sent lock").push(message);
            Ok(())
        }

        async fn recv(&mut self) -> HostClientResult<Option<Value>> {
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
        let connection = RemoteHostConnection::new(
            HostDataClient::new(transport),
            HostCapabilities::filesystem(true, true),
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
    async fn remote_process_executor_starts_reads_and_tracks_next_sequence() {
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
                    "nextSeq": 3
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
                    "nextSeq": 4
                }
            }),
        ]);
        let connection = RemoteHostConnection::new(
            HostDataClient::new(transport),
            HostCapabilities::filesystem(true, true).with_process(),
        );
        let process = connection.process_executor().expect("process executor");

        let output = process
            .run_process(ProcessRequest {
                argv: vec!["sh".to_owned(), "-lc".to_owned(), "cat".to_owned()],
                cwd: Some(FsPath::new("/workspace").expect("cwd")),
                env: BTreeMap::new(),
                stdin: None,
                timeout_ms: Some(60_000),
                yield_time_ms: Some(10),
                max_output_bytes: Some(1024),
            })
            .await
            .expect("run process");

        assert_eq!(output.status, ProcessStatus::Running);
        assert_eq!(output.handle, Some(ProcessHandle::new("proc-1")));
        assert_eq!(output.stdout.bytes, b"ok\n");
        assert_eq!(output.stderr.bytes, b"warn\n");

        let output = process
            .write_stdin(WriteProcessStdinRequest {
                handle: ProcessHandle::new("proc-1"),
                input: b"input\n".to_vec(),
                close_stdin: true,
                yield_time_ms: Some(10),
                max_output_bytes: Some(1024),
            })
            .await
            .expect("write stdin");

        assert_eq!(output.status, ProcessStatus::Succeeded);
        assert_eq!(output.handle, None);
        assert_eq!(output.stdout.bytes, b"done\n");

        let sent = sent.lock().expect("sent lock");
        assert_eq!(sent[0]["method"], "process/start");
        assert_eq!(sent[0]["params"]["pipeStdin"], true);
        assert_eq!(sent[1]["method"], "process/read");
        assert_eq!(sent[2]["method"], "process/write");
        assert_eq!(sent[3]["method"], "process/read");
        assert_eq!(sent[3]["params"]["afterSeq"], 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_connection_builds_tool_contexts() {
        let (transport, _sent) = MockTransport::new([]);
        let connection = RemoteHostConnection::new(
            HostDataClient::new(transport),
            HostCapabilities::filesystem(true, false),
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
        let connection = RemoteHostConnection::new(
            HostDataClient::new(transport),
            HostCapabilities::filesystem(true, true),
        )
        .with_cwd(FsPath::new("/workspace").expect("cwd"));
        let (fs_ctx, _env_ctx) = connection.into_contexts(Arc::new(InMemoryBlobStore::new()));

        let write = invoke_write_file(
            &fs_ctx,
            WriteFileArgs {
                path: FsPath::new("nested/file.txt").expect("path"),
                content: "hello\n".to_owned(),
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

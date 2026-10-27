//! Remote environment adapters backed by `environment-client`.
//!
//! One [`DataClient`] serves every adapter of a connection: calls are
//! multiplexed, so a long process read never blocks a file read. The
//! daemon's `initialize` response decides which adapters exist (capability
//! families) and bounds every request (advertised ceilings).

use std::{
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use environment_client::{ClientError, DataClient};
use environment_protocol::{
    bytes::ByteChunk,
    data::{
        exec::{
            CommandSpec, ExecInput, ExecState, ExitStatus, OutputPage, PtySize, ReturnOn, StoppedBy,
        },
        fs::{
            CreateDirectoryParams, FileKind, GetMetadataParams, GlobFilesLimits, GlobFilesParams,
            GlobFilesStop, ReadDirectoryParams, ReadFileParams, RemoveParams, RenameParams,
            SearchTextLimits, SearchTextParams, SearchTextStop, TreeFilter, WriteCondition,
            WriteFileParams,
        },
        handshake::{FilesystemLimits, InitializeResponse, JobLimits, TransferCeilings},
        job::{
            CancelJobsParams, CancelJobsResponse, ReadJobsParams, ReadJobsResponse,
            StartJobsParams, StartJobsResponse,
        },
        process::{
            ReadProcessParams, ReadProcessResponse, StartProcessParams, TerminateProcessParams,
            WriteProcessParams,
        },
        transfer::{
            AppendTransferInventoryParams, BeginTransferParams, ListTransferMissingParams,
            ListTransferMissingResponse, ReadTransferContentParams, ReadTransferContentResponse,
            ReadTransferInventoryParams, ReadTransferInventoryResponse, TransferOperationParams,
            TransferStatus, WriteTransferContentParams, WriteTransferContentResponse,
        },
    },
    error::{Capability, ErrorData, PreconditionReason, ProtocolErrorKind},
    ids::ProcessId,
    paths::EnvironmentPath,
};
use harness::storage::BlobStore;

use crate::{
    environment::{
        EnvironmentToolContext,
        jobs::{JobError, JobExecResult, JobExecutor},
        output::{ShownOutput, is_stderr},
        process::{
            ContinueProcessRequest, LeftoverProcess, ProcessError, ProcessExecResult,
            ProcessExecutor, ProcessHandle, ProcessOutput, ProcessRequest, ProcessSignal,
            ProcessStatus, StreamOutput,
        },
    },
    fs::{
        CreateDirectoryOptions, FileAccessPolicy, FileMetadata, FileSystem, FsError, FsGlobRequest,
        FsGlobResponse, FsPath, FsRangedRead, FsResult, FsSearchStop, FsTextSearchMatch,
        FsTextSearchRequest, FsTextSearchResponse, FsToolContext, FsVersion, ReadDirectoryEntry,
        RemoveOptions, RenameOptions, VersionedRead, WriteOptions,
    },
    transfer::EnvironmentTransfer,
};

/// Output a process read shows when the caller names no budget.
const DEFAULT_PROCESS_OUTPUT_BYTES: u64 = 512 * 1024;
/// Terminal size of a PTY the model asks for with `tty: true`.
const DEFAULT_PTY_SIZE: PtySize = PtySize { rows: 24, cols: 80 };
/// How long input that the process does not read yet is retried before the
/// write gives up.
const INPUT_RETRY_WINDOW: Duration = Duration::from_secs(10);

static NEXT_PROCESS_EXECUTOR_ID: AtomicU64 = AtomicU64::new(1);
static PROCESS_ID_NONCE: LazyLock<String> = LazyLock::new(|| {
    let started_at_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}-{started_at_nanos}", std::process::id())
});

/// A data-plane connection and the tool adapters it supports.
#[derive(Clone)]
pub struct RemoteEnvironmentConnection {
    client: DataClient,
    cwd: Option<FsPath>,
}

impl RemoteEnvironmentConnection {
    /// Adapters over an initialized client; capabilities and ceilings come
    /// from its `initialize` response.
    pub fn new(client: DataClient) -> Self {
        Self { client, cwd: None }
    }

    pub fn with_cwd(mut self, cwd: FsPath) -> Self {
        self.cwd = Some(cwd);
        self
    }

    pub fn client(&self) -> &DataClient {
        &self.client
    }

    pub fn initialize_response(&self) -> &InitializeResponse {
        self.client.initialize_response()
    }

    /// Close the shared connection once every adapter derived from it has
    /// finished.
    pub async fn close(&self) -> Result<(), ClientError> {
        self.client.close().await
    }

    pub fn file_system(&self) -> RemoteEnvironmentFileSystem {
        let initialized = self.initialize_response();
        RemoteEnvironmentFileSystem {
            client: self.client.clone(),
            writable: initialized
                .capabilities
                .filesystem
                .as_ref()
                .is_some_and(|filesystem| filesystem.write),
            limits: initialized.limits.filesystem.clone(),
        }
    }

    /// Present when the daemon serves the `process` family.
    pub fn process_executor(&self) -> Option<RemoteProcessExecutor> {
        let initialized = self.initialize_response();
        let process = initialized.capabilities.process.as_ref()?;
        Some(RemoteProcessExecutor {
            client: self.client.clone(),
            pty: process.pty,
            max_read_bytes: initialized.limits.process.max_read_bytes,
            max_wait_ms: initialized.limits.max_wait_ms,
            process_id_prefix: NEXT_PROCESS_EXECUTOR_ID.fetch_add(1, Ordering::Relaxed),
            next_process_id: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Present when the daemon serves the `job` family.
    pub fn job_executor(&self) -> Option<RemoteJobExecutor> {
        let initialized = self.initialize_response();
        initialized.capabilities.job.as_ref()?;
        Some(RemoteJobExecutor {
            client: self.client.clone(),
            limits: initialized.limits.job.clone(),
        })
    }

    pub fn into_contexts(
        self,
        blobs: Arc<dyn BlobStore>,
    ) -> (FsToolContext, EnvironmentToolContext) {
        let has_filesystem = self.initialize_response().capabilities.filesystem.is_some();
        let fs = Arc::new(self.file_system());
        let process = self
            .process_executor()
            .map(|executor| Arc::new(executor) as Arc<dyn ProcessExecutor>);
        let jobs = self
            .job_executor()
            .map(|executor| Arc::new(executor) as Arc<dyn JobExecutor>);
        let mut fs_ctx = FsToolContext::new(fs, blobs.clone());
        let mut env_ctx = EnvironmentToolContext::new(process, blobs);
        if has_filesystem {
            env_ctx.transfer = Some(Arc::new(self.client.clone()));
        }
        if let Some(jobs) = jobs {
            env_ctx = env_ctx.with_jobs(jobs);
        }
        if let Some(cwd) = self.cwd {
            fs_ctx = fs_ctx.with_cwd(cwd.clone());
            env_ctx = env_ctx.with_process_cwd(cwd);
        }
        // Filesystem tools exist only when the daemon serves the filesystem
        // family; write access follows its `write` fact through the file
        // system's access policy.
        if has_filesystem {
            env_ctx = env_ctx.with_filesystem(fs_ctx.clone());
        }
        (fs_ctx, env_ctx)
    }
}

#[async_trait]
impl EnvironmentTransfer for DataClient {
    fn transfer_ceilings(&self) -> TransferCeilings {
        self.initialize_response().limits.filesystem.transfer
    }

    async fn begin(&self, params: &BeginTransferParams) -> Result<TransferStatus, ClientError> {
        self.transfer_begin(params).await
    }

    async fn advance(
        &self,
        params: &TransferOperationParams,
    ) -> Result<TransferStatus, ClientError> {
        self.transfer_advance(params).await
    }

    async fn get_status(
        &self,
        params: &TransferOperationParams,
    ) -> Result<TransferStatus, ClientError> {
        self.transfer_get_status(params).await
    }

    async fn read_inventory(
        &self,
        params: &ReadTransferInventoryParams,
    ) -> Result<ReadTransferInventoryResponse, ClientError> {
        self.transfer_read_inventory(params).await
    }

    async fn append(
        &self,
        params: &AppendTransferInventoryParams,
    ) -> Result<TransferStatus, ClientError> {
        self.transfer_append(params).await
    }

    async fn list_missing(
        &self,
        params: &ListTransferMissingParams,
    ) -> Result<ListTransferMissingResponse, ClientError> {
        self.transfer_list_missing(params).await
    }

    async fn read(
        &self,
        params: &ReadTransferContentParams,
    ) -> Result<ReadTransferContentResponse, ClientError> {
        self.transfer_read(params).await
    }

    async fn write(
        &self,
        params: &WriteTransferContentParams,
    ) -> Result<WriteTransferContentResponse, ClientError> {
        self.transfer_write(params).await
    }

    async fn commit(
        &self,
        params: &TransferOperationParams,
    ) -> Result<TransferStatus, ClientError> {
        self.transfer_commit(params).await
    }

    async fn abort(&self, params: &TransferOperationParams) -> Result<TransferStatus, ClientError> {
        self.transfer_abort(params).await
    }
}

/// The environment filesystem over `fs/*`. Every bound comes from the
/// daemon's advertised ceilings: a read longer than one `fs/readFile` is
/// refused rather than returned short, and writes are staged and atomic.
#[derive(Clone)]
pub struct RemoteEnvironmentFileSystem {
    client: DataClient,
    writable: bool,
    limits: FilesystemLimits,
}

impl RemoteEnvironmentFileSystem {
    async fn read_whole(&self, path: &FsPath) -> FsResult<VersionedRead> {
        let response = self
            .client
            .read_file(&ReadFileParams {
                path: environment_path(path)?,
                offset: 0,
                max_bytes: self.limits.max_read_file_bytes,
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        if response.truncated {
            return Err(FsError::InvalidInput {
                message: format!(
                    "{path} is {} bytes; the environment returns at most {} bytes per read",
                    response.file_size, self.limits.max_read_file_bytes
                ),
            });
        }
        Ok(VersionedRead {
            bytes: response.data.into_inner(),
            version: Some(FsVersion::new(response.version.as_str())),
        })
    }

    async fn metadata(&self, path: &FsPath, follow_symlinks: bool) -> FsResult<FileKindFacts> {
        let metadata = self
            .client
            .get_metadata(&GetMetadataParams {
                path: environment_path(path)?,
                follow_symlinks,
            })
            .await
            .map_err(|error| map_fs_error(error, path))?
            .metadata;
        Ok(FileKindFacts {
            kind: metadata.kind,
            created_at_ms: metadata.created_at_ms.unwrap_or(0),
            modified_at_ms: metadata.modified_at_ms,
        })
    }
}

struct FileKindFacts {
    kind: FileKind,
    created_at_ms: i64,
    modified_at_ms: i64,
}

#[async_trait]
impl FileSystem for RemoteEnvironmentFileSystem {
    fn access_policy(&self) -> FileAccessPolicy {
        if self.writable {
            FileAccessPolicy::FullReadWrite
        } else {
            FileAccessPolicy::FullReadOnly
        }
    }

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
        Ok(self.read_whole(path).await?.bytes)
    }

    async fn read_file_versioned(&self, path: &FsPath) -> FsResult<VersionedRead> {
        self.read_whole(path).await
    }

    fn max_read_bytes(&self) -> Option<u64> {
        Some(self.limits.max_read_file_bytes)
    }

    async fn read_file_range(
        &self,
        path: &FsPath,
        offset: u64,
        max_bytes: Option<u64>,
    ) -> FsResult<FsRangedRead> {
        let ceiling = self.limits.max_read_file_bytes;
        let response = self
            .client
            .read_file(&ReadFileParams {
                path: environment_path(path)?,
                offset,
                max_bytes: max_bytes.map_or(ceiling, |max| max.min(ceiling)),
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        Ok(FsRangedRead {
            file_size: response.file_size,
            truncated: response.truncated,
            bytes: response.data.into_inner(),
        })
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
        self.write_file_with(path, contents, WriteOptions::default())
            .await
    }

    async fn write_file_with(
        &self,
        path: &FsPath,
        contents: Vec<u8>,
        options: WriteOptions,
    ) -> FsResult<()> {
        if contents.len() as u64 > self.limits.max_write_file_bytes {
            return Err(FsError::InvalidInput {
                message: format!(
                    "{} bytes exceed the environment's {}-byte inline write limit; write larger files from a content_ref",
                    contents.len(),
                    self.limits.max_write_file_bytes
                ),
            });
        }
        let if_match = options
            .if_match
            .map(|version| {
                environment_protocol::data::fs::FileVersion::new(version.as_str()).map_err(
                    |error| FsError::InvalidInput {
                        message: error.to_string(),
                    },
                )
            })
            .transpose()?;
        self.client
            .write_file(&WriteFileParams {
                path: environment_path(path)?,
                data: ByteChunk::from(contents),
                create: WriteCondition::Always,
                if_match,
                executable: false,
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
            .create_directory(&CreateDirectoryParams {
                path: environment_path(path)?,
                recursive: options.recursive,
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        Ok(())
    }

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata> {
        let own = self.metadata(path, false).await?;
        let is_symlink = own.kind == FileKind::Symlink;
        let target = if is_symlink {
            self.metadata(path, true).await?
        } else {
            own
        };
        Ok(FileMetadata {
            is_directory: target.kind == FileKind::Directory,
            is_file: target.kind == FileKind::File,
            is_symlink,
            created_at_ms: target.created_at_ms,
            modified_at_ms: target.modified_at_ms,
        })
    }

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
        let directory = environment_path(path)?;
        let mut entries = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .client
                .read_directory(&ReadDirectoryParams {
                    path: directory.clone(),
                    limit: self.limits.max_directory_entries,
                    cursor,
                })
                .await
                .map_err(|error| map_fs_error(error, path))?;
            entries.extend(page.entries.into_iter().map(|entry| ReadDirectoryEntry {
                file_name: entry.name.as_str().to_owned(),
                is_directory: entry.metadata.kind == FileKind::Directory,
                is_file: entry.metadata.kind == FileKind::File,
            }));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(entries)
    }

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()> {
        self.client
            .remove(&RemoveParams {
                path: environment_path(path)?,
                recursive: options.recursive,
                force: options.force,
            })
            .await
            .map_err(|error| map_fs_error(error, path))?;
        Ok(())
    }

    async fn rename(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: RenameOptions,
    ) -> FsResult<()> {
        self.client
            .rename(&RenameParams {
                source: environment_path(source_path)?,
                destination: environment_path(destination_path)?,
                overwrite: options.overwrite,
                if_match: None,
            })
            .await
            .map_err(|error| match error.protocol_kind() {
                Some(ProtocolErrorKind::AlreadyExists) => FsError::AlreadyExists {
                    path: destination_path.clone(),
                },
                _ => map_fs_error(error, source_path),
            })?;
        Ok(())
    }

    async fn search_text(
        &self,
        request: &FsTextSearchRequest,
    ) -> FsResult<Option<FsTextSearchResponse>> {
        let ceilings = self.limits.search;
        let mut params = SearchTextParams {
            root: environment_path(&request.root)?,
            pattern: request.pattern.clone(),
            pattern_ignore_case: !request.case_sensitive,
            filter: TreeFilter {
                include: request.include.iter().cloned().collect(),
                max_depth: request.max_depth.map(depth),
                ..TreeFilter::default()
            },
            limits: SearchTextLimits {
                max_matches: count(request.limits.max_matches, ceilings.max_matches),
                max_files: count(request.limits.max_files, ceilings.max_files),
                max_bytes: request.limits.max_bytes.min(ceilings.max_bytes),
                max_duration_ms: request.limits.max_duration_ms.min(ceilings.max_duration_ms),
                max_line_bytes: ceilings.max_line_bytes,
            },
        };
        let response = match self.client.search_text(&params).await {
            Ok(response) => response,
            // `fs/searchText` walks directories; a file root searches that
            // one file, as the generic fallback does: its parent, limited to
            // the file's own name.
            Err(error) if is_not_directory(&error) => {
                let Some(file_root) = file_root_search(request)? else {
                    return Ok(Some(FsTextSearchResponse {
                        matches: Vec::new(),
                        files_searched: 0,
                        bytes_searched: 0,
                        skipped_files: 0,
                        stopped: None,
                    }));
                };
                params.root = file_root.parent;
                params.filter = file_root.filter;
                self.client
                    .search_text(&params)
                    .await
                    .map_err(|error| map_fs_error(error, &request.root))?
            }
            Err(error) => return Err(map_fs_error(error, &request.root)),
        };
        let matches = response
            .matches
            .into_iter()
            .map(|item| {
                Ok(FsTextSearchMatch {
                    path: FsPath::new(item.path.as_str()).map_err(FsError::from)?,
                    line_number: u64::from(item.line_number),
                    line: item.line,
                    line_truncated: item.line_truncated,
                })
            })
            .collect::<FsResult<Vec<_>>>()?;
        Ok(Some(FsTextSearchResponse {
            matches,
            files_searched: u64::from(response.searched_file_count),
            bytes_searched: response.searched_bytes,
            skipped_files: u64::from(response.skipped_file_count),
            stopped: response.stopped.map(|stop| match stop {
                SearchTextStop::MatchLimit => FsSearchStop::MatchLimit,
                SearchTextStop::FileLimit => FsSearchStop::FileLimit,
                SearchTextStop::ByteLimit => FsSearchStop::ByteLimit,
                SearchTextStop::TimeLimit => FsSearchStop::TimeLimit,
                SearchTextStop::ResponseLimit => FsSearchStop::ResponseLimit,
            }),
        }))
    }

    async fn glob_files(&self, request: &FsGlobRequest) -> FsResult<Option<FsGlobResponse>> {
        let ceilings = self.limits.glob;
        let params = GlobFilesParams {
            root: environment_path(&request.root)?,
            filter: TreeFilter {
                include: vec![request.pattern.clone()],
                max_depth: request.max_depth.map(depth),
                ..TreeFilter::default()
            },
            limits: GlobFilesLimits {
                max_matches: count(request.limits.max_matches, ceilings.max_matches),
                max_files: count(request.limits.max_entries, ceilings.max_files),
                max_duration_ms: request.limits.max_duration_ms.min(ceilings.max_duration_ms),
            },
        };
        let response = self
            .client
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
            entries_visited: u64::from(response.visited_file_count),
            stopped: response.stopped.map(|stop| match stop {
                GlobFilesStop::MatchLimit => FsSearchStop::MatchLimit,
                GlobFilesStop::FileLimit => FsSearchStop::FileLimit,
                GlobFilesStop::TimeLimit => FsSearchStop::TimeLimit,
                GlobFilesStop::ResponseLimit => FsSearchStop::ResponseLimit,
            }),
        }))
    }
}

fn is_not_directory(error: &ClientError) -> bool {
    matches!(
        error.protocol_error().map(|error| &error.data),
        Some(ErrorData::FailedPrecondition {
            reason: PreconditionReason::NotDirectory
        })
    )
}

/// A text search rooted at a file, as a search of its parent.
struct FileRootSearch {
    parent: EnvironmentPath,
    filter: TreeFilter,
}

/// The parent walk that searches only `request.root`, or `None` when the
/// request's include does not select the file (nothing to search).
fn file_root_search(request: &FsTextSearchRequest) -> FsResult<Option<FileRootSearch>> {
    let invalid = || FsError::InvalidInput {
        message: format!("{} is neither a directory nor a file", request.root),
    };
    let name = request.root.file_name().ok_or_else(invalid)?;
    let parent = request.root.parent().ok_or_else(invalid)?;
    if let Some(include) = request.include.as_deref() {
        let pattern = crate::fs::pattern::PathPattern::new(include, false).map_err(|error| {
            FsError::InvalidInput {
                message: error.to_string(),
            }
        })?;
        if !pattern.matches(name, false) {
            return Ok(None);
        }
    }
    Ok(Some(FileRootSearch {
        parent: environment_path(&parent)?,
        filter: TreeFilter {
            include: vec![format!("/{}", globset::escape(name))],
            max_depth: Some(1),
            include_hidden: true,
            include_ignored: true,
            ..TreeFilter::default()
        },
    }))
}

fn count(requested: u64, ceiling: u32) -> u32 {
    u32::try_from(requested).unwrap_or(u32::MAX).min(ceiling)
}

fn depth(depth: usize) -> u32 {
    u32::try_from(depth).unwrap_or(u32::MAX)
}

/// Process executor over the data plane. It keeps no per-handle state: the
/// daemon owns the read cursor, so a handle continued from a later batch or
/// connection reads only what was not delivered yet.
#[derive(Clone)]
pub struct RemoteProcessExecutor {
    client: DataClient,
    pty: bool,
    max_read_bytes: u64,
    max_wait_ms: u64,
    process_id_prefix: u64,
    next_process_id: Arc<AtomicU64>,
}

#[async_trait]
impl ProcessExecutor for RemoteProcessExecutor {
    async fn run_process(&self, request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
        if request.tty && !self.pty {
            return Err(ProcessError::Unsupported {
                message: "this environment cannot run commands under a PTY (tty: true)".to_owned(),
            });
        }
        let process_id = self.next_process_id();
        self.client
            .start_process(&StartProcessParams {
                process_id: process_id.clone(),
                command: CommandSpec {
                    argv: request.argv,
                    cwd: request
                        .cwd
                        .as_ref()
                        .map(process_environment_path)
                        .transpose()?,
                    env: request.env,
                    secret_env: request.secret_env,
                    input: match request.stdin {
                        Some(stdin) => ExecInput::Bytes {
                            data: ByteChunk::from(stdin),
                        },
                        None => ExecInput::None {},
                    },
                    timeout_ms: request.timeout_ms,
                },
                pty: request.tty.then_some(DEFAULT_PTY_SIZE),
            })
            .await
            .map_err(map_process_error)?;
        let handle = ProcessHandle::new(process_id.as_str());
        self.read_output(
            &handle,
            &process_id,
            request.yield_ms,
            request.max_output_bytes,
        )
        .await
    }

    async fn continue_process(
        &self,
        request: ContinueProcessRequest,
    ) -> ProcessExecResult<ProcessOutput> {
        let handle = request.handle;
        let process_id =
            ProcessId::new(handle.as_str()).map_err(|_| ProcessError::UnknownHandle {
                handle: handle.clone(),
            })?;
        if let Some(signal) = request.signal {
            self.client
                .terminate_process(&TerminateProcessParams {
                    process_id: process_id.clone(),
                    signal: match signal {
                        ProcessSignal::Interrupt => {
                            environment_protocol::data::process::ProcessSignal::Interrupt
                        }
                        ProcessSignal::Kill => {
                            environment_protocol::data::process::ProcessSignal::Kill
                        }
                    },
                })
                .await
                .map_err(|error| map_handle_error(error, &handle))?;
        } else {
            let input = request.input.unwrap_or_default();
            if !input.is_empty() || request.close_stdin {
                self.write_input(&handle, &process_id, input, request.close_stdin)
                    .await?;
            }
        }
        self.read_output(
            &handle,
            &process_id,
            request.wait_ms,
            request.max_output_bytes,
        )
        .await
    }
}

impl RemoteProcessExecutor {
    fn next_process_id(&self) -> ProcessId {
        let id = self.next_process_id.fetch_add(1, Ordering::Relaxed);
        ProcessId::new(format!(
            "proc-{}-{}-{id}",
            PROCESS_ID_NONCE.as_str(),
            self.process_id_prefix
        ))
        .expect("generated process ids fit the id grammar")
    }

    /// Queue `input` for the process, resending what a full input queue did
    /// not accept, then close the input when asked.
    async fn write_input(
        &self,
        handle: &ProcessHandle,
        process_id: &ProcessId,
        input: Vec<u8>,
        close_input: bool,
    ) -> ProcessExecResult<()> {
        let started = Instant::now();
        let mut offset = 0usize;
        loop {
            let rest = &input[offset..];
            let accepted = self
                .client
                .write_process(&WriteProcessParams {
                    process_id: process_id.clone(),
                    data: ByteChunk::from(rest),
                    close_input,
                })
                .await
                .map_err(|error| map_handle_error(error, handle))?
                .accepted_bytes;
            offset += usize::try_from(accepted)
                .unwrap_or(usize::MAX)
                .min(rest.len());
            if offset >= input.len() {
                return Ok(());
            }
            if started.elapsed() >= INPUT_RETRY_WINDOW {
                return Err(ProcessError::Failed {
                    message: format!(
                        "process {handle} accepted {offset} of {} input bytes; it is not reading its input",
                        input.len()
                    ),
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Read until the process exits and its output settles, or until
    /// `wait_ms` elapses. A finished command whose output exceeds the budget
    /// shows its head and tail with the omitted count in between; a running
    /// one keeps the rest behind the daemon cursor for the next call.
    async fn read_output(
        &self,
        handle: &ProcessHandle,
        process_id: &ProcessId,
        wait_ms: Option<u64>,
        max_output_bytes: Option<u64>,
    ) -> ProcessExecResult<ProcessOutput> {
        let budget = max_output_bytes
            .unwrap_or(DEFAULT_PROCESS_OUTPUT_BYTES)
            .max(1);
        let deadline = wait_ms.map(|wait| Instant::now() + Duration::from_millis(wait));
        let mut pages = Vec::new();
        let mut collected = 0u64;
        let mut settled = false;
        let last = loop {
            // Once the exit has settled, further pages only drain output.
            let wait = match deadline {
                _ if settled => 0,
                Some(deadline) => deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64,
                None => self.max_wait_ms,
            }
            .min(self.max_wait_ms);
            let response = self
                .read_page(
                    handle,
                    ReadProcessParams {
                        process_id: process_id.clone(),
                        after: None,
                        tail_bytes: None,
                        ack: None,
                        max_bytes: (budget - collected).min(self.max_read_bytes),
                        wait_ms: wait,
                        return_on: ReturnOn::Exit,
                    },
                )
                .await?;
            settled = response.state == ExecState::Exited;
            collected += page_bytes(&response.output);
            pages.push(response.output.clone());
            let more_output = response.output.next < response.output.end;
            let out_of_time = deadline.is_some_and(|deadline| Instant::now() >= deadline);
            match response.state {
                ExecState::Exited if !more_output => break response,
                ExecState::Exited if collected >= budget => {
                    let tail_bytes = budget / 2;
                    let tail = self
                        .read_page(
                            handle,
                            ReadProcessParams {
                                process_id: process_id.clone(),
                                after: None,
                                tail_bytes: Some(tail_bytes),
                                ack: None,
                                max_bytes: tail_bytes.min(self.max_read_bytes),
                                wait_ms: 0,
                                return_on: ReturnOn::AnyOutput,
                            },
                        )
                        .await?;
                    pages.push(tail.output.clone());
                    break tail;
                }
                // The page ended before the budget (its chunk limit): read on.
                ExecState::Exited => continue,
                ExecState::Running if collected >= budget || out_of_time => break response,
                ExecState::Running => continue,
            }
        };
        Ok(output_from_read(handle, budget, pages, last))
    }

    async fn read_page(
        &self,
        handle: &ProcessHandle,
        params: ReadProcessParams,
    ) -> ProcessExecResult<ReadProcessResponse> {
        self.client
            .read_process(&params)
            .await
            .map_err(|error| map_handle_error(error, handle))
    }
}

fn page_bytes(page: &OutputPage) -> u64 {
    page.chunks
        .iter()
        .map(|chunk| chunk.data.len() as u64)
        .sum()
}

/// Shape the pages of one call into the model-facing output. The last page
/// may be a tail read; earlier pages are consecutive cursor reads.
fn output_from_read(
    handle: &ProcessHandle,
    budget: u64,
    mut pages: Vec<OutputPage>,
    last: ReadProcessResponse,
) -> ProcessOutput {
    let exited = last.state == ExecState::Exited;
    let tail = (pages.len() > 1 && exited && last.output.next == last.output.end)
        .then(|| pages.pop())
        .flatten();
    let head = merge_pages(pages);
    let shown = match tail {
        Some(tail) if tail.end > head.next => ShownOutput::from_pages(head, Some(tail)),
        Some(tail) => ShownOutput::from_pages(merge_pages(vec![head, tail]), None),
        None => ShownOutput::from_pages(head, None),
    }
    .trim_to(budget);
    let (stdout, stderr) = split_output(&shown);
    let (status, exit_code, failure) = if !exited {
        (ProcessStatus::Running, None, None)
    } else {
        match (&last.stopped_by, &last.exit, &last.error) {
            (Some(StoppedBy::Timeout), exit, _) => (ProcessStatus::TimedOut, exit_code(exit), None),
            (Some(StoppedBy::Kill | StoppedBy::Terminate), exit, _) => {
                (ProcessStatus::Killed, exit_code(exit), None)
            }
            (None, _, Some(error)) => (ProcessStatus::Failed, None, Some(error.to_string())),
            (None, Some(ExitStatus::Code { code: 0 }), None) => {
                (ProcessStatus::Succeeded, Some(0), None)
            }
            (None, Some(ExitStatus::Code { code }), None) => {
                (ProcessStatus::Failed, Some(*code), None)
            }
            (
                None,
                Some(ExitStatus::Signal {
                    signal,
                    core_dumped,
                }),
                None,
            ) => (
                ProcessStatus::Failed,
                None,
                Some(format!(
                    "killed by {signal}{}",
                    if *core_dumped { " (core dumped)" } else { "" }
                )),
            ),
            (None, None, None) => (
                ProcessStatus::Failed,
                None,
                Some("the environment reported no exit status".to_owned()),
            ),
        }
    };
    ProcessOutput {
        status,
        handle: (!exited).then(|| handle.clone()),
        pid: Some(last.pid),
        exit_code,
        failure,
        stdout,
        stderr,
        omitted_bytes: shown.omitted_bytes(),
        leftover_processes: last
            .leftovers
            .map(|report| {
                report
                    .processes
                    .into_iter()
                    .map(|member| LeftoverProcess {
                        pid: member.pid,
                        command: member.command.unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn exit_code(exit: &Option<ExitStatus>) -> Option<i32> {
    match exit {
        Some(ExitStatus::Code { code }) => Some(*code),
        _ => None,
    }
}

/// Consecutive pages as one: chunks and gaps in order, the last page's
/// position.
fn merge_pages(pages: Vec<OutputPage>) -> OutputPage {
    let mut merged = OutputPage::default();
    for page in pages {
        merged.chunks.extend(page.chunks);
        merged.gaps.extend(page.gaps);
        merged.next = merged.next.max(page.next);
        merged.end = merged.end.max(page.end);
    }
    merged
}

/// Sorts chunks into their streams. The omission marker goes where output
/// was left out: before the first chunk after the omitted range, or at the
/// end of standard output when nothing follows it.
fn split_output(shown: &ShownOutput) -> (StreamOutput, StreamOutput) {
    let mut stdout = StreamOutput::default();
    let mut stderr = StreamOutput::default();
    for chunk in &shown.head {
        let stream = if is_stderr(chunk.stream) {
            &mut stderr
        } else {
            &mut stdout
        };
        stream.bytes.extend_from_slice(chunk.data.as_slice());
    }
    if shown.omitted.is_some() {
        let stream = match shown.tail.first() {
            Some(chunk) if is_stderr(chunk.stream) => &mut stderr,
            _ => &mut stdout,
        };
        stream.omitted_at = Some(stream.bytes.len());
    }
    for chunk in &shown.tail {
        let stream = if is_stderr(chunk.stream) {
            &mut stderr
        } else {
            &mut stdout
        };
        stream.bytes.extend_from_slice(chunk.data.as_slice());
    }
    (stdout, stderr)
}

#[derive(Clone)]
pub struct RemoteJobExecutor {
    client: DataClient,
    limits: JobLimits,
}

#[async_trait]
impl JobExecutor for RemoteJobExecutor {
    fn limits(&self) -> Option<JobLimits> {
        Some(self.limits.clone())
    }

    async fn start_jobs(&self, request: StartJobsParams) -> JobExecResult<StartJobsResponse> {
        self.client
            .start_jobs(&request)
            .await
            .map_err(map_job_error)
    }

    async fn read_jobs(&self, request: ReadJobsParams) -> JobExecResult<ReadJobsResponse> {
        self.client.read_jobs(&request).await.map_err(map_job_error)
    }

    async fn cancel_jobs(&self, request: CancelJobsParams) -> JobExecResult<CancelJobsResponse> {
        self.client
            .cancel_jobs(&request)
            .await
            .map_err(map_job_error)
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

fn map_fs_error(error: ClientError, path: &FsPath) -> FsError {
    let Some(protocol) = error.protocol_error() else {
        return FsError::Failed {
            message: error.to_string(),
        };
    };
    let message = protocol.message.clone();
    match &protocol.data {
        ErrorData::NotFound { .. } => FsError::NotFound { path: path.clone() },
        ErrorData::AlreadyExists {} => FsError::AlreadyExists { path: path.clone() },
        ErrorData::PermissionDenied { .. } => FsError::PermissionDenied { path: path.clone() },
        ErrorData::FailedPrecondition {
            reason: PreconditionReason::IfMatchFailed,
        } => FsError::Conflict { path: path.clone() },
        ErrorData::CapabilityUnavailable {
            capability: Capability::FilesystemWrite,
        } => FsError::PermissionDenied { path: path.clone() },
        ErrorData::CapabilityUnavailable { .. } => FsError::Unsupported { message },
        ErrorData::FailedPrecondition { .. }
        | ErrorData::InvalidParams { .. }
        | ErrorData::InvalidRequest {}
        | ErrorData::PayloadTooLarge { .. } => FsError::InvalidInput { message },
        _ => FsError::Failed {
            message: protocol.to_string(),
        },
    }
}

fn map_process_error(error: ClientError) -> ProcessError {
    let Some(protocol) = error.protocol_error() else {
        return ProcessError::Failed {
            message: error.to_string(),
        };
    };
    let message = protocol.message.clone();
    match protocol.kind() {
        ProtocolErrorKind::CapabilityUnavailable => ProcessError::Unsupported { message },
        ProtocolErrorKind::InvalidParams
        | ProtocolErrorKind::InvalidRequest
        | ProtocolErrorKind::NotFound
        | ProtocolErrorKind::PermissionDenied
        | ProtocolErrorKind::FailedPrecondition => ProcessError::InvalidRequest {
            message: protocol.to_string(),
        },
        _ => ProcessError::Failed {
            message: protocol.to_string(),
        },
    }
}

/// Errors of calls on an existing handle: an unknown, expired or earlier
/// daemon's handle is `notFound`, closed input `inputClosed`.
fn map_handle_error(error: ClientError, handle: &ProcessHandle) -> ProcessError {
    match error.protocol_error().map(|protocol| &protocol.data) {
        Some(ErrorData::NotFound { .. }) => ProcessError::UnknownHandle {
            handle: handle.clone(),
        },
        Some(ErrorData::FailedPrecondition {
            reason: PreconditionReason::InputClosed,
        }) => ProcessError::StdinClosed {
            handle: handle.clone(),
        },
        _ => map_process_error(error),
    }
}

fn map_job_error(error: ClientError) -> JobError {
    let Some(protocol) = error.protocol_error() else {
        return JobError::Failed {
            message: error.to_string(),
        };
    };
    let message = protocol.message.clone();
    match &protocol.data {
        ErrorData::FailedPrecondition {
            reason: PreconditionReason::JobIdInUse,
        } => JobError::JobIdInUse {
            message: format!("{message}; choose a new job_id for a new run"),
        },
        ErrorData::CapabilityUnavailable { .. } => JobError::Unsupported { message },
        ErrorData::InvalidParams { .. }
        | ErrorData::InvalidRequest {}
        | ErrorData::FailedPrecondition { .. }
        | ErrorData::NotFound { .. } => JobError::InvalidRequest { message },
        _ => JobError::Failed {
            message: protocol.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::Mutex,
    };

    use environment_protocol::{
        data::exec::{OutputChunk, OutputStream},
        error::ProtocolError,
        jsonrpc::{Request, Response},
    };
    use harness::storage::InMemoryBlobStore;
    use serde_json::{Value, json};

    use super::*;
    use crate::fs::tools::{ReadFileArgs, WriteFileArgs, invoke_read_file, invoke_write_file};

    /// Answers each request with the next scripted result and records it.
    struct Script {
        replies: Mutex<VecDeque<Result<Value, ProtocolError>>>,
        sent: Mutex<Vec<Request>>,
    }

    fn initialize(capabilities: Value) -> InitializeResponse {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../environment-protocol/fixtures/data/initialize.json"
        ))
        .unwrap();
        let mut response = fixture["response"].clone();
        response["capabilities"] = capabilities;
        response["defaultCwd"] = json!("/workspace");
        serde_json::from_value(response).unwrap()
    }

    fn scripted(
        capabilities: Value,
        replies: impl IntoIterator<Item = Result<Value, ProtocolError>>,
    ) -> (DataClient, Arc<Script>) {
        let script = Arc::new(Script {
            replies: Mutex::new(replies.into_iter().collect()),
            sent: Mutex::new(Vec::new()),
        });
        let handler_script = script.clone();
        let client = DataClient::in_process(initialize(capabilities), move |request: Request| {
            let script = handler_script.clone();
            Box::pin(async move {
                let reply = script
                    .replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| panic!("unscripted request {}", request.method));
                let id = request.id.clone();
                script.sent.lock().unwrap().push(request);
                Ok(match reply {
                    Ok(value) => Response::success(id, &value),
                    Err(error) => Response::failure(Some(id), error),
                })
            })
        });
        (client, script)
    }

    fn all() -> Value {
        json!({"filesystem": {"write": true}, "process": {"pty": true}, "job": {}})
    }

    fn sent(script: &Script) -> Vec<(String, Value)> {
        script
            .sent
            .lock()
            .unwrap()
            .iter()
            .map(|request| (request.method.clone(), request.params.clone()))
            .collect()
    }

    fn read_response(chunks: Value, state: &str, next: u64, end: u64) -> Value {
        let mut response = json!({
            "output": {"chunks": chunks, "next": next, "end": end},
            "state": state,
            "pid": 4242
        });
        if state == "exited" {
            response["exit"] = json!({"kind": "code", "code": 0});
        }
        response
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_reads_are_bounded_and_refuse_truncated_files() {
        let (client, script) = scripted(
            all(),
            [
                Ok(json!({"data": "aGk=", "fileSize": 2, "version": "v1"})),
                Ok(json!({"version": "v2"})),
                Ok(
                    json!({"data": "aGk=", "fileSize": 9000000, "truncated": true, "version": "v3"}),
                ),
            ],
        );
        let fs = RemoteEnvironmentConnection::new(client).file_system();
        let path = FsPath::new("/workspace/file.txt").unwrap();

        let read = fs.read_file_versioned(&path).await.unwrap();
        assert_eq!(read.bytes, b"hi");
        fs.write_file_with(
            &path,
            b"updated".to_vec(),
            WriteOptions {
                if_match: read.version,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            fs.read_file(&path).await,
            Err(FsError::InvalidInput { .. })
        ));

        let sent = sent(&script);
        assert_eq!(sent[0].0, "fs/readFile");
        assert_eq!(sent[0].1["maxBytes"], 8388608, "reads send the ceiling");
        assert_eq!(sent[1].0, "fs/writeFile");
        assert_eq!(sent[1].1["data"], "dXBkYXRlZA==");
        assert_eq!(sent[1].1["ifMatch"], "v1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_changed_file_is_a_conflict_and_moves_use_rename() {
        let (client, script) = scripted(
            all(),
            [
                Err(ProtocolError::failed_precondition(
                    PreconditionReason::IfMatchFailed,
                    "changed",
                )),
                Ok(json!({})),
                Err(ProtocolError::already_exists("exists")),
            ],
        );
        let fs = RemoteEnvironmentConnection::new(client).file_system();
        let path = FsPath::new("/workspace/file.txt").unwrap();
        let moved = FsPath::new("/workspace/moved.txt").unwrap();

        assert!(matches!(
            fs.write_file_with(
                &path,
                b"x".to_vec(),
                WriteOptions {
                    if_match: Some(FsVersion::new("old")),
                },
            )
            .await,
            Err(FsError::Conflict { .. })
        ));
        fs.rename(&path, &moved, RenameOptions::default())
            .await
            .unwrap();
        assert!(matches!(
            fs.rename(&path, &moved, RenameOptions::default()).await,
            Err(FsError::AlreadyExists { path }) if path == moved
        ));
        let sent = sent(&script);
        assert_eq!(sent[1].0, "fs/rename");
        assert_eq!(sent[1].1["destination"], "/workspace/moved.txt");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn process_runs_and_continues_through_the_daemon_cursor() {
        let (client, script) = scripted(
            all(),
            [
                Ok(json!({"pid": 4242})),
                Ok(read_response(
                    json!([
                        {"offset": 0, "stream": "stdout", "data": "b2sK"},
                        {"offset": 3, "stream": "stderr", "data": "d2Fybgo="}
                    ]),
                    "running",
                    8,
                    8,
                )),
                Ok(json!({"acceptedBytes": 6})),
                Ok({
                    let mut response = read_response(
                        json!([{"offset": 8, "stream": "stdout", "data": "ZG9uZQo="}]),
                        "exited",
                        13,
                        13,
                    );
                    response["leftovers"] = json!({
                        "disposition": "kept",
                        "processes": [{"pid": 4243, "command": "sleep 100"}]
                    });
                    response
                }),
            ],
        );
        let process = RemoteEnvironmentConnection::new(client)
            .process_executor()
            .unwrap();

        let output = process
            .run_process(ProcessRequest {
                argv: vec!["sh".to_owned(), "-lc".to_owned(), "cat".to_owned()],
                cwd: Some(FsPath::new("/workspace").unwrap()),
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                stdin: None,
                tty: true,
                timeout_ms: None,
                yield_ms: Some(0),
                max_output_bytes: Some(1024),
            })
            .await
            .unwrap();
        let process_id = sent(&script)[0].1["processId"].as_str().unwrap().to_owned();
        assert_eq!(output.status, ProcessStatus::Running);
        assert_eq!(output.handle, Some(ProcessHandle::new(process_id.clone())));
        assert_eq!(output.stdout.bytes, b"ok\n");
        assert_eq!(output.stderr.bytes, b"warn\n");

        let output = process
            .continue_process(ContinueProcessRequest {
                handle: ProcessHandle::new(process_id.clone()),
                input: Some(b"input\n".to_vec()),
                close_stdin: true,
                signal: None,
                wait_ms: Some(0),
                max_output_bytes: Some(1024),
            })
            .await
            .unwrap();
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

        let sent = sent(&script);
        assert_eq!(sent[0].0, "process/start");
        assert_eq!(sent[0].1["pty"], json!({"rows": 24, "cols": 80}));
        assert!(
            sent[0].1["command"].get("input").is_none(),
            "stdin stays /dev/null"
        );
        assert_eq!(sent[1].0, "process/read");
        assert!(
            sent[1].1.get("after").is_none(),
            "reads use the daemon cursor"
        );
        assert_eq!(sent[1].1["returnOn"], "exit");
        assert_eq!(sent[2].0, "process/write");
        assert_eq!(sent[2].1["closeInput"], true);
        assert_eq!(sent[3].0, "process/read");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_finished_command_shows_head_and_tail_of_large_output() {
        let head = "h".repeat(8);
        let tail = "t".repeat(4);
        let encode = |text: &str| {
            use std::io::Write;
            let mut out = Vec::new();
            write!(out, "{text}").unwrap();
            serde_json::to_value(ByteChunk::from(out)).unwrap()
        };
        let (client, script) = scripted(
            all(),
            [
                Ok(json!({"pid": 1})),
                Ok({
                    let mut response = read_response(
                        json!([{"offset": 0, "stream": "stdout", "data": encode(&head)}]),
                        "exited",
                        8,
                        100,
                    );
                    response["exit"] = json!({"kind": "code", "code": 1});
                    response
                }),
                Ok({
                    let mut response = read_response(
                        json!([{"offset": 96, "stream": "stdout", "data": encode(&tail)}]),
                        "exited",
                        100,
                        100,
                    );
                    response["exit"] = json!({"kind": "code", "code": 1});
                    response
                }),
            ],
        );
        let process = RemoteEnvironmentConnection::new(client)
            .process_executor()
            .unwrap();
        let output = process
            .run_process(ProcessRequest {
                max_output_bytes: Some(8),
                ..ProcessRequest::argv(["make"])
            })
            .await
            .unwrap();

        assert_eq!(output.status, ProcessStatus::Failed);
        assert_eq!(output.exit_code, Some(1));
        assert_eq!(output.handle, None);
        assert_eq!(output.stdout.bytes, b"hhhhtttt");
        assert_eq!(output.stdout.omitted_at, Some(4));
        assert_eq!(output.omitted_bytes, 92);
        let sent = sent(&script);
        assert_eq!(sent[2].1["tailBytes"], 4, "the end of the output is read");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handles_map_not_found_input_closed_and_signals() {
        let (client, script) = scripted(
            all(),
            [
                Ok(json!({"running": true})),
                Ok({
                    let mut response = read_response(json!([]), "exited", 0, 0);
                    response["exit"] = json!({"kind": "signal", "signal": "SIGKILL"});
                    response["stoppedBy"] = json!("kill");
                    response
                }),
                Err(ProtocolError::failed_precondition(
                    PreconditionReason::InputClosed,
                    "no input",
                )),
                Err(ProtocolError::not_found("unknown process")),
            ],
        );
        let process = RemoteEnvironmentConnection::new(client)
            .process_executor()
            .unwrap();
        let continue_with = |input: Option<&[u8]>, signal| ContinueProcessRequest {
            handle: ProcessHandle::new("proc-9"),
            input: input.map(<[u8]>::to_vec),
            close_stdin: false,
            signal,
            wait_ms: Some(0),
            max_output_bytes: None,
        };

        let killed = process
            .continue_process(continue_with(None, Some(ProcessSignal::Kill)))
            .await
            .unwrap();
        assert_eq!(killed.status, ProcessStatus::Killed);
        assert_eq!(killed.handle, None);
        assert!(matches!(
            process
                .continue_process(continue_with(Some(b"x"), None))
                .await,
            Err(ProcessError::StdinClosed { .. })
        ));
        assert!(matches!(
            process
                .continue_process(continue_with(Some(b"x"), None))
                .await,
            Err(ProcessError::UnknownHandle { .. })
        ));
        assert_eq!(sent(&script)[0].1["signal"], "kill");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn capability_facts_gate_the_adapters() {
        let (client, script) = scripted(json!({"filesystem": {}, "process": {}}), []);
        let connection =
            RemoteEnvironmentConnection::new(client).with_cwd(FsPath::new("/workspace").unwrap());
        let process = connection.process_executor().unwrap();
        assert!(matches!(
            process
                .run_process(ProcessRequest {
                    tty: true,
                    ..ProcessRequest::argv(["vim"])
                })
                .await,
            Err(ProcessError::Unsupported { .. })
        ));
        assert!(script.sent.lock().unwrap().is_empty(), "nothing was sent");
        assert!(connection.job_executor().is_none());

        let (fs_ctx, env_ctx) = connection.into_contexts(Arc::new(InMemoryBlobStore::new()));
        assert_eq!(fs_ctx.fs.access_policy(), FileAccessPolicy::FullReadOnly);
        assert_eq!(fs_ctx.fs_cwd, Some(FsPath::new("/workspace").unwrap()));
        assert!(env_ctx.filesystem.is_some());
        assert!(env_ctx.transfer.is_some());
        assert!(env_ctx.jobs.is_none());

        let (client, _) = scripted(json!({}), []);
        let (_, none) = RemoteEnvironmentConnection::new(client)
            .into_contexts(Arc::new(InMemoryBlobStore::new()));
        assert!(none.filesystem.is_none() && none.process.is_none() && none.transfer.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn process_executors_use_distinct_process_id_prefixes() {
        let exited = || Ok(read_response(json!([]), "exited", 0, 0));
        let (client_a, sent_a) = scripted(all(), [Ok(json!({"pid": 1})), exited()]);
        let (client_b, sent_b) = scripted(all(), [Ok(json!({"pid": 2})), exited()]);
        for client in [client_a, client_b] {
            RemoteEnvironmentConnection::new(client)
                .process_executor()
                .unwrap()
                .run_process(ProcessRequest::argv(["true"]))
                .await
                .unwrap();
        }
        let id_a = sent(&sent_a)[0].1["processId"].clone();
        let id_b = sent(&sent_b)[0].1["processId"].clone();
        assert_ne!(id_a, id_b);
        assert!(id_a.as_str().unwrap().starts_with("proc-"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn existing_file_tools_work_through_remote_context() {
        let (client, script) = scripted(
            all(),
            [
                Ok(json!({})),
                Ok(json!({"version": "v1"})),
                Ok(json!({"data": "aGVsbG8K", "fileSize": 6, "version": "v1"})),
            ],
        );
        let connection =
            RemoteEnvironmentConnection::new(client).with_cwd(FsPath::new("/workspace").unwrap());
        let (fs_ctx, _env_ctx) = connection.into_contexts(Arc::new(InMemoryBlobStore::new()));

        let write = invoke_write_file(
            &fs_ctx,
            WriteFileArgs {
                path: FsPath::new("nested/file.txt").unwrap(),
                content: Some("hello\n".to_owned()),
                content_ref: None,
            },
        )
        .await
        .unwrap();
        let read = invoke_read_file(
            &fs_ctx,
            ReadFileArgs {
                path: FsPath::new("nested/file.txt").unwrap(),
                offset: None,
                limit: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            write.resolved_path,
            FsPath::new("/workspace/nested/file.txt").unwrap()
        );
        assert_eq!(read.text, "hello");
        let sent = sent(&script);
        assert_eq!(sent[0].0, "fs/createDirectory");
        assert_eq!(sent[0].1["path"], "/workspace/nested");
        assert_eq!(sent[1].0, "fs/writeFile");
        assert_eq!(sent[2].0, "fs/readFile");
        assert_eq!(sent[2].1["path"], "/workspace/nested/file.txt");
    }

    #[test]
    fn split_output_places_the_omission_marker_before_the_tail() {
        let chunk = |offset: u64, stream: OutputStream, text: &str| OutputChunk {
            offset,
            stream,
            data: text.as_bytes().to_vec().into(),
        };
        let shown = ShownOutput::from_pages(
            OutputPage {
                chunks: vec![
                    chunk(0, OutputStream::Stdout, "head"),
                    chunk(4, OutputStream::Stderr, "warn"),
                ],
                next: 8,
                end: 60,
                gaps: vec![],
            },
            Some(OutputPage {
                chunks: vec![chunk(56, OutputStream::Stdout, "tail")],
                next: 60,
                end: 60,
                gaps: vec![],
            }),
        );
        let (stdout, stderr) = split_output(&shown);
        assert_eq!(stdout.bytes, b"headtail");
        assert_eq!(stdout.omitted_at, Some(4));
        assert_eq!(stderr.omitted_at, None);
        assert_eq!(shown.omitted_bytes(), 48);
    }
}

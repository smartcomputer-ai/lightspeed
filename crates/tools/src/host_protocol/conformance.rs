//! Reusable conformance checks for host data-plane implementations.

use std::sync::Arc;

use engine::storage::InMemoryBlobStore;
use host_client::{HostClientError, HostDataClient, JsonRpcTransport};
use host_protocol::{
    data::handshake::{InitializeParams, InitializedParams},
    shared::{CURRENT_PROTOCOL_VERSION, HostCapabilities},
};
use thiserror::Error;

use crate::{
    error::ToolError,
    fs::{
        CopyOptions, CreateDirectoryOptions, FileAccessPolicy, FsError, FsPath, FsPathError,
        RemoveOptions,
        tools::{ReadFileArgs, WriteFileArgs, invoke_read_file, invoke_write_file},
    },
    host_protocol::RemoteHostConnection,
};

#[derive(Clone, Debug)]
pub struct HostDataConformanceOptions {
    pub initialize: InitializeParams,
    pub expected_capabilities: HostCapabilities,
    pub expected_default_cwd: FsPath,
    /// A relative directory that the suite may create and remove.
    pub test_directory: FsPath,
    /// An absolute path outside the provider's exposed filesystem route.
    pub forbidden_path: FsPath,
}

#[derive(Debug, Error)]
pub enum HostDataConformanceError {
    #[error(transparent)]
    Client(#[from] HostClientError),

    #[error(transparent)]
    Filesystem(#[from] FsError),

    #[error(transparent)]
    Path(#[from] FsPathError),

    #[error(transparent)]
    Tool(#[from] ToolError),

    #[error("host data conformance check failed: {0}")]
    Check(String),
}

/// Exercises the transport handshake and the generic filesystem adapter.
///
/// Provider implementations can invoke this from their own integration tests.
/// The supplied test directory is removed after a successful check.
pub async fn assert_host_data_conformance<T>(
    mut client: HostDataClient<T>,
    options: HostDataConformanceOptions,
) -> Result<(), HostDataConformanceError>
where
    T: JsonRpcTransport + Send + 'static,
{
    if options.test_directory.is_absolute()
        || options.test_directory.is_root()
        || options.test_directory.has_unresolved_parent()
        || options.test_directory.segments().count() != 1
    {
        return Err(check(
            "test_directory must be a single relative path segment",
        ));
    }
    if !options.forbidden_path.is_absolute() {
        return Err(check("forbidden_path must be absolute"));
    }
    if !options.expected_capabilities.filesystem_read
        || !options.expected_capabilities.filesystem_write
    {
        return Err(check(
            "filesystem conformance requires read and write capabilities",
        ));
    }

    let initialized = client.initialize(&options.initialize).await?;
    if initialized.protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(check(format!(
            "protocol version was {}, expected {CURRENT_PROTOCOL_VERSION}",
            initialized.protocol_version
        )));
    }
    if initialized.capabilities != options.expected_capabilities {
        return Err(check(format!(
            "initialize capabilities were {:?}, expected {:?}",
            initialized.capabilities, options.expected_capabilities
        )));
    }
    if initialized.default_cwd.as_deref() != Some(options.expected_default_cwd.as_str()) {
        return Err(check(format!(
            "default cwd was {:?}, expected {}",
            initialized.default_cwd, options.expected_default_cwd
        )));
    }
    client.initialized(&InitializedParams {}).await?;

    let connection = RemoteHostConnection::new(client, initialized.capabilities)
        .with_cwd(options.expected_default_cwd.clone());
    if connection.process_executor().is_some() != options.expected_capabilities.process_start {
        return Err(check(
            "process executor did not follow process_start capability",
        ));
    }
    let has_any_job_capability = options.expected_capabilities.job_start
        || options.expected_capabilities.job_list
        || options.expected_capabilities.job_read
        || options.expected_capabilities.job_cancel;
    if connection.job_executor().is_some() != has_any_job_capability {
        return Err(check("job executor did not follow advertised capabilities"));
    }

    let (fs_context, _) = connection.into_contexts(Arc::new(InMemoryBlobStore::new()));
    if fs_context.fs_cwd.as_ref() != Some(&options.expected_default_cwd) {
        return Err(check("filesystem tool context did not retain default cwd"));
    }
    if fs_context.fs.access_policy() != FileAccessPolicy::FullReadWrite {
        return Err(check(
            "writable host filesystem was not exposed as read-write",
        ));
    }

    let nested = options.test_directory.join("nested")?;
    let source = nested.join("source.txt")?;
    let copy = nested.join("copy.txt")?;
    let copied_tree = FsPath::new(format!("{}-copy", options.test_directory))?;
    let contents = "host data conformance\n";

    let write = invoke_write_file(
        &fs_context,
        WriteFileArgs {
            path: source.clone(),
            content: contents.to_owned(),
        },
    )
    .await?;
    let expected_source = options.expected_default_cwd.join_path(&source)?;
    if write.resolved_path != expected_source {
        return Err(check(format!(
            "relative tool path resolved to {}, expected {expected_source}",
            write.resolved_path
        )));
    }

    let read = invoke_read_file(
        &fs_context,
        ReadFileArgs {
            path: source.clone(),
            offset: None,
            limit: None,
        },
    )
    .await?;
    if read.text != contents.trim_end() {
        return Err(check(
            "read_file did not return bytes written by write_file",
        ));
    }

    let source_metadata = fs_context.fs.get_metadata(&expected_source).await?;
    if !source_metadata.is_file || source_metadata.is_directory {
        return Err(check("file metadata did not identify a regular file"));
    }
    let expected_nested = options.expected_default_cwd.join_path(&nested)?;
    let directory_metadata = fs_context.fs.get_metadata(&expected_nested).await?;
    if !directory_metadata.is_directory || directory_metadata.is_file {
        return Err(check("directory metadata did not identify a directory"));
    }
    if !matches!(
        fs_context
            .fs
            .create_directory(&expected_nested, CreateDirectoryOptions::single())
            .await,
        Err(FsError::AlreadyExists { .. })
    ) {
        return Err(check(
            "provider conflict did not map to FsError::AlreadyExists",
        ));
    }
    let entries = fs_context.fs.read_directory(&expected_nested).await?;
    if !entries
        .iter()
        .any(|entry| entry.file_name == "source.txt" && entry.is_file)
    {
        return Err(check("read_directory omitted the written file"));
    }

    let expected_copy = options.expected_default_cwd.join_path(&copy)?;
    fs_context
        .fs
        .copy(&expected_source, &expected_copy, CopyOptions::file())
        .await?;
    if fs_context.fs.read_file(&expected_copy).await? != contents.as_bytes() {
        return Err(check("file copy did not preserve contents"));
    }

    let expected_test_directory = options
        .expected_default_cwd
        .join_path(&options.test_directory)?;
    let expected_copied_tree = options.expected_default_cwd.join_path(&copied_tree)?;
    fs_context
        .fs
        .copy(
            &expected_test_directory,
            &expected_copied_tree,
            CopyOptions::recursive(),
        )
        .await?;
    let copied_source = expected_copied_tree.join("nested/source.txt")?;
    if fs_context.fs.read_file(&copied_source).await? != contents.as_bytes() {
        return Err(check("recursive directory copy did not preserve contents"));
    }

    let missing = options
        .expected_default_cwd
        .join("missing-conformance-file")?;
    if !matches!(
        fs_context.fs.read_file(&missing).await,
        Err(FsError::NotFound { .. })
    ) {
        return Err(check("provider notFound did not map to FsError::NotFound"));
    }
    if !matches!(
        fs_context.fs.read_file(&options.forbidden_path).await,
        Err(FsError::PermissionDenied { .. })
    ) {
        return Err(check(
            "route escape did not map to FsError::PermissionDenied",
        ));
    }

    fs_context
        .fs
        .remove(&expected_copy, RemoveOptions::file())
        .await?;
    fs_context
        .fs
        .remove(&expected_copied_tree, RemoveOptions::recursive())
        .await?;
    fs_context
        .fs
        .remove(&expected_test_directory, RemoveOptions::recursive())
        .await?;
    if !matches!(
        fs_context.fs.get_metadata(&expected_test_directory).await,
        Err(FsError::NotFound { .. })
    ) {
        return Err(check("recursive remove left the test directory behind"));
    }

    Ok(())
}

fn check(message: impl Into<String>) -> HostDataConformanceError {
    HostDataConformanceError::Check(message.into())
}

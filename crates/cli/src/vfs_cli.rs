use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api_client::HttpAgentApi;
use crate::vfs_transfer::{
    DEFAULT_HAS_MANY_MAX_REFS, DEFAULT_MAX_DEPTH, DEFAULT_MAX_FILES, DEFAULT_MAX_SINGLE_FILE_BYTES,
    DEFAULT_MAX_TOTAL_BYTES, DEFAULT_PUT_MANY_MAX_BATCH_BYTES, DEFAULT_PUT_MANY_MAX_BATCH_FILES,
    SnapshotUploadOptions, materialize_snapshot, upload_snapshot_directory,
};

#[derive(Args, Debug, Clone)]
pub(crate) struct VfsArgs {
    #[command(subcommand)]
    command: VfsCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum VfsCommand {
    /// Upload a local directory as a CAS-backed VFS snapshot.
    Snapshot(SnapshotArgs),
    /// Materialize a CAS-backed VFS snapshot into a local directory.
    Materialize(MaterializeArgs),
}

#[derive(Args, Debug, Clone)]
struct SnapshotArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the snapshot summary as JSON.
    #[arg(long)]
    json: bool,
    /// Max raw bytes per blob/put_many request.
    #[arg(long = "put-batch-bytes", default_value_t = DEFAULT_PUT_MANY_MAX_BATCH_BYTES)]
    put_batch_bytes: u64,
    /// Max blobs per blob/put_many request.
    #[arg(long = "put-batch-files", default_value_t = DEFAULT_PUT_MANY_MAX_BATCH_FILES)]
    put_batch_files: usize,
    /// Max refs per blob/has_many request.
    #[arg(long = "has-batch-refs", default_value_t = DEFAULT_HAS_MANY_MAX_REFS)]
    has_batch_refs: usize,
    /// Max file count in the snapshot.
    #[arg(long = "max-files", default_value_t = DEFAULT_MAX_FILES)]
    max_files: u64,
    /// Max total file bytes in the snapshot.
    #[arg(long = "max-total-bytes", default_value_t = DEFAULT_MAX_TOTAL_BYTES)]
    max_total_bytes: u64,
    /// Max single file bytes in the snapshot.
    #[arg(long = "max-file-bytes", default_value_t = DEFAULT_MAX_SINGLE_FILE_BYTES)]
    max_file_bytes: u64,
    /// Max VFS path depth in the snapshot.
    #[arg(long = "max-depth", default_value_t = DEFAULT_MAX_DEPTH)]
    max_depth: usize,
    /// Local directory to snapshot.
    directory: PathBuf,
}

#[derive(Args, Debug, Clone)]
struct MaterializeArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the materialization summary as JSON.
    #[arg(long)]
    json: bool,
    /// Snapshot ref to materialize.
    snapshot_ref: String,
    /// Local destination directory.
    destination: PathBuf,
}

pub(crate) async fn handle(args: VfsArgs) -> Result<()> {
    match args.command {
        VfsCommand::Snapshot(args) => snapshot(args).await,
        VfsCommand::Materialize(args) => materialize(args).await,
    }
}

async fn snapshot(args: SnapshotArgs) -> Result<()> {
    let options = snapshot_options(&args);
    let api = HttpAgentApi::new(args.api_url);
    let summary = upload_snapshot_directory(&api, args.directory, options).await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!("snapshotRef {}", summary.snapshot_ref);
    println!("files {}", summary.files);
    println!("bytes {}", summary.bytes);
    println!("uploadedBlobs {}", summary.uploaded_blobs);
    println!("uploadedBytes {}", summary.uploaded_bytes);
    println!("reusedBlobs {}", summary.reused_blobs);
    println!("reusedBytes {}", summary.reused_bytes);
    if summary.skipped_paths > 0 {
        println!("skippedPaths {}", summary.skipped_paths);
        for warning in summary.warnings {
            println!("warning {}: {}", warning.path, warning.message);
        }
    }
    Ok(())
}

async fn materialize(args: MaterializeArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let summary = materialize_snapshot(&api, args.snapshot_ref, args.destination).await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!("snapshotRef {}", summary.snapshot_ref);
    println!("destination {}", summary.destination);
    println!("files {}", summary.files);
    println!("bytes {}", summary.bytes);
    println!("directories {}", summary.directories);
    println!("createdDirectories {}", summary.created_directories);
    println!("writtenFiles {}", summary.written_files);
    println!("writtenBytes {}", summary.written_bytes);
    println!("reusedFiles {}", summary.reused_files);
    println!("reusedBytes {}", summary.reused_bytes);
    println!("downloadedBlobs {}", summary.downloaded_blobs);
    println!("downloadedBytes {}", summary.downloaded_bytes);
    Ok(())
}

fn snapshot_options(args: &SnapshotArgs) -> SnapshotUploadOptions {
    SnapshotUploadOptions {
        limits: vfs::VfsSnapshotLimits::new(
            args.max_files,
            args.max_total_bytes,
            args.max_file_bytes,
            args.max_depth,
        ),
        max_put_many_batch_bytes: args.put_batch_bytes,
        max_put_many_batch_files: args.put_batch_files,
        max_has_many_refs: args.has_batch_refs,
    }
}

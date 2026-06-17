//! Canonical write-file operation.

use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    fs::{CreateDirectoryOptions, FsPath, FsToolContext},
};

use super::resolve_path;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WriteFileArgs {
    pub path: FsPath,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WriteFileResult {
    pub path: FsPath,
    pub resolved_path: FsPath,
    pub bytes_written: usize,
}

pub async fn invoke_write_file(
    ctx: &FsToolContext,
    args: WriteFileArgs,
) -> ToolResult<WriteFileResult> {
    let resolved_path = resolve_path(ctx, &args.path)?;
    if let Some(parent) = resolved_path.parent()
        && !parent.is_root()
    {
        ctx.fs
            .create_directory(&parent, CreateDirectoryOptions::recursive())
            .await?;
    }

    let bytes = args.content.into_bytes();
    let bytes_written = bytes.len();
    ctx.fs.write_file(&resolved_path, bytes).await?;

    Ok(WriteFileResult {
        path: args.path,
        resolved_path,
        bytes_written,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::storage::InMemoryBlobStore;

    use super::*;
    use crate::{
        error::ToolError,
        fs::{FileSystem, InMemoryFileSystem},
    };

    fn context(fs: Arc<dyn FileSystem>) -> FsToolContext {
        FsToolContext::new(fs, Arc::new(InMemoryBlobStore::new()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_write_file_resolves_relative_paths_and_creates_parents() {
        let fs = InMemoryFileSystem::full_access();
        let ctx = context(Arc::new(fs.clone())).with_cwd(FsPath::new("/workspace").expect("cwd"));

        let result = invoke_write_file(
            &ctx,
            WriteFileArgs {
                path: FsPath::new("nested/file.txt").expect("relative path"),
                content: "hello".to_string(),
            },
        )
        .await
        .expect("write file");

        assert_eq!(
            result.resolved_path,
            FsPath::new("/workspace/nested/file.txt").unwrap()
        );
        assert_eq!(result.bytes_written, 5);
        assert_eq!(
            fs.read_file_text(&FsPath::new("/workspace/nested/file.txt").unwrap())
                .await
                .expect("read file"),
            "hello"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_write_file_propagates_read_only_policy_errors() {
        let fs = InMemoryFileSystem::new(crate::fs::FileAccessPolicy::FullReadOnly);
        let ctx = context(Arc::new(fs));

        let error = invoke_write_file(
            &ctx,
            WriteFileArgs {
                path: FsPath::new("/file.txt").expect("path"),
                content: "hello".to_string(),
            },
        )
        .await
        .expect_err("write should fail");

        assert!(matches!(error, ToolError::Filesystem(_)));
    }
}

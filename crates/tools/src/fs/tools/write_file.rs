//! Canonical write-file operation.

use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    fs::{CreateDirectoryOptions, FsPath, FsToolContext},
};

use super::resolve_path;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteFileArgs {
    pub path: FsPath,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub content: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub content_ref: Option<crate::content::ContentReference>,
}

pub(crate) fn present_value<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WriteFileResult {
    pub path: FsPath,
    pub resolved_path: FsPath,
    pub bytes_written: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<environment_protocol::data::transfer_session::TransferStatus>,
}

impl WriteFileArgs {
    pub fn validate(&self) -> ToolResult<()> {
        if self.content.is_some() == self.content_ref.is_some() {
            return Err(super::invalid_request(
                "provide exactly one of content or content_ref",
            ));
        }
        Ok(())
    }
}

pub(crate) async fn invoke_builtin_write_file(
    ctx: crate::builtin::BuiltinToolContext<'_>,
    args: WriteFileArgs,
) -> ToolResult<WriteFileResult> {
    args.validate()?;
    if args.content_ref.is_some()
        && let crate::builtin::BuiltinToolContext::Environment(environment) = ctx
    {
        return crate::transfer::invoke_write_reference(environment, args).await;
    }
    invoke_write_file(ctx.filesystem()?, args).await
}

pub async fn invoke_write_file(
    ctx: &FsToolContext,
    args: WriteFileArgs,
) -> ToolResult<WriteFileResult> {
    args.validate()?;
    let resolved_path = resolve_path(ctx, &args.path)?;
    // Validate and retain the source before creating any destination directories.
    let reference = match &args.content_ref {
        Some(reference) => Some(ctx.content_resolver.resolve(reference).await?),
        None => None,
    };
    if let Some(parent) = resolved_path.parent()
        && !parent.is_root()
    {
        ctx.fs
            .create_directory(&parent, CreateDirectoryOptions::recursive())
            .await?;
    }

    let bytes_written = if let Some(reference) = reference {
        let byte_len = usize::try_from(reference.byte_len)
            .map_err(|_| super::invalid_request("file size exceeds platform limits"))?;
        ctx.fs.write_file_ref(&resolved_path, &reference).await?;
        byte_len
    } else {
        let bytes = args.content.unwrap().into_bytes();
        let byte_len = bytes.len();
        ctx.fs.write_file(&resolved_path, bytes).await?;
        byte_len
    };

    Ok(WriteFileResult {
        path: args.path,
        resolved_path,
        bytes_written,
        receipt: None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use harness::storage::InMemoryBlobStore;

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
                content: Some("hello".to_string()),
                content_ref: None,
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
                content: Some("hello".to_string()),
                content_ref: None,
            },
        )
        .await
        .expect_err("write should fail");

        assert!(matches!(error, ToolError::Filesystem(_)));
    }
}

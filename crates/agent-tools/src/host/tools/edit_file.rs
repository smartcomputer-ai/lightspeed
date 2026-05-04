//! Canonical edit-file operation.

use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    host::{
        context::HostToolContext,
        fs::{FsError, FsPath},
    },
};

use super::{invalid_request, resolve_path};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EditFileArgs {
    pub path: FsPath,
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EditFileResult {
    pub path: FsPath,
    pub resolved_path: FsPath,
    pub replacements: usize,
    pub bytes_written: usize,
}

pub async fn invoke_edit_file(
    ctx: &HostToolContext,
    args: EditFileArgs,
) -> ToolResult<EditFileResult> {
    if args.old_string.is_empty() {
        return Err(invalid_request("edit_file old_string must not be empty"));
    }

    let resolved_path = resolve_path(ctx, &args.path)?;
    let contents = ctx.fs.read_file(&resolved_path).await?;
    let contents = String::from_utf8(contents).map_err(FsError::invalid_data)?;
    let replacements = contents.matches(&args.old_string).count();

    if replacements == 0 {
        return Err(invalid_request(format!(
            "edit_file old_string was not found in {}",
            resolved_path
        )));
    }

    if replacements > 1 && !args.replace_all {
        return Err(invalid_request(format!(
            "edit_file old_string matched {replacements} times in {}; set replace_all=true to replace every match",
            resolved_path
        )));
    }

    let updated = if args.replace_all {
        contents.replace(&args.old_string, &args.new_string)
    } else {
        contents.replacen(&args.old_string, &args.new_string, 1)
    };
    let bytes = updated.into_bytes();
    let bytes_written = bytes.len();
    ctx.fs.write_file(&resolved_path, bytes).await?;

    Ok(EditFileResult {
        path: args.path,
        resolved_path,
        replacements: if args.replace_all { replacements } else { 1 },
        bytes_written,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agent_core::storage::InMemoryBlobStore;

    use super::*;
    use crate::{
        error::ToolError,
        host::fs::{CreateDirectoryOptions, FileSystem, InMemoryFileSystem},
    };

    fn context(fs: Arc<dyn FileSystem>) -> HostToolContext {
        HostToolContext::new(fs, None, Arc::new(InMemoryBlobStore::new()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_edit_file_replaces_unique_match() {
        let fs = InMemoryFileSystem::full_access();
        fs.create_directory(
            &FsPath::new("/workspace").expect("workspace"),
            CreateDirectoryOptions::single(),
        )
        .await
        .expect("create workspace");
        fs.write_file(
            &FsPath::new("/workspace/file.txt").expect("file"),
            b"hello world\n".to_vec(),
        )
        .await
        .expect("write file");
        let ctx = context(Arc::new(fs.clone())).with_cwd(FsPath::new("/workspace").expect("cwd"));

        let result = invoke_edit_file(
            &ctx,
            EditFileArgs {
                path: FsPath::new("file.txt").expect("path"),
                old_string: "world".to_string(),
                new_string: "forge".to_string(),
                replace_all: false,
            },
        )
        .await
        .expect("edit file");

        assert_eq!(result.replacements, 1);
        assert_eq!(
            fs.read_file_text(&FsPath::new("/workspace/file.txt").unwrap())
                .await
                .expect("read file"),
            "hello forge\n"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_edit_file_rejects_ambiguous_match_without_replace_all() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(
            &FsPath::new("/file.txt").expect("file"),
            b"one one".to_vec(),
        )
        .await
        .expect("write file");
        let ctx = context(Arc::new(fs));

        let error = invoke_edit_file(
            &ctx,
            EditFileArgs {
                path: FsPath::new("/file.txt").expect("path"),
                old_string: "one".to_string(),
                new_string: "two".to_string(),
                replace_all: false,
            },
        )
        .await
        .expect_err("edit should fail");

        assert!(matches!(error, ToolError::InvalidRequest { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_edit_file_replaces_all_matches_when_requested() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(
            &FsPath::new("/file.txt").expect("file"),
            b"one one".to_vec(),
        )
        .await
        .expect("write file");
        let ctx = context(Arc::new(fs.clone()));

        let result = invoke_edit_file(
            &ctx,
            EditFileArgs {
                path: FsPath::new("/file.txt").expect("path"),
                old_string: "one".to_string(),
                new_string: "two".to_string(),
                replace_all: true,
            },
        )
        .await
        .expect("edit file");

        assert_eq!(result.replacements, 2);
        assert_eq!(
            fs.read_file_text(&FsPath::new("/file.txt").unwrap())
                .await
                .expect("read file"),
            "two two"
        );
    }
}

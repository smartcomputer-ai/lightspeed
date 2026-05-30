//! Canonical read-file operation.

use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    host::{
        context::HostToolContext,
        fs::{FsError, FsPath},
    },
};

use super::{invalid_request, resolve_path};

pub const DEFAULT_READ_FILE_LINE_LIMIT: usize = 10_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadFileArgs {
    pub path: FsPath,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadFileResult {
    pub path: FsPath,
    pub resolved_path: FsPath,
    pub text: String,
    pub line_numbered_text: String,
    pub line_start: usize,
    pub line_count: usize,
    pub total_lines: usize,
    pub truncated: bool,
    pub bytes_read: usize,
}

pub async fn invoke_read_file(
    ctx: &HostToolContext,
    args: ReadFileArgs,
) -> ToolResult<ReadFileResult> {
    let resolved_path = resolve_path(ctx, &args.path)?;
    let offset = args.offset.unwrap_or(1);
    if offset == 0 {
        return Err(invalid_request("read_file offset must be 1 or greater"));
    }
    let limit = args.limit.unwrap_or(DEFAULT_READ_FILE_LINE_LIMIT);
    if limit == 0 {
        return Err(invalid_request("read_file limit must be 1 or greater"));
    }

    let bytes = ctx.fs.read_file(&resolved_path).await?;
    let bytes_read = bytes.len();
    if bytes_read as u64 > ctx.limits.max_file_read_bytes {
        return Err(invalid_request(format!(
            "read_file read {bytes_read} bytes, exceeding max_file_read_bytes={}",
            ctx.limits.max_file_read_bytes
        )));
    }

    let contents = String::from_utf8(bytes).map_err(FsError::invalid_data)?;
    let lines = contents.lines().collect::<Vec<_>>();
    let total_lines = lines.len();
    let start_index = offset - 1;
    let selected = lines
        .iter()
        .enumerate()
        .skip(start_index)
        .take(limit)
        .map(|(index, line)| (index + 1, *line))
        .collect::<Vec<_>>();

    let text = selected
        .iter()
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n");
    let line_numbered_text = selected
        .iter()
        .map(|(line_number, line)| format!("{line_number:>6} | {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let line_count = selected.len();
    let truncated = start_index < total_lines && start_index + line_count < total_lines;

    Ok(ReadFileResult {
        path: args.path,
        resolved_path,
        text,
        line_numbered_text,
        line_start: offset,
        line_count,
        total_lines,
        truncated,
        bytes_read,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::storage::InMemoryBlobStore;

    use super::*;
    use crate::{
        error::ToolError,
        host::{
            context::HostToolLimits,
            fs::{CreateDirectoryOptions, FileSystem, InMemoryFileSystem},
        },
    };

    fn context(fs: Arc<dyn FileSystem>) -> HostToolContext {
        HostToolContext::new(fs, None, Arc::new(InMemoryBlobStore::new()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_read_file_resolves_relative_paths_against_context_cwd() {
        let fs = InMemoryFileSystem::full_access();
        fs.create_directory(
            &FsPath::new("/workspace").expect("dir"),
            CreateDirectoryOptions::single(),
        )
        .await
        .expect("create directory");
        fs.write_file(
            &FsPath::new("/workspace/file.txt").expect("file path"),
            b"hello".to_vec(),
        )
        .await
        .expect("write file");
        let ctx = context(Arc::new(fs)).with_cwd(FsPath::new("/workspace").expect("cwd"));

        let result = invoke_read_file(
            &ctx,
            ReadFileArgs {
                path: FsPath::new("file.txt").expect("relative path"),
                offset: None,
                limit: None,
            },
        )
        .await
        .expect("read file");

        assert_eq!(
            result.resolved_path,
            FsPath::new("/workspace/file.txt").unwrap()
        );
        assert_eq!(result.text, "hello");
        assert_eq!(result.line_numbered_text, "     1 | hello");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_read_file_applies_offset_and_limit() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(
            &FsPath::new("/file.txt").expect("file path"),
            b"one\ntwo\nthree\nfour".to_vec(),
        )
        .await
        .expect("write file");
        let ctx = context(Arc::new(fs));

        let result = invoke_read_file(
            &ctx,
            ReadFileArgs {
                path: FsPath::new("/file.txt").expect("path"),
                offset: Some(2),
                limit: Some(2),
            },
        )
        .await
        .expect("read file");

        assert_eq!(result.text, "two\nthree");
        assert_eq!(result.line_numbered_text, "     2 | two\n     3 | three");
        assert_eq!(result.line_start, 2);
        assert_eq!(result.line_count, 2);
        assert_eq!(result.total_lines, 4);
        assert!(result.truncated);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_read_file_enforces_max_file_read_bytes() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(
            &FsPath::new("/file.txt").expect("file path"),
            b"hello".to_vec(),
        )
        .await
        .expect("write file");
        let ctx = context(Arc::new(fs)).with_limits(HostToolLimits {
            max_file_read_bytes: 4,
            ..HostToolLimits::default()
        });

        let error = invoke_read_file(
            &ctx,
            ReadFileArgs {
                path: FsPath::new("/file.txt").expect("path"),
                offset: None,
                limit: None,
            },
        )
        .await
        .expect_err("read should fail");

        assert!(matches!(error, ToolError::InvalidRequest { .. }));
    }
}

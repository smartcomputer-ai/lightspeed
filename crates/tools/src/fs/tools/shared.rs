//! Shared helpers for generic filesystem tool operations.

use crate::{
    error::{ToolError, ToolResult},
    fs::{FsError, FsPath, FsToolContext},
};

pub(crate) fn resolve_path(ctx: &FsToolContext, path: &FsPath) -> ToolResult<FsPath> {
    if path.is_absolute() {
        return Ok(path.clone());
    }

    let Some(cwd) = &ctx.fs_cwd else {
        return Ok(path.clone());
    };

    cwd.join_path(path)
        .map_err(FsError::from)
        .map_err(ToolError::from)
}

pub(crate) fn invalid_request(message: impl Into<String>) -> ToolError {
    ToolError::InvalidRequest {
        message: message.into(),
    }
}

pub(crate) async fn collect_file_paths(
    ctx: &FsToolContext,
    root: FsPath,
    max_depth: Option<usize>,
) -> ToolResult<Vec<FsPath>> {
    let metadata = ctx.fs.get_metadata(&root).await?;
    if metadata.is_file {
        return Ok(vec![root]);
    }
    if !metadata.is_directory {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    let mut stack = vec![(root, 0usize)];

    while let Some((dir, depth)) = stack.pop() {
        let mut entries = ctx.fs.read_directory(&dir).await?;
        entries.sort_by(|left, right| right.file_name.cmp(&left.file_name));

        for entry in entries {
            let child = dir.join(&entry.file_name).map_err(FsError::from)?;
            if entry.is_file {
                files.push(child);
            } else if entry.is_directory && max_depth.is_none_or(|max_depth| depth < max_depth) {
                stack.push((child, depth + 1));
            }
        }
    }

    files.sort();
    Ok(files)
}

pub(crate) fn relative_path_string(path: &FsPath, root: &FsPath) -> String {
    if !path.starts_with(root) {
        return path.as_str().to_string();
    }

    let root_segment_count = root.segments().count();
    let relative = path
        .segments()
        .skip(root_segment_count)
        .collect::<Vec<_>>()
        .join("/");
    if relative.is_empty() {
        ".".to_string()
    } else {
        relative
    }
}

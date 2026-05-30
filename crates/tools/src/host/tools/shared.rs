//! Shared helpers for host tool surfaces and operations.

use serde_json::{Value, json};

use crate::{
    error::{ToolError, ToolResult},
    host::{context::HostToolContext, fs::FsPath, process::ProcessOutput},
};

pub(super) fn visible_with_truncation(mut visible: String, truncated: bool) -> String {
    if truncated {
        if !visible.is_empty() {
            visible.push('\n');
        }
        visible.push_str("[truncated]");
    }
    visible
}

pub(super) fn process_visible_output(output: &ProcessOutput) -> String {
    let stdout = output.stdout.text_lossy();
    let stderr = output.stderr.text_lossy();
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}\n{stderr}"),
        (false, true) => stdout,
        (true, false) => stderr,
        (true, true) => format!("process status: {:?}", output.status),
    }
}

pub(super) fn object<const N: usize, const M: usize>(
    properties: [(&'static str, Value); N],
    required: [&'static str; M],
) -> Value {
    let properties = properties
        .into_iter()
        .map(|(name, schema)| (name.to_string(), schema))
        .collect::<serde_json::Map<_, _>>();
    let required = required.into_iter().collect::<Vec<_>>();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

pub(super) fn string(description: &'static str) -> Value {
    json!({ "type": "string", "description": description })
}

pub(super) fn nullable_string(description: &'static str) -> Value {
    json!({ "type": ["string", "null"], "description": description })
}

pub(super) fn integer(description: &'static str) -> Value {
    json!({ "type": "integer", "minimum": 0, "description": description })
}

pub(super) fn nullable_integer(description: &'static str) -> Value {
    json!({ "anyOf": [integer(description), { "type": "null" }] })
}

pub(super) fn boolean(description: &'static str) -> Value {
    json!({ "type": "boolean", "description": description })
}

pub(super) fn optional_boolean(description: &'static str) -> Value {
    json!({ "type": ["boolean", "null"], "description": description })
}

pub(super) fn optional_enum<const N: usize>(
    description: &'static str,
    values: [&'static str; N],
) -> Value {
    let values = values.into_iter().collect::<Vec<_>>();
    json!({
        "anyOf": [
            { "type": "string", "enum": values },
            { "type": "null" }
        ],
        "description": description
    })
}

pub(super) fn array_of_strings(description: &'static str) -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": description
    })
}

pub(super) fn string_map(description: &'static str) -> Value {
    json!({
        "type": "object",
        "additionalProperties": { "type": "string" },
        "description": description
    })
}

pub(crate) fn resolve_path(ctx: &HostToolContext, path: &FsPath) -> ToolResult<FsPath> {
    if path.is_absolute() {
        return Ok(path.clone());
    }

    let Some(cwd) = &ctx.cwd else {
        return Ok(path.clone());
    };

    cwd.join_path(path)
        .map_err(crate::host::fs::FsError::from)
        .map_err(ToolError::from)
}

pub(crate) fn invalid_request(message: impl Into<String>) -> ToolError {
    ToolError::InvalidRequest {
        message: message.into(),
    }
}

pub(crate) fn unsupported_capability(message: impl Into<String>) -> ToolError {
    ToolError::UnsupportedCapability {
        message: message.into(),
    }
}

pub(crate) async fn collect_file_paths(
    ctx: &HostToolContext,
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
            let child = dir
                .join(&entry.file_name)
                .map_err(crate::host::fs::FsError::from)?;
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

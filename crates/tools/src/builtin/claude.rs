//! Claude Code-like built-in tool surface.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::{
    environment::tools::{RunProcessArgs, invoke_run_process},
    error::{ToolError, ToolResult},
    fs::{
        FsPath,
        tools::{
            EditFileArgs, GlobArgs, GrepArgs, GrepResult, ReadFileArgs, WriteFileArgs,
            invoke_edit_file, invoke_glob, invoke_grep, invoke_read_file, invoke_write_file,
        },
    },
    runtime::{ToolInvocationOutput, decode_args, encode_output},
    targets::ResolvedToolContext,
};

use super::{
    BuiltinToolOperation, canonical,
    shared::{
        invalid_request, nullable_integer, nullable_string, object, optional_boolean,
        optional_enum, process_visible_output, string, visible_with_truncation,
    },
};

pub(super) fn description(
    operation: BuiltinToolOperation,
    scoped_paths: bool,
) -> ToolResult<String> {
    let path_guidance = if scoped_paths {
        " Paths are resolved within the configured filesystem scope."
    } else {
        ""
    };
    let text = match operation {
        BuiltinToolOperation::ReadFile => "Reads a file from the filesystem.",
        BuiltinToolOperation::WriteFile => "Writes a file to the filesystem.",
        BuiltinToolOperation::EditFile => "Performs exact string replacements in a file.",
        BuiltinToolOperation::Grep => "Searches file contents with a regular expression.",
        BuiltinToolOperation::Glob => "Finds files by glob pattern.",
        BuiltinToolOperation::RunProcess => "Executes a shell command.",
        BuiltinToolOperation::JobStart
        | BuiltinToolOperation::JobList
        | BuiltinToolOperation::JobRead
        | BuiltinToolOperation::JobWait
        | BuiltinToolOperation::JobCancel => {
            return Ok(canonical::description(operation, scoped_paths));
        }
        BuiltinToolOperation::ApplyPatch
        | BuiltinToolOperation::ListDir
        | BuiltinToolOperation::WriteProcessStdin => return Err(unsupported(operation)),
    };
    Ok(format!("{text}{path_guidance}"))
}

pub(super) fn input_schema(operation: BuiltinToolOperation) -> ToolResult<Value> {
    let schema = match operation {
        BuiltinToolOperation::ReadFile => object(
            [
                (
                    "file_path",
                    string("The absolute path to the file to read."),
                ),
                (
                    "offset",
                    nullable_integer("The line number to start reading from."),
                ),
                ("limit", nullable_integer("The number of lines to read.")),
                (
                    "pages",
                    nullable_string("Page range for PDF files. Ignored by Lightspeed tools."),
                ),
            ],
            ["file_path"],
        ),
        BuiltinToolOperation::WriteFile => object(
            [
                (
                    "file_path",
                    string("The absolute path to the file to write."),
                ),
                ("content", string("The content to write to the file.")),
            ],
            ["file_path", "content"],
        ),
        BuiltinToolOperation::EditFile => object(
            [
                (
                    "file_path",
                    string("The absolute path to the file to modify."),
                ),
                ("old_string", string("The text to replace.")),
                (
                    "new_string",
                    string("The text to replace it with. Must be different from old_string."),
                ),
                (
                    "replace_all",
                    optional_boolean("Replace all occurrences of old_string. Defaults to false."),
                ),
            ],
            ["file_path", "old_string", "new_string"],
        ),
        BuiltinToolOperation::Grep => object(
            [
                (
                    "pattern",
                    string("The regular expression pattern to search for in file contents."),
                ),
                (
                    "path",
                    nullable_string("File or directory to search in. Defaults to cwd."),
                ),
                (
                    "glob",
                    nullable_string("Glob pattern to filter files, such as \"*.rs\"."),
                ),
                (
                    "output_mode",
                    optional_enum(
                        "Output mode. Defaults to files_with_matches.",
                        ["content", "files_with_matches", "count"],
                    ),
                ),
                (
                    "-B",
                    nullable_integer(
                        "Number of lines to show before each match. Parsed but not yet applied.",
                    ),
                ),
                (
                    "-A",
                    nullable_integer(
                        "Number of lines to show after each match. Parsed but not yet applied.",
                    ),
                ),
                (
                    "-C",
                    nullable_integer(
                        "Number of context lines around each match. Parsed but not yet applied.",
                    ),
                ),
                (
                    "context",
                    nullable_integer(
                        "Number of context lines around each match. Parsed but not yet applied.",
                    ),
                ),
                (
                    "-n",
                    optional_boolean("Show line numbers in content output. Defaults to true."),
                ),
                ("-i", optional_boolean("Case insensitive search.")),
                (
                    "type",
                    nullable_string("File type to search. Parsed but not yet applied."),
                ),
                (
                    "head_limit",
                    nullable_integer("Limit output to first N entries. Pass 0 for unlimited."),
                ),
                (
                    "offset",
                    nullable_integer("Skip first N output entries before applying head_limit."),
                ),
                (
                    "multiline",
                    optional_boolean("Enable multiline mode. Parsed but not yet applied."),
                ),
            ],
            ["pattern"],
        ),
        BuiltinToolOperation::Glob => object(
            [
                (
                    "pattern",
                    string("The glob pattern to match files against."),
                ),
                (
                    "path",
                    nullable_string("The directory to search in. Defaults to cwd."),
                ),
            ],
            ["pattern"],
        ),
        BuiltinToolOperation::RunProcess => object(
            [
                ("command", string("The command to execute.")),
                (
                    "timeout",
                    nullable_integer("Optional timeout in milliseconds."),
                ),
                (
                    "description",
                    nullable_string("Clear, concise description of what this command does."),
                ),
                (
                    "run_in_background",
                    optional_boolean("Parsed but not supported by Lightspeed tools."),
                ),
                (
                    "dangerouslyDisableSandbox",
                    optional_boolean("Parsed but not supported by Lightspeed tools."),
                ),
            ],
            ["command"],
        ),
        BuiltinToolOperation::JobStart
        | BuiltinToolOperation::JobList
        | BuiltinToolOperation::JobRead
        | BuiltinToolOperation::JobWait
        | BuiltinToolOperation::JobCancel => return Ok(canonical::input_schema(operation)),
        BuiltinToolOperation::ApplyPatch
        | BuiltinToolOperation::ListDir
        | BuiltinToolOperation::WriteProcessStdin => return Err(unsupported(operation)),
    };
    Ok(schema)
}

pub(super) async fn invoke_json(
    operation: BuiltinToolOperation,
    ctx: ResolvedToolContext<'_>,
    arguments: Value,
) -> ToolResult<ToolInvocationOutput> {
    match operation {
        BuiltinToolOperation::ReadFile => {
            let args: ClaudeCodeReadArgs = decode_args(arguments)?;
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_read_file(fs_ctx, args.try_into_read_file_args()?).await?;
            encode_output(&result, result.line_numbered_text.clone())
        }
        BuiltinToolOperation::WriteFile => {
            let args: ClaudeCodeWriteArgs = decode_args(arguments)?;
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_write_file(fs_ctx, args.try_into_write_file_args()?).await?;
            let visible = format!(
                "Wrote {} bytes to {}",
                result.bytes_written, result.resolved_path
            );
            encode_output(&result, visible)
        }
        BuiltinToolOperation::EditFile => {
            let args: ClaudeCodeEditArgs = decode_args(arguments)?;
            let fs_ctx = ctx.filesystem()?;
            if args.old_string.is_empty() {
                let result = invoke_write_file(fs_ctx, args.try_into_write_file_args()?).await?;
                let visible = format!(
                    "Wrote {} bytes to {}",
                    result.bytes_written, result.resolved_path
                );
                return encode_output(&result, visible);
            }

            let result = invoke_edit_file(fs_ctx, args.try_into_edit_file_args()?).await?;
            let visible = format!(
                "Replaced {} match(es) in {}",
                result.replacements, result.resolved_path
            );
            encode_output(&result, visible)
        }
        BuiltinToolOperation::Grep => {
            let args: ClaudeCodeGrepArgs = decode_args(arguments)?;
            let output_mode = args.output_mode()?;
            let show_line_numbers = args.show_line_numbers();
            let offset = args.offset.unwrap_or(0);
            let head_limit = args.head_limit;
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_grep(fs_ctx, args.try_into_grep_args()?).await?;
            let visible = claude_code_grep_visible(
                &result,
                output_mode,
                show_line_numbers,
                offset,
                head_limit,
            );
            encode_output(&result, visible_with_truncation(visible, result.truncated))
        }
        BuiltinToolOperation::Glob => {
            let args: ClaudeCodeGlobArgs = decode_args(arguments)?;
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_glob(fs_ctx, args.try_into_glob_args()?).await?;
            let visible = result
                .matches
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            encode_output(&result, visible_with_truncation(visible, result.truncated))
        }
        BuiltinToolOperation::RunProcess => {
            let args: ClaudeCodeBashArgs = decode_args(arguments)?;
            let env_ctx = ctx.environment()?;
            let result = invoke_run_process(env_ctx, args.into_run_process_args()).await?;
            let visible = process_visible_output(&result);
            encode_output(&result, visible)
        }
        BuiltinToolOperation::JobStart
        | BuiltinToolOperation::JobList
        | BuiltinToolOperation::JobRead
        | BuiltinToolOperation::JobWait
        | BuiltinToolOperation::JobCancel => {
            canonical::invoke_json(operation, ctx, arguments).await
        }
        BuiltinToolOperation::ApplyPatch
        | BuiltinToolOperation::ListDir
        | BuiltinToolOperation::WriteProcessStdin => Err(unsupported(operation)),
    }
}

fn unsupported(operation: BuiltinToolOperation) -> ToolError {
    ToolError::UnsupportedCapability {
        message: format!(
            "ClaudeCodeLike tool surface does not support {}",
            operation.name_for_error()
        ),
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeReadArgs {
    file_path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    #[allow(dead_code)]
    pages: Option<String>,
}

impl ClaudeCodeReadArgs {
    fn try_into_read_file_args(self) -> ToolResult<ReadFileArgs> {
        Ok(ReadFileArgs {
            path: parse_fs_path(self.file_path)?,
            offset: self.offset.map(|offset| offset.max(1)),
            limit: self.limit,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeWriteArgs {
    file_path: String,
    content: String,
}

impl ClaudeCodeWriteArgs {
    fn try_into_write_file_args(self) -> ToolResult<WriteFileArgs> {
        Ok(WriteFileArgs {
            path: parse_fs_path(self.file_path)?,
            content: self.content,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeEditArgs {
    file_path: String,
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
}

impl ClaudeCodeEditArgs {
    fn try_into_edit_file_args(self) -> ToolResult<EditFileArgs> {
        Ok(EditFileArgs {
            path: parse_fs_path(self.file_path)?,
            old_string: self.old_string,
            new_string: self.new_string,
            replace_all: self.replace_all.unwrap_or(false),
        })
    }

    fn try_into_write_file_args(self) -> ToolResult<WriteFileArgs> {
        Ok(WriteFileArgs {
            path: parse_fs_path(self.file_path)?,
            content: self.new_string,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClaudeCodeGrepOutputMode {
    Content,
    FilesWithMatches,
    Count,
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeGrepArgs {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    output_mode: Option<String>,
    #[serde(rename = "-B")]
    before_context: Option<usize>,
    #[serde(rename = "-A")]
    after_context: Option<usize>,
    #[serde(rename = "-C")]
    context_alias: Option<usize>,
    context: Option<usize>,
    #[serde(rename = "-n")]
    line_numbers: Option<bool>,
    #[serde(rename = "-i")]
    case_insensitive: Option<bool>,
    #[serde(rename = "type")]
    file_type: Option<String>,
    head_limit: Option<usize>,
    offset: Option<usize>,
    multiline: Option<bool>,
}

impl ClaudeCodeGrepArgs {
    fn output_mode(&self) -> ToolResult<ClaudeCodeGrepOutputMode> {
        match self.output_mode.as_deref().unwrap_or("files_with_matches") {
            "content" => Ok(ClaudeCodeGrepOutputMode::Content),
            "files_with_matches" => Ok(ClaudeCodeGrepOutputMode::FilesWithMatches),
            "count" => Ok(ClaudeCodeGrepOutputMode::Count),
            value => Err(invalid_request(format!(
                "unsupported Grep output_mode: {value}"
            ))),
        }
    }

    fn show_line_numbers(&self) -> bool {
        self.line_numbers.unwrap_or(true)
    }

    fn try_into_grep_args(self) -> ToolResult<GrepArgs> {
        let _parsed_but_not_applied = (
            self.before_context,
            self.after_context,
            self.context_alias,
            self.context,
            self.file_type,
            self.multiline,
        );
        let limit = match self.head_limit {
            Some(0) => None,
            Some(limit) => Some(limit.saturating_add(self.offset.unwrap_or(0))),
            None => Some(250usize.saturating_add(self.offset.unwrap_or(0))),
        };
        Ok(GrepArgs {
            pattern: self.pattern,
            path: self.path.map(parse_fs_path).transpose()?,
            include: self.glob,
            case_sensitive: !self.case_insensitive.unwrap_or(false),
            max_depth: None,
            limit,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeGlobArgs {
    pattern: String,
    path: Option<String>,
}

impl ClaudeCodeGlobArgs {
    fn try_into_glob_args(self) -> ToolResult<GlobArgs> {
        Ok(GlobArgs {
            pattern: self.pattern,
            path: self.path.map(parse_fs_path).transpose()?,
            max_depth: None,
            limit: Some(100),
        })
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeBashArgs {
    command: String,
    timeout: Option<u64>,
    #[allow(dead_code)]
    description: Option<String>,
    #[allow(dead_code)]
    run_in_background: Option<bool>,
    #[allow(dead_code)]
    #[serde(rename = "dangerouslyDisableSandbox")]
    dangerously_disable_sandbox: Option<bool>,
}

impl ClaudeCodeBashArgs {
    fn into_run_process_args(self) -> RunProcessArgs {
        RunProcessArgs {
            argv: vec!["bash".to_string(), "-lc".to_string(), self.command],
            cwd: None,
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: self.timeout,
            yield_time_ms: None,
            max_output_bytes: None,
        }
    }
}

fn parse_fs_path(path: String) -> ToolResult<FsPath> {
    FsPath::new(path)
        .map_err(crate::fs::FsError::from)
        .map_err(ToolError::from)
}

fn claude_code_grep_visible(
    result: &GrepResult,
    output_mode: ClaudeCodeGrepOutputMode,
    show_line_numbers: bool,
    offset: usize,
    head_limit: Option<usize>,
) -> String {
    match output_mode {
        ClaudeCodeGrepOutputMode::Content => select_visible_entries(
            result
                .matches
                .iter()
                .map(|m| {
                    if show_line_numbers {
                        format!("{}:{}:{}", m.path, m.line_number, m.line)
                    } else {
                        format!("{}:{}", m.path, m.line)
                    }
                })
                .collect(),
            offset,
            head_limit,
        )
        .join("\n"),
        ClaudeCodeGrepOutputMode::FilesWithMatches => {
            let mut paths = result
                .matches
                .iter()
                .map(|m| m.path.to_string())
                .collect::<Vec<_>>();
            paths.dedup();
            select_visible_entries(paths, offset, head_limit).join("\n")
        }
        ClaudeCodeGrepOutputMode::Count => {
            let mut counts = BTreeMap::<String, usize>::new();
            for m in &result.matches {
                *counts.entry(m.path.to_string()).or_default() += 1;
            }
            let entries = counts
                .into_iter()
                .map(|(path, count)| format!("{path}:{count}"))
                .collect();
            select_visible_entries(entries, offset, head_limit).join("\n")
        }
    }
}

fn select_visible_entries(
    entries: Vec<String>,
    offset: usize,
    head_limit: Option<usize>,
) -> Vec<String> {
    let entries = entries.into_iter().skip(offset);
    match head_limit {
        Some(0) | None => entries.collect(),
        Some(limit) => entries.take(limit).collect(),
    }
}

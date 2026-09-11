//! Claude Code-like built-in tool surface.
//!
//! The process tools carry Claude Code's names and parameters: `Bash` with
//! `run_in_background`, `BashOutput`, and `KillShell`. The wording is ours
//! except the background start line. There is no stdin input and no `tty`
//! on this surface: Claude Code has neither.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::{
    environment::{
        process::{ProcessHandle, ProcessSignal},
        tools::{ContinueProcessArgs, RunProcessArgs, invoke_continue_process, invoke_run_process},
    },
    error::{ToolError, ToolResult},
    fs::{
        FsPath,
        tools::{
            EditFileArgs, GlobArgs, GrepArgs, GrepResult, ReadFileArgs, WriteFileArgs,
            invoke_edit_file, invoke_glob, invoke_grep, invoke_read_file, invoke_write_file,
        },
    },
    limits::ToolLimits,
    runtime::{ToolInvocationOutput, decode_args, encode_output},
};

use super::{
    BuiltinTool, BuiltinToolContext, BuiltinToolOperation, BuiltinToolVariant, canonical,
    shared::{
        ProcessPresentation, invalid_request, nullable_integer, nullable_string, object,
        optional_boolean, optional_enum, process_visible_output, string, visible_with_search_stop,
    },
};

/// How long `KillShell` waits for the killed group's final output: the
/// environment daemon's output drain grace.
const KILL_SHELL_WAIT_MS: u64 = 2_000;

pub(super) fn description(tool: BuiltinTool, scoped_paths: bool) -> ToolResult<String> {
    let path_guidance = if scoped_paths {
        " Paths are resolved within the configured filesystem scope."
    } else {
        ""
    };
    let text = match (tool.operation(), tool.variant()) {
        (BuiltinToolOperation::ReadFile, _) => {
            "Reads a file from the filesystem. Images (PNG, JPEG, GIF, WebP) and PDFs are shown to you as media and named by a media: handle you can reference."
        }
        (BuiltinToolOperation::WriteFile, _) => "Writes a file to the filesystem.",
        (BuiltinToolOperation::EditFile, _) => "Performs exact string replacements in a file.",
        (BuiltinToolOperation::Grep, _) => "Searches file contents with a regular expression.",
        (BuiltinToolOperation::Glob, _) => "Finds files by glob pattern.",
        (BuiltinToolOperation::RunProcess, _) if tool.one_shot() => {
            "Executes a shell command and waits for it to finish, killing it at `timeout`. A command may leave services running; they keep running until stopped or the environment is closed."
        }
        (BuiltinToolOperation::RunProcess, _) => {
            "Executes a shell command. Waits for it to finish, killing it at `timeout`, unless `run_in_background` is true, in which case it returns at once with an ID for BashOutput and KillShell. A command may leave services running; they keep running until stopped or the environment is closed."
        }
        (BuiltinToolOperation::ContinueProcess, BuiltinToolVariant::Primary) => {
            "Wait up to `timeout` for a background command to finish and return the output produced since the last call. Returns at once if it has already exited, with its exit code."
        }
        (BuiltinToolOperation::ContinueProcess, BuiltinToolVariant::Kill) => {
            "Kills a running background command by its ID and returns the output it produced since the last call."
        }
        (
            BuiltinToolOperation::Materialize
            | BuiltinToolOperation::Capture
            | BuiltinToolOperation::ListDir
            | BuiltinToolOperation::JobSubmit
            | BuiltinToolOperation::JobRun
            | BuiltinToolOperation::JobRead,
            _,
        ) => {
            return Ok(canonical::description(tool, scoped_paths));
        }
        (BuiltinToolOperation::ApplyPatch, _) => {
            return Err(unsupported(tool.operation()));
        }
    };
    Ok(format!("{text}{path_guidance}"))
}

pub(super) fn input_schema(tool: BuiltinTool) -> ToolResult<Value> {
    let schema = match (tool.operation(), tool.variant()) {
        (BuiltinToolOperation::ReadFile, _) => object(
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
        (BuiltinToolOperation::WriteFile, _) => object(
            [
                (
                    "file_path",
                    string("The absolute path to the file to write."),
                ),
                ("content", string("The content to write to the file.")),
            ],
            ["file_path", "content"],
        ),
        (BuiltinToolOperation::EditFile, _) => object(
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
        (BuiltinToolOperation::Grep, _) => object(
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
        (BuiltinToolOperation::Glob, _) => object(
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
        (BuiltinToolOperation::RunProcess, _) if tool.one_shot() => object(
            [
                ("command", string("The command to execute.")),
                (
                    "timeout",
                    nullable_integer("Optional kill deadline in milliseconds. Defaults to 60000."),
                ),
                (
                    "description",
                    nullable_string("Clear, concise description of what this command does."),
                ),
                (
                    "dangerouslyDisableSandbox",
                    optional_boolean("Parsed and ignored by Lightspeed tools."),
                ),
            ],
            ["command"],
        ),
        (BuiltinToolOperation::RunProcess, _) => object(
            [
                ("command", string("The command to execute.")),
                (
                    "timeout",
                    nullable_integer(
                        "Optional kill deadline in milliseconds. Defaults to 60000. Ignored when run_in_background is true.",
                    ),
                ),
                (
                    "description",
                    nullable_string("Clear, concise description of what this command does."),
                ),
                (
                    "run_in_background",
                    optional_boolean(
                        "Run the command in the background and return its ID at once. Read its output with BashOutput and stop it with KillShell.",
                    ),
                ),
                (
                    "dangerouslyDisableSandbox",
                    optional_boolean("Parsed and ignored by Lightspeed tools."),
                ),
            ],
            ["command"],
        ),
        (BuiltinToolOperation::ContinueProcess, BuiltinToolVariant::Primary) => object(
            [
                (
                    "bash_id",
                    string("The ID of the background command to read output from."),
                ),
                (
                    "timeout",
                    nullable_integer(
                        "Milliseconds to wait for the command to finish before returning what it produced so far. Defaults to 60000.",
                    ),
                ),
                (
                    "filter",
                    nullable_string("Optional regular expression. Parsed but not applied."),
                ),
            ],
            ["bash_id"],
        ),
        (BuiltinToolOperation::ContinueProcess, BuiltinToolVariant::Kill) => object(
            [(
                "shell_id",
                string("The ID of the background command to kill."),
            )],
            ["shell_id"],
        ),
        (
            BuiltinToolOperation::Materialize
            | BuiltinToolOperation::Capture
            | BuiltinToolOperation::ListDir
            | BuiltinToolOperation::JobSubmit
            | BuiltinToolOperation::JobRun
            | BuiltinToolOperation::JobRead,
            _,
        ) => {
            return Ok(canonical::input_schema(tool));
        }
        (BuiltinToolOperation::ApplyPatch, _) => {
            return Err(unsupported(tool.operation()));
        }
    };
    Ok(schema)
}

pub(super) async fn invoke_json(
    tool: BuiltinTool,
    ctx: BuiltinToolContext<'_>,
    arguments: Value,
) -> ToolResult<ToolInvocationOutput> {
    match (tool.operation(), tool.variant()) {
        (BuiltinToolOperation::ReadFile, _) => {
            let args: ClaudeCodeReadArgs = decode_args(arguments)?;
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_read_file(fs_ctx, args.try_into_read_file_args()?).await?;
            let media = result.media_outputs();
            encode_output(&result, result.line_numbered_text.clone())
                .map(|output| output.with_media(media))
        }
        (BuiltinToolOperation::WriteFile, _) => {
            let args: ClaudeCodeWriteArgs = decode_args(arguments)?;
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_write_file(fs_ctx, args.try_into_write_file_args()?).await?;
            let visible = format!(
                "Wrote {} bytes to {}",
                result.bytes_written, result.resolved_path
            );
            encode_output(&result, visible)
        }
        (BuiltinToolOperation::EditFile, _) => {
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
        (BuiltinToolOperation::Grep, _) => {
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
            encode_output(&result, visible_with_search_stop(visible, result.stopped))
        }
        (BuiltinToolOperation::Glob, _) => {
            let args: ClaudeCodeGlobArgs = decode_args(arguments)?;
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_glob(fs_ctx, args.try_into_glob_args()?).await?;
            let visible = result
                .matches
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            encode_output(&result, visible_with_search_stop(visible, result.stopped))
        }
        (BuiltinToolOperation::RunProcess, _) => {
            let args: ClaudeCodeBashArgs = decode_args(arguments)?;
            let env_ctx = ctx.environment()?;
            let background = !tool.one_shot() && args.run_in_background.unwrap_or(false);
            let result = invoke_run_process(
                env_ctx,
                args.into_run_process_args(background, &env_ctx.limits),
            )
            .await?;
            let visible =
                process_visible_output(&result, ProcessPresentation::ClaudeBash { background });
            encode_output(&result, visible)
        }
        (BuiltinToolOperation::ContinueProcess, BuiltinToolVariant::Primary) => {
            let args: ClaudeCodeBashOutputArgs = decode_args(arguments)?;
            let env_ctx = ctx.environment()?;
            let result =
                invoke_continue_process(env_ctx, args.into_continue_process_args(&env_ctx.limits))
                    .await?;
            let visible = process_visible_output(&result, ProcessPresentation::ClaudeBashOutput);
            encode_output(&result, visible)
        }
        (BuiltinToolOperation::ContinueProcess, BuiltinToolVariant::Kill) => {
            let args: ClaudeCodeKillShellArgs = decode_args(arguments)?;
            let env_ctx = ctx.environment()?;
            let result =
                invoke_continue_process(env_ctx, args.into_continue_process_args()).await?;
            let visible = process_visible_output(&result, ProcessPresentation::ClaudeKillShell);
            encode_output(&result, visible)
        }
        (
            BuiltinToolOperation::Materialize
            | BuiltinToolOperation::Capture
            | BuiltinToolOperation::ListDir
            | BuiltinToolOperation::JobSubmit
            | BuiltinToolOperation::JobRun
            | BuiltinToolOperation::JobRead,
            _,
        ) => canonical::invoke_json(tool, ctx, arguments).await,
        (BuiltinToolOperation::ApplyPatch, _) => Err(unsupported(tool.operation())),
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
    run_in_background: Option<bool>,
    #[allow(dead_code)]
    #[serde(rename = "dangerouslyDisableSandbox")]
    dangerously_disable_sandbox: Option<bool>,
}

impl ClaudeCodeBashArgs {
    /// A background start is a zero yield with no kill deadline; a foreground
    /// command waits to exit under `timeout` as a kill deadline, as in
    /// Claude Code.
    fn into_run_process_args(self, background: bool, limits: &ToolLimits) -> RunProcessArgs {
        let (yield_ms, timeout_ms) = if background {
            (Some(0), None)
        } else {
            (
                None,
                Some(self.timeout.unwrap_or(limits.default_process_timeout_ms)),
            )
        };
        RunProcessArgs {
            argv: vec!["bash".to_string(), "-lc".to_string(), self.command],
            cwd: None,
            env: BTreeMap::new(),
            stdin: None,
            tty: false,
            yield_ms,
            timeout_ms,
            max_output_bytes: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeBashOutputArgs {
    bash_id: String,
    timeout: Option<u64>,
    #[allow(dead_code)]
    filter: Option<String>,
}

impl ClaudeCodeBashOutputArgs {
    fn into_continue_process_args(self, limits: &ToolLimits) -> ContinueProcessArgs {
        ContinueProcessArgs::wait(
            ProcessHandle::new(self.bash_id),
            Some(self.timeout.unwrap_or(limits.default_process_timeout_ms)),
        )
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeKillShellArgs {
    shell_id: String,
}

impl ClaudeCodeKillShellArgs {
    fn into_continue_process_args(self) -> ContinueProcessArgs {
        ContinueProcessArgs {
            handle: ProcessHandle::new(self.shell_id),
            input: None,
            close_stdin: false,
            signal: Some(ProcessSignal::Kill),
            wait_ms: Some(KILL_SHELL_WAIT_MS),
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn bash_maps_background_to_a_zero_yield_and_foreground_to_a_kill_deadline() {
        let limits = ToolLimits::default();
        let background: ClaudeCodeBashArgs =
            decode_args(json!({ "command": "make", "run_in_background": true })).expect("args");
        let args = background.into_run_process_args(true, &limits);
        assert_eq!(args.argv, ["bash", "-lc", "make"]);
        assert_eq!(args.yield_ms, Some(0));
        assert_eq!(args.timeout_ms, None);

        let foreground: ClaudeCodeBashArgs =
            decode_args(json!({ "command": "make", "timeout": 5000 })).expect("args");
        let args = foreground.into_run_process_args(false, &limits);
        assert_eq!(args.yield_ms, None);
        assert_eq!(args.timeout_ms, Some(5000));

        let defaulted: ClaudeCodeBashArgs = decode_args(json!({ "command": "ls" })).expect("args");
        let args = defaulted.into_run_process_args(false, &limits);
        assert_eq!(args.timeout_ms, Some(limits.default_process_timeout_ms));
        assert!(!args.tty);
    }

    #[test]
    fn bash_output_and_kill_shell_map_onto_continue_process() {
        let limits = ToolLimits::default();
        let output: ClaudeCodeBashOutputArgs =
            decode_args(json!({ "bash_id": "proc-3", "filter": "err" })).expect("args");
        let args = output.into_continue_process_args(&limits);
        assert_eq!(args.handle.as_str(), "proc-3");
        assert_eq!(args.wait_ms, Some(limits.default_process_timeout_ms));
        assert_eq!(args.signal, None);
        assert_eq!(args.input, None);

        let kill: ClaudeCodeKillShellArgs =
            decode_args(json!({ "shell_id": "proc-3" })).expect("args");
        let args = kill.into_continue_process_args();
        assert_eq!(args.signal, Some(ProcessSignal::Kill));
        assert_eq!(args.wait_ms, Some(KILL_SHELL_WAIT_MS));
    }
}

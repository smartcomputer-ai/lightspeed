//! Canonical Lightspeed built-in tool surface: the substrate presented
//! directly, with neutral names and descriptions written for a model that
//! has seen neither Codex nor Claude Code.

use serde_json::{Value, json};

use crate::{
    environment::{
        jobs::{
            JOB_READ_TOOL_NAME, JOB_RUN_MAX_TIMEOUT_MS, JOB_RUN_TOOL_NAME, JOB_SUBMIT_TOOL_NAME,
            visible_job_read_output,
        },
        tools::{
            RunProcessArgs, invoke_continue_process, invoke_job_read, invoke_job_submit,
            invoke_run_process,
        },
    },
    error::ToolResult,
    fs::tools::{
        invoke_apply_patch, invoke_edit_file, invoke_glob, invoke_grep, invoke_list_dir,
        invoke_read_file, invoke_write_file,
    },
    runtime::{ToolInvocationOutput, decode_args, encode_output},
};

use super::{
    BuiltinTool, BuiltinToolContext, BuiltinToolOperation,
    shared::{
        ProcessPresentation, array_of_strings, boolean, nullable_integer, nullable_string, object,
        optional_enum, process_visible_output, string, string_map, visible_with_search_stop,
    },
};

pub(super) fn description(tool: BuiltinTool, scoped_paths: bool) -> String {
    let path_guidance = if scoped_paths {
        " Paths are resolved within the configured filesystem scope."
    } else {
        ""
    };
    let text = match tool.operation() {
        BuiltinToolOperation::ReadFile => {
            "Read a UTF-8 file with optional 1-based line offset and line limit. Images (PNG, JPEG, GIF, WebP) and PDFs are shown to you as media and named by a media: handle you can reference."
        }
        BuiltinToolOperation::WriteFile => {
            "Write full UTF-8 file content, creating parent directories when needed."
        }
        BuiltinToolOperation::EditFile => {
            "Replace exact text in a UTF-8 file. Multiple matches require replace_all=true."
        }
        BuiltinToolOperation::ApplyPatch => {
            "Apply a Codex-style apply_patch patch to the filesystem."
        }
        BuiltinToolOperation::Grep => "Search UTF-8 files recursively with a regular expression.",
        BuiltinToolOperation::Glob => "Find files recursively with a glob pattern.",
        BuiltinToolOperation::ListDir => "List one directory.",
        BuiltinToolOperation::Materialize => {
            "Copy a VFS file or tree to the current environment. Replaces the complete selected destination by default, preserving siblings. Transfers only missing file content."
        }
        BuiltinToolOperation::Capture => {
            "Capture an environment file or tree into a writable VFS workspace. Replaces the selected destination by default. Fails publication if the workspace changed concurrently; returns the captured snapshot for recovery."
        }
        BuiltinToolOperation::RunProcess if tool.one_shot() => {
            "Run a command and wait until it exits, returning its output. With `timeout_ms` the command is killed at that deadline. A command may leave services running; they keep running until stopped or the environment closes. Interactive programs need `tty: true`."
        }
        BuiltinToolOperation::RunProcess => {
            "Run a command. Waits until it exits, or until `yield_ms` if set, and returns its output. If it is still running you get a handle for `continue_process`. With `timeout_ms` the command is killed at that deadline; without it a running command keeps running until stopped or the environment closes. Interactive programs need `tty: true`."
        }
        BuiltinToolOperation::ContinueProcess => {
            "Continue with a running handle: optionally send input or a signal, then wait up to `wait_ms` and return the output produced since the last call. With nothing but the handle it only waits. Once the process has exited it returns the remaining output and the exit code."
        }
        BuiltinToolOperation::JobSubmit => {
            "Start one or more durable environment jobs asynchronously. Returns one Promise per job; use await, cancel, or detach when appropriate."
        }
        BuiltinToolOperation::JobRun => {
            "Run one durable environment job and wait for its terminal readable result. Use job_submit for dependency groups, longer work, or explicit Promise control."
        }
        BuiltinToolOperation::JobRead => {
            "Read durable environment job status and bounded output tails."
        }
    };
    format!("{text}{path_guidance}")
}

pub(super) fn input_schema(tool: BuiltinTool) -> Value {
    match tool.operation() {
        BuiltinToolOperation::Materialize => {
            json!({"type":"object","properties":{"source_vfs_path":{"type":"string"},"destination_environment_path":{"type":"string"},"on_existing":{"type":"string","enum":["replace","error"]}},"required":["source_vfs_path","destination_environment_path"],"additionalProperties":false})
        }
        BuiltinToolOperation::Capture => {
            json!({"type":"object","properties":{"source_environment_path":{"type":"string"},"destination_vfs_path":{"type":"string"},"on_existing":{"type":"string","enum":["replace","error"]}},"required":["source_environment_path","destination_vfs_path"],"additionalProperties":false})
        }
        BuiltinToolOperation::ReadFile => object(
            [
                ("path", string("File path to read.")),
                (
                    "offset",
                    nullable_integer("1-based line number to start at."),
                ),
                (
                    "limit",
                    nullable_integer("Maximum number of lines to return."),
                ),
            ],
            ["path"],
        ),
        BuiltinToolOperation::WriteFile => object(
            [
                ("path", string("File path to write.")),
                ("content", string("Full file content.")),
            ],
            ["path", "content"],
        ),
        BuiltinToolOperation::EditFile => object(
            [
                ("path", string("File path to edit.")),
                ("old_string", string("Exact text to replace.")),
                ("new_string", string("Replacement text.")),
                (
                    "replace_all",
                    boolean(
                        "Replace all matches instead of requiring one match. Defaults to false.",
                    ),
                ),
            ],
            ["path", "old_string", "new_string"],
        ),
        BuiltinToolOperation::ApplyPatch => object(
            [(
                "patch",
                string("Full apply_patch text, including begin and end markers."),
            )],
            ["patch"],
        ),
        BuiltinToolOperation::Grep => object(
            [
                ("pattern", string("Regular expression to search for.")),
                ("path", nullable_string("Directory path to search from.")),
                (
                    "include",
                    nullable_string("Optional glob for files to include."),
                ),
                (
                    "case_sensitive",
                    boolean("Whether the regex is case-sensitive. Defaults to false."),
                ),
                (
                    "max_depth",
                    nullable_integer("Optional maximum directory depth."),
                ),
                (
                    "limit",
                    nullable_integer("Maximum number of matching lines."),
                ),
            ],
            ["pattern"],
        ),
        BuiltinToolOperation::Glob => object(
            [
                ("pattern", string("Glob pattern to match files.")),
                ("path", nullable_string("Directory path to search from.")),
                (
                    "max_depth",
                    nullable_integer("Optional maximum directory depth."),
                ),
                (
                    "limit",
                    nullable_integer("Maximum number of matching files."),
                ),
            ],
            ["pattern"],
        ),
        BuiltinToolOperation::ListDir => object(
            [(
                "path",
                string("Directory path to list. Defaults to the workspace root."),
            )],
            [],
        ),
        BuiltinToolOperation::RunProcess if tool.one_shot() => object(
            [
                ("argv", array_of_strings("Command and arguments.")),
                (
                    "cwd",
                    nullable_string("Working directory. Defaults to the environment's."),
                ),
                (
                    "env",
                    string_map("Environment variables to add. Defaults to empty."),
                ),
                (
                    "stdin",
                    nullable_string("Standard input, written and closed at start."),
                ),
                (
                    "tty",
                    boolean("Allocate a pseudo-terminal. Defaults to false."),
                ),
                (
                    "timeout_ms",
                    nullable_integer(
                        "Kill the command after this many milliseconds. Requests above the deployment ceiling (30 minutes) are clamped.",
                    ),
                ),
                (
                    "max_output_bytes",
                    nullable_integer("Output byte budget for this call."),
                ),
            ],
            ["argv"],
        ),
        BuiltinToolOperation::RunProcess => object(
            [
                ("argv", array_of_strings("Command and arguments.")),
                (
                    "cwd",
                    nullable_string("Working directory. Defaults to the environment's."),
                ),
                (
                    "env",
                    string_map("Environment variables to add. Defaults to empty."),
                ),
                (
                    "stdin",
                    nullable_string("Standard input, written and closed at start."),
                ),
                (
                    "tty",
                    boolean(
                        "Allocate a pseudo-terminal so `continue_process` can send input. Defaults to false.",
                    ),
                ),
                (
                    "yield_ms",
                    nullable_integer(
                        "Return after this many milliseconds with a handle if the command is still running. Defaults to waiting until it exits, up to 30 minutes.",
                    ),
                ),
                (
                    "timeout_ms",
                    nullable_integer(
                        "Kill the command after this many milliseconds. Absent means it is never killed by this call. Requests above the deployment ceiling (30 minutes) are clamped.",
                    ),
                ),
                (
                    "max_output_bytes",
                    nullable_integer("Output byte budget for this call."),
                ),
            ],
            ["argv"],
        ),
        BuiltinToolOperation::ContinueProcess => object(
            [
                ("handle", string("Handle returned by `run_process`.")),
                (
                    "input",
                    nullable_string(
                        "Text to send to the process's input. Requires the command to have been started with `tty: true`.",
                    ),
                ),
                (
                    "close_stdin",
                    boolean("Close the process's input after writing. Defaults to false."),
                ),
                (
                    "signal",
                    optional_enum(
                        "Send a signal to the process group: `interrupt` (SIGINT) or `kill`.",
                        ["interrupt", "kill"],
                    ),
                ),
                (
                    "wait_ms",
                    nullable_integer(
                        "Collect output for this many milliseconds, returning early if the process exits. Defaults to waiting until it exits, up to 30 minutes.",
                    ),
                ),
                (
                    "max_output_bytes",
                    nullable_integer("Output byte budget for this call."),
                ),
            ],
            ["handle"],
        ),
        BuiltinToolOperation::JobSubmit => job_submit_schema(),
        BuiltinToolOperation::JobRun => job_run_schema(),
        BuiltinToolOperation::JobRead => job_read_schema(),
    }
}

pub(super) async fn invoke_json(
    tool: BuiltinTool,
    ctx: BuiltinToolContext<'_>,
    arguments: Value,
) -> ToolResult<ToolInvocationOutput> {
    match tool.operation() {
        BuiltinToolOperation::Materialize => {
            crate::transfer::invoke_materialize(
                ctx.vfs()?,
                ctx.environment()?,
                ctx.transfer_operation_id(),
                arguments,
            )
            .await
        }
        BuiltinToolOperation::Capture => {
            crate::transfer::invoke_capture(
                ctx.vfs()?,
                ctx.environment()?,
                ctx.transfer_operation_id(),
                arguments,
            )
            .await
        }
        BuiltinToolOperation::ReadFile => {
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_read_file(fs_ctx, decode_args(arguments)?).await?;
            let media = result.media_outputs();
            encode_output(&result, result.line_numbered_text.clone())
                .map(|output| output.with_media(media))
        }
        BuiltinToolOperation::WriteFile => {
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_write_file(fs_ctx, decode_args(arguments)?).await?;
            let visible = format!(
                "Wrote {} bytes to {}",
                result.bytes_written, result.resolved_path
            );
            encode_output(&result, visible)
        }
        BuiltinToolOperation::EditFile => {
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_edit_file(fs_ctx, decode_args(arguments)?).await?;
            let visible = format!(
                "Replaced {} match(es) in {}",
                result.replacements, result.resolved_path
            );
            encode_output(&result, visible)
        }
        BuiltinToolOperation::ApplyPatch => {
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_apply_patch(fs_ctx, decode_args(arguments)?).await?;
            encode_output(&result, result.output.clone())
        }
        BuiltinToolOperation::Grep => {
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_grep(fs_ctx, decode_args(arguments)?).await?;
            let visible = result
                .matches
                .iter()
                .map(|m| format!("{}:{}:{}", m.path, m.line_number, m.line))
                .collect::<Vec<_>>()
                .join("\n");
            encode_output(&result, visible_with_search_stop(visible, result.stopped))
        }
        BuiltinToolOperation::Glob => {
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_glob(fs_ctx, decode_args(arguments)?).await?;
            let visible = result
                .matches
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            encode_output(&result, visible_with_search_stop(visible, result.stopped))
        }
        BuiltinToolOperation::ListDir => {
            let fs_ctx = ctx.filesystem()?;
            let result = invoke_list_dir(fs_ctx, decode_args(arguments)?).await?;
            let visible = result
                .entries
                .iter()
                .map(|entry| {
                    let suffix = if entry.is_directory { "/" } else { "" };
                    format!("{}{suffix}", entry.file_name)
                })
                .collect::<Vec<_>>()
                .join("\n");
            encode_output(&result, visible)
        }
        BuiltinToolOperation::RunProcess => {
            let env_ctx = ctx.environment()?;
            let mut args: RunProcessArgs = decode_args(arguments)?;
            if tool.one_shot() {
                args.yield_ms = None;
            }
            let result = invoke_run_process(env_ctx, args).await?;
            let visible = process_visible_output(&result, ProcessPresentation::Canonical);
            encode_output(&result, visible)
        }
        BuiltinToolOperation::ContinueProcess => {
            let env_ctx = ctx.environment()?;
            let result = invoke_continue_process(env_ctx, decode_args(arguments)?).await?;
            let visible = process_visible_output(&result, ProcessPresentation::Canonical);
            encode_output(&result, visible)
        }
        BuiltinToolOperation::JobSubmit => {
            let env_ctx = ctx.environment()?;
            let result = invoke_job_submit(env_ctx, decode_args(arguments)?).await?;
            let visible = result
                .jobs
                .iter()
                .map(|job| format!("{}: {:?}", job.job_id.as_str(), job.status))
                .collect::<Vec<_>>()
                .join("\n");
            encode_output(&result, visible)
        }
        BuiltinToolOperation::JobRun => Err(crate::error::ToolError::UnsupportedCapability {
            message: "job_run requires its joined workflow-tool binding".to_owned(),
        }),
        BuiltinToolOperation::JobRead => {
            let env_ctx = ctx.environment()?;
            let result = invoke_job_read(env_ctx, decode_args(arguments)?).await?;
            let visible = visible_job_read_output(&result.jobs);
            encode_output(&result, visible)
        }
    }
}

fn job_submit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "jobs": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": ["string", "null"] },
                        "job_id": {
                            "type": "string",
                            "pattern": "^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$",
                            "description": "Your name for the job (for example \"build\"). It keys the job's promise in the result and identifies the job to job_read."
                        },
                        "argv": { "type": "array", "items": { "type": "string" } },
                        "cwd": { "type": ["string", "null"] },
                        "env": { "type": "object", "additionalProperties": { "type": "string" } },
                        "stdin": { "type": ["string", "null"] },
                        "timeout_ms": { "type": ["integer", "null"], "minimum": 0 },
                        "depends_on": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "job_id": { "type": ["string", "null"] },
                                    "name": { "type": ["string", "null"] }
                                },
                                "additionalProperties": false
                            }
                        },
                        "dependency_policy": { "type": "string", "enum": ["allSucceeded", "allTerminal"] },
                        "queue_key": { "type": ["string", "null"] }
                    },
                    "required": ["job_id", "argv"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["jobs"],
        "additionalProperties": false,
        "description": JOB_SUBMIT_TOOL_NAME
    })
}

fn job_run_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": ["string", "null"] },
            "argv": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "description": "Process argv for the single durable job."
            },
            "cwd": { "type": ["string", "null"] },
            "env": { "type": "object", "additionalProperties": { "type": "string" } },
            "stdin": { "type": ["string", "null"] },
            "timeout_ms": {
                "type": ["integer", "null"],
                "minimum": 0,
                "maximum": JOB_RUN_MAX_TIMEOUT_MS,
                "description": "Provider execution timeout. Defaults to 30 minutes; maximum 60 minutes."
            },
            "queue_key": { "type": ["string", "null"] }
        },
        "required": ["argv"],
        "additionalProperties": false,
        "description": JOB_RUN_TOOL_NAME
    })
}

fn job_handle_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "environment_id": {
                "type": ["string", "null"],
                "description": "Omit for this session's active environment (where job_submit and job_run start jobs); set it only to read a job in another environment."
            },
            "job_id": { "type": "string", "description": "The job id you gave job_submit, or the one job_run returned." }
        },
        "required": ["job_id"],
        "additionalProperties": false
    })
}

fn job_read_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "jobs": { "type": "array", "items": job_handle_schema() },
            "output_bytes": { "type": ["integer", "null"], "minimum": 0 },
            "after_seq": { "type": ["integer", "null"], "minimum": 0 },
            "include_artifacts": { "type": "boolean" }
        },
        "required": ["jobs"],
        "additionalProperties": false,
        "description": JOB_READ_TOOL_NAME
    })
}

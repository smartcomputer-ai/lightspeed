//! Codex-like built-in tool surface.
//!
//! Filesystem tools share the canonical schemas. The process tools copy
//! Codex CLI's `exec_command` and `write_stdin` as of Codex `728cb12fe5`:
//! names, argument names, defaults, the `Wall time` / `Process …` /
//! `Output:` header, Ctrl-C as interrupt, and stdin only under a PTY. What
//! would change the substrate is refused: session ids stay handle strings,
//! there is no 64-session LRU kill, no structured output schema, and the
//! yield cap is the deployment's 30 minutes rather than Codex's 30 seconds.

use std::{collections::BTreeMap, time::Instant};

use serde::Deserialize;
use serde_json::Value;

use crate::{
    environment::{
        process::ProcessSignal,
        tools::{ContinueProcessArgs, RunProcessArgs, invoke_continue_process, invoke_run_process},
    },
    error::{ToolError, ToolResult},
    fs::FsPath,
    limits::ToolLimits,
    runtime::{ToolInvocationOutput, decode_args, encode_output},
};

use super::{
    BuiltinTool, BuiltinToolContext, BuiltinToolOperation, canonical,
    shared::{
        ProcessPresentation, nullable_integer, nullable_string, object, optional_boolean,
        process_visible_output, string,
    },
};

/// Codex defaults and caps, in milliseconds and tokens.
const EXEC_DEFAULT_YIELD_MS: u64 = 10_000;
const EXEC_MIN_YIELD_MS: u64 = 250;
const EXEC_MAX_YIELD_MS: u64 = 1_800_000;
const WRITE_DEFAULT_YIELD_MS: u64 = 250;
const WRITE_MAX_YIELD_MS: u64 = 30_000;
const POLL_DEFAULT_YIELD_MS: u64 = 60_000;
const POLL_MAX_YIELD_MS: u64 = 1_800_000;
const DEFAULT_OUTPUT_TOKENS: u64 = 10_000;
const BYTES_PER_OUTPUT_TOKEN: u64 = 4;
const CTRL_C: &str = "\u{3}";

pub(super) fn description(tool: BuiltinTool, scoped_paths: bool) -> String {
    match tool.operation() {
        BuiltinToolOperation::ApplyPatch => {
            let mut description = "Apply a patch to create, edit, delete, or move files. Pass the patch text in the JSON `patch` argument. Use `apply_patch` syntax, starting with `*** Begin Patch` and ending with `*** End Patch`.".to_owned();
            if scoped_paths {
                description.push_str(" Paths are resolved within the configured filesystem scope.");
            }
            description
        }
        BuiltinToolOperation::RunProcess if tool.one_shot() => {
            "Runs a command to completion and returns its output. The process is terminated on timeout or cancellation and cannot be resumed.".to_owned()
        }
        BuiltinToolOperation::RunProcess => {
            "Runs a command in a shell (a PTY when `tty` is true), returning output or a session ID for ongoing interaction. A command may leave services running for later calls; they keep running until stopped or the environment closes.".to_owned()
        }
        BuiltinToolOperation::ContinueProcess => {
            "Writes characters to an existing unified exec session and returns recent output. Empty `chars` polls without writing; Ctrl-C (\\u0003) interrupts the session.".to_owned()
        }
        _ => canonical::description(tool, scoped_paths),
    }
}

pub(super) fn input_schema(tool: BuiltinTool) -> Value {
    match tool.operation() {
        BuiltinToolOperation::RunProcess if tool.one_shot() => object(
            [
                ("cmd", string("Shell command to execute.")),
                (
                    "workdir",
                    nullable_string(
                        "Working directory for the command. Defaults to the environment's working directory.",
                    ),
                ),
                (
                    "timeout_ms",
                    nullable_integer(
                        "Kill the command after this many milliseconds. Defaults to 60000 ms.",
                    ),
                ),
                (
                    "max_output_tokens",
                    nullable_integer("Output token budget. Defaults to 10000 tokens."),
                ),
                (
                    "login",
                    optional_boolean("Run in a login shell. Defaults to true."),
                ),
            ],
            ["cmd"],
        ),
        BuiltinToolOperation::RunProcess => object(
            [
                ("cmd", string("Shell command to execute.")),
                (
                    "workdir",
                    nullable_string(
                        "Working directory for the command. Defaults to the environment's working directory.",
                    ),
                ),
                (
                    "tty",
                    optional_boolean(
                        "True allocates a PTY for the command; false or omitted uses plain pipes.",
                    ),
                ),
                (
                    "yield_time_ms",
                    nullable_integer(
                        "Wait before yielding output. Defaults to 10000 ms; effective range is 250-1800000 ms.",
                    ),
                ),
                (
                    "max_output_tokens",
                    nullable_integer("Output token budget. Defaults to 10000 tokens."),
                ),
                (
                    "login",
                    optional_boolean("Run in a login shell. Defaults to true."),
                ),
            ],
            ["cmd"],
        ),
        BuiltinToolOperation::ContinueProcess => object(
            [
                (
                    "session_id",
                    string("Identifier of the running session, from `exec_command`."),
                ),
                (
                    "chars",
                    nullable_string(
                        "Bytes to write to stdin. Defaults to empty, which polls without writing.",
                    ),
                ),
                (
                    "yield_time_ms",
                    nullable_integer(
                        "Wait before yielding output. Non-empty writes default to 250 ms and cap at 30000 ms; empty polls default to 60000 ms and cap at 1800000 ms.",
                    ),
                ),
                (
                    "max_output_tokens",
                    nullable_integer("Output token budget. Defaults to 10000 tokens."),
                ),
            ],
            ["session_id"],
        ),
        _ => canonical::input_schema(tool),
    }
}

pub(super) async fn invoke_json(
    tool: BuiltinTool,
    ctx: BuiltinToolContext<'_>,
    arguments: Value,
) -> ToolResult<ToolInvocationOutput> {
    match tool.operation() {
        BuiltinToolOperation::RunProcess => {
            let args: CodexExecCommandArgs = decode_args(arguments)?;
            let env_ctx = ctx.environment()?;
            let started = Instant::now();
            let result = invoke_run_process(
                env_ctx,
                args.into_run_process_args(tool.one_shot(), &env_ctx.limits)?,
            )
            .await?;
            let visible = process_visible_output(
                &result,
                ProcessPresentation::Codex {
                    wall_time: started.elapsed(),
                },
            );
            encode_output(&result, visible)
        }
        BuiltinToolOperation::ContinueProcess => {
            let args: CodexWriteStdinArgs = decode_args(arguments)?;
            let env_ctx = ctx.environment()?;
            let started = Instant::now();
            let result =
                invoke_continue_process(env_ctx, args.into_continue_process_args()).await?;
            let visible = process_visible_output(
                &result,
                ProcessPresentation::Codex {
                    wall_time: started.elapsed(),
                },
            );
            encode_output(&result, visible)
        }
        _ => canonical::invoke_json(tool, ctx, arguments).await,
    }
}

#[derive(Debug, Deserialize)]
struct CodexExecCommandArgs {
    cmd: String,
    workdir: Option<String>,
    tty: Option<bool>,
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<u64>,
    login: Option<bool>,
    /// Only the one-shot shape offers it; honored if a model sends it
    /// anyway, since it can only shorten the command's life.
    timeout_ms: Option<u64>,
}

impl CodexExecCommandArgs {
    fn into_run_process_args(
        self,
        one_shot: bool,
        limits: &ToolLimits,
    ) -> ToolResult<RunProcessArgs> {
        let shell_flag = if self.login.unwrap_or(true) {
            "-lc"
        } else {
            "-c"
        };
        let (yield_ms, timeout_ms) = if one_shot {
            (
                None,
                Some(self.timeout_ms.unwrap_or(limits.default_process_timeout_ms)),
            )
        } else {
            (
                Some(
                    self.yield_time_ms
                        .unwrap_or(EXEC_DEFAULT_YIELD_MS)
                        .clamp(EXEC_MIN_YIELD_MS, EXEC_MAX_YIELD_MS),
                ),
                self.timeout_ms,
            )
        };
        Ok(RunProcessArgs {
            argv: vec!["bash".to_owned(), shell_flag.to_owned(), self.cmd],
            cwd: self.workdir.map(parse_fs_path).transpose()?,
            env: BTreeMap::new(),
            stdin: None,
            tty: self.tty.unwrap_or(false),
            yield_ms,
            timeout_ms,
            max_output_bytes: Some(output_bytes_for_tokens(self.max_output_tokens)),
        })
    }
}

#[derive(Debug, Deserialize)]
struct CodexWriteStdinArgs {
    session_id: String,
    chars: Option<String>,
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<u64>,
}

impl CodexWriteStdinArgs {
    fn into_continue_process_args(self) -> ContinueProcessArgs {
        let chars = self.chars.unwrap_or_default();
        let max_output_bytes = Some(output_bytes_for_tokens(self.max_output_tokens));
        let handle = crate::environment::process::ProcessHandle::new(self.session_id);
        if chars.is_empty() {
            return ContinueProcessArgs {
                handle,
                input: None,
                close_stdin: false,
                signal: None,
                wait_ms: Some(
                    self.yield_time_ms
                        .unwrap_or(POLL_DEFAULT_YIELD_MS)
                        .min(POLL_MAX_YIELD_MS),
                ),
                max_output_bytes,
            };
        }
        let wait_ms = Some(
            self.yield_time_ms
                .unwrap_or(WRITE_DEFAULT_YIELD_MS)
                .min(WRITE_MAX_YIELD_MS),
        );
        if chars == CTRL_C {
            return ContinueProcessArgs {
                handle,
                input: None,
                close_stdin: false,
                signal: Some(ProcessSignal::Interrupt),
                wait_ms,
                max_output_bytes,
            };
        }
        ContinueProcessArgs {
            handle,
            input: Some(chars),
            close_stdin: false,
            signal: None,
            wait_ms,
            max_output_bytes,
        }
    }
}

fn output_bytes_for_tokens(max_output_tokens: Option<u64>) -> u64 {
    max_output_tokens
        .unwrap_or(DEFAULT_OUTPUT_TOKENS)
        .saturating_mul(BYTES_PER_OUTPUT_TOKEN)
}

fn parse_fs_path(path: String) -> ToolResult<FsPath> {
    FsPath::new(path)
        .map_err(crate::fs::FsError::from)
        .map_err(ToolError::from)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn exec_args(value: Value) -> CodexExecCommandArgs {
        decode_args(value).expect("exec_command args")
    }

    fn write_args(value: Value) -> CodexWriteStdinArgs {
        decode_args(value).expect("write_stdin args")
    }

    #[test]
    fn exec_command_maps_to_a_shell_string_with_codex_defaults_and_no_kill() {
        let limits = ToolLimits::default();
        let args = exec_args(json!({ "cmd": "sleep 30" }))
            .into_run_process_args(false, &limits)
            .expect("args");
        assert_eq!(args.argv, ["bash", "-lc", "sleep 30"]);
        assert_eq!(args.yield_ms, Some(EXEC_DEFAULT_YIELD_MS));
        assert_eq!(args.timeout_ms, None, "Codex never kills at a yield");
        assert!(!args.tty);
        assert_eq!(
            args.max_output_bytes,
            Some(DEFAULT_OUTPUT_TOKENS * BYTES_PER_OUTPUT_TOKEN)
        );

        let args = exec_args(json!({
            "cmd": "python3 -i",
            "workdir": "/workspace",
            "tty": true,
            "yield_time_ms": 10,
            "max_output_tokens": 100,
            "login": false
        }))
        .into_run_process_args(false, &limits)
        .expect("args");
        assert_eq!(args.argv, ["bash", "-c", "python3 -i"]);
        assert_eq!(args.cwd, Some(FsPath::new("/workspace").expect("cwd")));
        assert!(args.tty);
        assert_eq!(args.yield_ms, Some(EXEC_MIN_YIELD_MS), "floored at 250 ms");
        assert_eq!(args.max_output_bytes, Some(400));

        let args = exec_args(json!({ "cmd": "true", "yield_time_ms": 999_999_999 }))
            .into_run_process_args(false, &limits)
            .expect("args");
        assert_eq!(args.yield_ms, Some(EXEC_MAX_YIELD_MS));
    }

    #[test]
    fn one_shot_exec_command_waits_to_exit_with_a_default_kill_deadline() {
        let limits = ToolLimits::default();
        let args = exec_args(json!({ "cmd": "make" }))
            .into_run_process_args(true, &limits)
            .expect("args");
        assert_eq!(args.yield_ms, None);
        assert_eq!(args.timeout_ms, Some(limits.default_process_timeout_ms));

        let args = exec_args(json!({ "cmd": "make", "timeout_ms": 5000 }))
            .into_run_process_args(true, &limits)
            .expect("args");
        assert_eq!(args.timeout_ms, Some(5000));
    }

    #[test]
    fn write_stdin_maps_polls_input_and_ctrl_c() {
        let poll = write_args(json!({ "session_id": "proc-1" })).into_continue_process_args();
        assert_eq!(poll.handle.as_str(), "proc-1");
        assert_eq!(poll.input, None);
        assert_eq!(poll.signal, None);
        assert_eq!(poll.wait_ms, Some(POLL_DEFAULT_YIELD_MS));

        let long_poll = write_args(json!({
            "session_id": "proc-1",
            "chars": "",
            "yield_time_ms": 9_999_999
        }))
        .into_continue_process_args();
        assert_eq!(long_poll.wait_ms, Some(POLL_MAX_YIELD_MS));

        let input = write_args(json!({ "session_id": "proc-1", "chars": "y\n" }))
            .into_continue_process_args();
        assert_eq!(input.input, Some("y\n".to_owned()));
        assert_eq!(input.wait_ms, Some(WRITE_DEFAULT_YIELD_MS));

        let capped = write_args(json!({
            "session_id": "proc-1",
            "chars": "y\n",
            "yield_time_ms": 90_000
        }))
        .into_continue_process_args();
        assert_eq!(capped.wait_ms, Some(WRITE_MAX_YIELD_MS));

        let interrupt = write_args(json!({ "session_id": "proc-1", "chars": "\u{3}" }))
            .into_continue_process_args();
        assert_eq!(interrupt.input, None);
        assert_eq!(interrupt.signal, Some(ProcessSignal::Interrupt));
    }
}

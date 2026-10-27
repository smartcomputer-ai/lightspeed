//! Shared helpers for built-in tool surfaces and operations.

use std::time::Duration;

use serde_json::{Value, json};

use crate::{
    environment::process::{LeftoverProcess, ProcessOutput, ProcessStatus, StreamOutput},
    error::ToolError,
};

/// Model-visible marker for a search that stopped at one of its bounds,
/// naming the bound and how to narrow the search.
pub(super) fn visible_with_search_stop(
    mut visible: String,
    stopped: Option<crate::fs::FsSearchStop>,
) -> String {
    let Some(stopped) = stopped else {
        return visible;
    };
    let note = match stopped {
        crate::fs::FsSearchStop::MatchLimit => {
            "[truncated: match limit reached — narrow the pattern or lower the limit]"
        }
        crate::fs::FsSearchStop::FileLimit => {
            "[truncated: file budget exhausted — narrow the path or add an include filter]"
        }
        crate::fs::FsSearchStop::ByteLimit => {
            "[truncated: byte budget exhausted — narrow the path or add an include filter]"
        }
        crate::fs::FsSearchStop::TimeLimit => {
            "[truncated: time budget exhausted — narrow the path or add an include filter]"
        }
        crate::fs::FsSearchStop::ResponseLimit => {
            "[truncated: response size limit reached — narrow the pattern or the path]"
        }
    };
    if !visible.is_empty() {
        visible.push('\n');
    }
    visible.push_str(note);
    visible
}

/// How a surface presents a process result. The substrate result is the
/// same everywhere; only the wording around it follows the harness the
/// model was trained on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProcessPresentation {
    /// Lightspeed's own plain state line.
    Canonical,
    /// Codex's `Wall time` / `Process …` / `Output:` header.
    Codex { wall_time: Duration },
    /// Claude Code's `Bash`; a background start returns the ID line.
    ClaudeBash { background: bool },
    /// Claude Code's `BashOutput`: `[exited with code N]` or `[still running]`.
    ClaudeBashOutput,
    /// Claude Code's `KillShell`.
    ClaudeKillShell,
}

/// Renders the state the model needs to act on, not only the bytes: the
/// handle and pid while running, the exit code once exited, and what the
/// command left running. Nothing the model needs lives only in the JSON.
pub(super) fn process_visible_output(
    output: &ProcessOutput,
    presentation: ProcessPresentation,
) -> String {
    let body = process_body(output);
    let mut visible = match presentation {
        ProcessPresentation::Canonical | ProcessPresentation::ClaudeBash { background: false } => {
            join_lines(body, state_line(output))
        }
        ProcessPresentation::ClaudeBash { background: true } => match &output.handle {
            Some(handle) => join_lines(
                body,
                format!(
                    "Command running in background with ID: {handle}. Use BashOutput to read its output and KillShell to stop it."
                ),
            ),
            None => join_lines(body, state_line(output)),
        },
        ProcessPresentation::ClaudeBashOutput => join_lines(
            body,
            if output.status == ProcessStatus::Running {
                "[still running]".to_owned()
            } else {
                state_line(output)
            },
        ),
        ProcessPresentation::ClaudeKillShell => join_lines(body, state_line(output)),
        ProcessPresentation::Codex { wall_time } => codex_visible(output, body, wall_time),
    };
    if let Some(note) = leftover_note(&output.leftover_processes) {
        visible = join_lines(visible, note);
    }
    visible
}

fn join_lines(first: String, second: String) -> String {
    if first.is_empty() {
        second
    } else if second.is_empty() {
        first
    } else {
        format!("{first}\n{second}")
    }
}

fn process_body(output: &ProcessOutput) -> String {
    let stdout = stream_text(&output.stdout, output.omitted_bytes);
    let stderr = stream_text(&output.stderr, output.omitted_bytes);
    join_lines(stdout, stderr)
}

fn stream_text(stream: &StreamOutput, omitted_bytes: u64) -> String {
    let Some(at) = stream.omitted_at.filter(|_| omitted_bytes > 0) else {
        return stream.text_lossy();
    };
    let at = at.min(stream.bytes.len());
    let head = String::from_utf8_lossy(&stream.bytes[..at]);
    let tail = String::from_utf8_lossy(&stream.bytes[at..]);
    let marker = format!("[omitted {omitted_bytes} bytes]");
    let mut text = String::new();
    if !head.is_empty() {
        text.push_str(&head);
        if !head.ends_with('\n') {
            text.push('\n');
        }
    }
    text.push_str(&marker);
    if !tail.is_empty() {
        if !tail.starts_with('\n') {
            text.push('\n');
        }
        text.push_str(&tail);
    }
    text
}

fn state_line(output: &ProcessOutput) -> String {
    match output.status {
        ProcessStatus::Running => match (&output.handle, output.pid) {
            (Some(handle), Some(pid)) => format!("[running: handle {handle}, pid {pid}]"),
            (Some(handle), None) => format!("[running: handle {handle}]"),
            (None, _) => "[running]".to_owned(),
        },
        ProcessStatus::Succeeded | ProcessStatus::Failed => match output.exit_code {
            Some(code) => format!("[exited with code {code}]"),
            None => match &output.failure {
                Some(failure) => format!("[failed: {failure}]"),
                None => "[failed]".to_owned(),
            },
        },
        ProcessStatus::TimedOut => "[timed out]".to_owned(),
        ProcessStatus::Killed => "[killed]".to_owned(),
    }
}

fn codex_visible(output: &ProcessOutput, body: String, wall_time: Duration) -> String {
    let mut text = format!("Wall time: {} seconds\n", format_seconds(wall_time));
    let status = match output.status {
        ProcessStatus::Running => match (&output.handle, output.pid) {
            (Some(handle), Some(pid)) => {
                format!("Process running with session ID {handle}, pid {pid}")
            }
            (Some(handle), None) => format!("Process running with session ID {handle}"),
            (None, _) => "Process running".to_owned(),
        },
        ProcessStatus::Succeeded | ProcessStatus::Failed => match output.exit_code {
            Some(code) => format!("Process exited with code {code}"),
            None => match &output.failure {
                Some(failure) => format!("Process failed: {failure}"),
                None => "Process failed".to_owned(),
            },
        },
        ProcessStatus::TimedOut => "Process timed out and was killed".to_owned(),
        ProcessStatus::Killed => "Process killed".to_owned(),
    };
    text.push_str(&status);
    text.push('\n');
    if output.omitted_bytes > 0 {
        let delivered = output.stdout.bytes.len() + output.stderr.bytes.len();
        text.push_str(&format!(
            "Original byte count: {}\n",
            delivered as u64 + output.omitted_bytes
        ));
    }
    text.push_str("Output:");
    if !body.is_empty() {
        text.push('\n');
        text.push_str(&body);
    }
    text
}

fn format_seconds(duration: Duration) -> String {
    let text = format!("{:.4}", duration.as_secs_f64());
    let trimmed = text.trim_end_matches('0');
    if trimmed.ends_with('.') {
        format!("{trimmed}0")
    } else {
        trimmed.to_owned()
    }
}

/// Informational, never a scolding: what was left running, how it ends.
fn leftover_note(leftovers: &[LeftoverProcess]) -> Option<String> {
    if leftovers.is_empty() {
        return None;
    }
    let listing = leftovers
        .iter()
        .map(|member| {
            if member.command.is_empty() {
                format!("pid {}", member.pid)
            } else {
                format!("pid {} `{}`", member.pid, member.command)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(if leftovers.len() == 1 {
        format!(
            "[note: 1 process is still running after the command exited: {listing}. It keeps running until you stop it or the environment is closed or powered down.]"
        )
    } else {
        format!(
            "[note: {} processes are still running after the command exited: {listing}. They keep running until you stop them or the environment is closed or powered down.]",
            leftovers.len()
        )
    })
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

pub(crate) fn invalid_request(message: impl Into<String>) -> ToolError {
    ToolError::InvalidRequest {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::process::ProcessHandle;

    fn output(status: ProcessStatus, stdout: &str, stderr: &str) -> ProcessOutput {
        ProcessOutput {
            status,
            handle: (status == ProcessStatus::Running).then(|| ProcessHandle::new("proc-7")),
            pid: Some(91),
            exit_code: match status {
                ProcessStatus::Succeeded => Some(0),
                ProcessStatus::Failed => Some(2),
                _ => None,
            },
            failure: None,
            stdout: StreamOutput {
                bytes: stdout.as_bytes().to_vec(),
                omitted_at: None,
            },
            stderr: StreamOutput {
                bytes: stderr.as_bytes().to_vec(),
                omitted_at: None,
            },
            omitted_bytes: 0,
            leftover_processes: Vec::new(),
        }
    }

    #[test]
    fn canonical_text_carries_handle_pid_and_exit_state() {
        let running = process_visible_output(
            &output(ProcessStatus::Running, "building\n", ""),
            ProcessPresentation::Canonical,
        );
        assert_eq!(running, "building\n\n[running: handle proc-7, pid 91]");

        let exited = process_visible_output(
            &output(ProcessStatus::Failed, "", "boom"),
            ProcessPresentation::Canonical,
        );
        assert_eq!(exited, "boom\n[exited with code 2]");

        let killed = process_visible_output(
            &output(ProcessStatus::Killed, "", ""),
            ProcessPresentation::Canonical,
        );
        assert_eq!(killed, "[killed]");
        let timed_out = process_visible_output(
            &output(ProcessStatus::TimedOut, "", ""),
            ProcessPresentation::Canonical,
        );
        assert_eq!(timed_out, "[timed out]");
    }

    #[test]
    fn leftover_note_names_pid_and_command_and_never_blames_the_host() {
        let mut exited = output(ProcessStatus::Succeeded, "", "");
        exited.leftover_processes = vec![LeftoverProcess {
            pid: 91,
            command: "python -m http.server 8080".to_owned(),
        }];
        let text = process_visible_output(&exited, ProcessPresentation::Canonical);
        assert_eq!(
            text,
            "[exited with code 0]\n[note: 1 process is still running after the command exited: pid 91 `python -m http.server 8080`. It keeps running until you stop it or the environment is closed or powered down.]"
        );
        assert!(!text.contains("terminated"));

        exited.leftover_processes.push(LeftoverProcess {
            pid: 92,
            command: String::new(),
        });
        let text = process_visible_output(&exited, ProcessPresentation::Canonical);
        assert!(text.contains("2 processes are still running"));
        assert!(text.contains("pid 91 `python -m http.server 8080`, pid 92."));
        assert!(text.contains("They keep running until you stop them"));

        let clean = process_visible_output(
            &output(ProcessStatus::Succeeded, "", ""),
            ProcessPresentation::Canonical,
        );
        assert!(!clean.contains("[note:"));
    }

    #[test]
    fn omission_marker_sits_where_the_middle_was() {
        let mut exited = output(ProcessStatus::Succeeded, "headtail", "");
        exited.omitted_bytes = 4096;
        exited.stdout.omitted_at = Some(4);
        let text = process_visible_output(&exited, ProcessPresentation::Canonical);
        assert_eq!(
            text,
            "head\n[omitted 4096 bytes]\ntail\n[exited with code 0]"
        );

        let codex = process_visible_output(
            &exited,
            ProcessPresentation::Codex {
                wall_time: Duration::from_millis(3200),
            },
        );
        assert_eq!(
            codex,
            "Wall time: 3.2 seconds\nProcess exited with code 0\nOriginal byte count: 4104\nOutput:\nhead\n[omitted 4096 bytes]\ntail"
        );
    }

    #[test]
    fn codex_header_matches_codex_wording() {
        let running = process_visible_output(
            &output(ProcessStatus::Running, "line\n", ""),
            ProcessPresentation::Codex {
                wall_time: Duration::from_micros(10_001_200),
            },
        );
        assert_eq!(
            running,
            "Wall time: 10.0012 seconds\nProcess running with session ID proc-7, pid 91\nOutput:\nline\n"
        );
        let exited = process_visible_output(
            &output(ProcessStatus::Succeeded, "", ""),
            ProcessPresentation::Codex {
                wall_time: Duration::from_secs(1),
            },
        );
        assert_eq!(
            exited,
            "Wall time: 1.0 seconds\nProcess exited with code 0\nOutput:"
        );
    }

    #[test]
    fn claude_code_texts_name_the_companion_tools() {
        let background = process_visible_output(
            &output(ProcessStatus::Running, "", ""),
            ProcessPresentation::ClaudeBash { background: true },
        );
        assert_eq!(
            background,
            "Command running in background with ID: proc-7. Use BashOutput to read its output and KillShell to stop it."
        );
        let still_running = process_visible_output(
            &output(ProcessStatus::Running, "more\n", ""),
            ProcessPresentation::ClaudeBashOutput,
        );
        assert_eq!(still_running, "more\n\n[still running]");
        let finished = process_visible_output(
            &output(ProcessStatus::Succeeded, "done", ""),
            ProcessPresentation::ClaudeBashOutput,
        );
        assert_eq!(finished, "done\n[exited with code 0]");
        let killed = process_visible_output(
            &output(ProcessStatus::Killed, "partial", ""),
            ProcessPresentation::ClaudeKillShell,
        );
        assert_eq!(killed, "partial\n[killed]");
    }
}

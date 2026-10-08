//! Structured process results for tools, independent of the raw process protocol.

use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    environment::process::{LeftoverProcess, ProcessHandle, ProcessOutput, ProcessStatus},
    error::ToolResult,
    runtime::{ToolInvocationOutput, encode_output},
};

use super::shared::{ProcessPresentation, process_visible_output};

/// Each stream contains UTF-8 text, or lossless bytes when UTF-8 decoding fails.
/// Exactly one representation is present per stream; empty output is a string.
#[derive(Serialize, JsonSchema)]
#[schemars(extend("allOf" = [
    {"oneOf": [{"required": ["stdout"]}, {"required": ["stdout_bytes"]}]},
    {"oneOf": [{"required": ["stderr"]}, {"required": ["stderr_bytes"]}]}
]))]
pub(super) struct ProcessToolOutput {
    status: ProcessStatus,
    /// Present while the process is still running.
    handle: Option<ProcessHandle>,
    /// OS pid of the root process, which is also its process group id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
    /// UTF-8 stdout. Absent when stdout_bytes is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    stdout: Option<String>,
    /// Raw stdout, present only when the retained output is not valid UTF-8.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<u8>")]
    stdout_bytes: Option<Vec<u8>>,
    /// UTF-8 stderr. Absent when stderr_bytes is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    stderr: Option<String>,
    /// Raw stderr, present only when the retained output is not valid UTF-8.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<u8>")]
    stderr_bytes: Option<Vec<u8>>,
    /// Bytes dropped from the middle of the output because more was
    /// produced than the environment retains between reads.
    #[serde(skip_serializing_if = "is_zero")]
    omitted_bytes: u64,
    /// Byte offset in retained stdout before which omitted_bytes were dropped.
    /// This is a UTF-8 byte offset even when stdout is a string.
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout_omitted_at: Option<usize>,
    /// Byte offset in retained stderr before which omitted_bytes were dropped.
    /// This is a UTF-8 byte offset even when stderr is a string.
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr_omitted_at: Option<usize>,
    /// Processes still running in the command's group after it exited.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    leftover_processes: Vec<LeftoverProcess>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn decode_stream(bytes: Vec<u8>) -> (Option<String>, Option<Vec<u8>>) {
    match String::from_utf8(bytes) {
        Ok(text) => (Some(text), None),
        Err(error) => (None, Some(error.into_bytes())),
    }
}

impl From<ProcessOutput> for ProcessToolOutput {
    fn from(output: ProcessOutput) -> Self {
        let (stdout, stdout_bytes) = decode_stream(output.stdout.bytes);
        let (stderr, stderr_bytes) = decode_stream(output.stderr.bytes);
        Self {
            status: output.status,
            handle: output.handle,
            pid: output.pid,
            exit_code: output.exit_code,
            failure: output.failure,
            stdout,
            stdout_bytes,
            stderr,
            stderr_bytes,
            omitted_bytes: output.omitted_bytes,
            stdout_omitted_at: output.stdout.omitted_at,
            stderr_omitted_at: output.stderr.omitted_at,
            leftover_processes: output.leftover_processes,
        }
    }
}

pub(super) fn encode_process_output(
    output: ProcessOutput,
    presentation: ProcessPresentation,
) -> ToolResult<ToolInvocationOutput> {
    let visible = process_visible_output(&output, presentation);
    encode_output(&ProcessToolOutput::from(output), visible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::process::StreamOutput;
    use serde_json::json;

    fn output(stdout: &[u8], stderr: &[u8]) -> ProcessOutput {
        ProcessOutput {
            status: ProcessStatus::Succeeded,
            handle: None,
            pid: None,
            exit_code: Some(0),
            failure: None,
            stdout: StreamOutput {
                bytes: stdout.to_vec(),
                omitted_at: None,
            },
            stderr: StreamOutput {
                bytes: stderr.to_vec(),
                omitted_at: None,
            },
            omitted_bytes: 0,
            leftover_processes: Vec::new(),
        }
    }

    #[test]
    fn streams_decode_independently_and_losslessly() {
        let streams: &[(&[u8], Option<&str>)] = &[
            (b"", Some("")),
            ("héllo 🌍\n".as_bytes(), Some("héllo 🌍\n")),
            (b"a\0b", Some("a\0b")),
            (&[255, 254, 0], None),
            (&[0xe2, 0x82], None), // A UTF-8 sequence split by a read or truncation.
        ];
        for (stdout, stdout_text) in streams {
            for (stderr, stderr_text) in streams {
                let result =
                    encode_process_output(output(stdout, stderr), ProcessPresentation::Canonical)
                        .expect("encode process result");
                let mut expected = json!({"status": "succeeded", "handle": null, "exit_code": 0});
                for (name, bytes, text) in [
                    ("stdout", stdout, stdout_text),
                    ("stderr", stderr, stderr_text),
                ] {
                    if let Some(text) = text {
                        expected[name] = json!(text);
                    } else {
                        expected[format!("{name}_bytes")] = json!(bytes);
                    }
                }
                assert_eq!(result.output_json, expected);
            }
        }
    }

    #[test]
    fn metadata_preserves_byte_offsets_and_failed_process_details() {
        let mut raw = output("étail".as_bytes(), &[255, 254, 0]);
        raw.status = ProcessStatus::Failed;
        raw.handle = Some(ProcessHandle::new("proc-1"));
        raw.pid = Some(11);
        raw.exit_code = Some(2);
        raw.failure = Some("failed".into());
        raw.omitted_bytes = 4096;
        raw.stdout.omitted_at = Some(2);
        raw.stderr.omitted_at = Some(1);
        raw.leftover_processes.push(LeftoverProcess {
            pid: 12,
            command: "child".into(),
        });
        let result = encode_process_output(raw, ProcessPresentation::Canonical).unwrap();
        assert_eq!(
            result.output_json,
            json!({
                "status": "failed", "handle": "proc-1", "pid": 11, "exit_code": 2,
                "failure": "failed", "stdout": "étail", "stderr_bytes": [255, 254, 0],
                "omitted_bytes": 4096, "stdout_omitted_at": 2, "stderr_omitted_at": 1,
                "leftover_processes": [{"pid": 12, "command": "child"}],
            })
        );
        assert_eq!(
            result.model_visible_text,
            "é\n[omitted 4096 bytes]\ntail\n�\n[omitted 4096 bytes]\n�\0\n[exited with code 2]\n[note: 1 process is still running after the command exited: pid 12 `child`. It keeps running until you stop it or the environment is closed or powered down.]"
        );
    }
}

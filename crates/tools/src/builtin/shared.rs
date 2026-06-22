//! Shared helpers for built-in tool surfaces and operations.

use serde_json::{Value, json};

use crate::{environment::process::ProcessOutput, error::ToolError};

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

pub(crate) fn invalid_request(message: impl Into<String>) -> ToolError {
    ToolError::InvalidRequest {
        message: message.into(),
    }
}

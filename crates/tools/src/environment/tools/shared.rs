//! Shared helpers for environment action tools.

use crate::error::ToolError;

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

pub(crate) fn unsupported_process_capability() -> ToolError {
    unsupported_capability(
        "process execution is not available in the active environment; file tools may still work through fs:session, but process tools require an active env target with process capability",
    )
}

pub(crate) fn unsupported_job_capability() -> ToolError {
    unsupported_capability(
        "durable jobs are not available in the active environment; job tools require an active env target with job capability",
    )
}

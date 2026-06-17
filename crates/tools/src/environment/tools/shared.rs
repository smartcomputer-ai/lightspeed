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

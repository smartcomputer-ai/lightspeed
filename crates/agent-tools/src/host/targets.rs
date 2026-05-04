//! Host execution target resolution.

use std::collections::BTreeMap;

use agent_core::ToolExecutionTarget;

use crate::{
    error::{ToolError, ToolResult},
    host::context::HostToolContext,
};

pub const HOST_TARGET_NAMESPACE: &str = "host";
pub const LOCAL_HOST_TARGET_ID: &str = "local";

#[derive(Clone, Default)]
pub struct HostToolTargets {
    targets: BTreeMap<String, HostToolContext>,
}

impl HostToolTargets {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn execution_target(id: impl Into<String>) -> ToolExecutionTarget {
        ToolExecutionTarget::new(HOST_TARGET_NAMESPACE, id)
    }

    pub fn local_execution_target() -> ToolExecutionTarget {
        Self::execution_target(LOCAL_HOST_TARGET_ID)
    }

    pub fn local(ctx: HostToolContext) -> Self {
        Self::new().with_target(LOCAL_HOST_TARGET_ID, ctx)
    }

    pub fn with_target(mut self, id: impl Into<String>, ctx: HostToolContext) -> Self {
        self.insert(id, ctx);
        self
    }

    pub fn insert(&mut self, id: impl Into<String>, ctx: HostToolContext) {
        self.targets.insert(id.into(), ctx);
    }

    pub fn get(&self, id: &str) -> Option<&HostToolContext> {
        self.targets.get(id)
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    pub fn error_context(&self) -> Option<&HostToolContext> {
        self.targets.values().next()
    }

    pub fn resolve(&self, target: &ToolExecutionTarget) -> ToolResult<&HostToolContext> {
        target
            .validate()
            .map_err(|error| ToolError::InvalidRequest {
                message: format!("invalid host execution target: {error}"),
            })?;
        if target.namespace != HOST_TARGET_NAMESPACE {
            return Err(ToolError::InvalidRequest {
                message: format!(
                    "host execution target must use namespace {HOST_TARGET_NAMESPACE}, got {}",
                    target.namespace
                ),
            });
        }
        self.targets
            .get(&target.id)
            .ok_or_else(|| ToolError::InvalidRequest {
                message: format!("unknown host execution target id {}", target.id),
            })
    }
}

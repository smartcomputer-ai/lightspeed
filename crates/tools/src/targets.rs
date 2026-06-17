//! Semantic tool execution target namespaces and runtime target registry.

use std::{collections::BTreeMap, sync::Arc};

use engine::{ToolEffect, ToolExecutionTarget, storage::BlobStore};

use crate::{
    environment::EnvironmentToolContext,
    error::{ToolError, ToolResult},
    fs::FsToolContext,
    limits::ToolLimits,
};

pub const FS_TARGET_NAMESPACE: &str = "fs";
pub const ENV_TARGET_NAMESPACE: &str = "env";

pub const SESSION_FS_TARGET_ID: &str = "session";
pub const LOCAL_ENV_TARGET_ID: &str = "local";

pub fn session_fs_target() -> ToolExecutionTarget {
    ToolExecutionTarget::new(FS_TARGET_NAMESPACE, SESSION_FS_TARGET_ID)
}

pub fn environment_target(id: impl Into<String>) -> ToolExecutionTarget {
    ToolExecutionTarget::new(ENV_TARGET_NAMESPACE, id)
}

pub fn local_environment_target() -> ToolExecutionTarget {
    environment_target(LOCAL_ENV_TARGET_ID)
}

#[derive(Clone, Copy)]
pub enum ResolvedToolContext<'a> {
    Filesystem(&'a FsToolContext),
    Environment(&'a EnvironmentToolContext),
}

impl<'a> ResolvedToolContext<'a> {
    pub fn filesystem(self) -> ToolResult<&'a FsToolContext> {
        match self {
            Self::Filesystem(ctx) => Ok(ctx),
            Self::Environment(_) => Err(ToolError::InvalidRequest {
                message: "filesystem tool requires an fs execution target".to_owned(),
            }),
        }
    }

    pub fn environment(self) -> ToolResult<&'a EnvironmentToolContext> {
        match self {
            Self::Environment(ctx) => Ok(ctx),
            Self::Filesystem(_) => Err(ToolError::InvalidRequest {
                message: "environment tool requires an env execution target".to_owned(),
            }),
        }
    }

    pub fn blobs(self) -> &'a Arc<dyn BlobStore> {
        match self {
            Self::Filesystem(ctx) => &ctx.blobs,
            Self::Environment(ctx) => &ctx.blobs,
        }
    }

    pub fn limits(self) -> ToolLimits {
        match self {
            Self::Filesystem(ctx) => ctx.limits,
            Self::Environment(ctx) => ctx.limits,
        }
    }

    pub fn drain_tool_effects(self) -> Vec<ToolEffect> {
        match self {
            Self::Filesystem(ctx) => ctx.fs.drain_tool_effects(),
            Self::Environment(_) => Vec::new(),
        }
    }
}

#[derive(Clone, Default)]
pub struct ToolTargets {
    fs_targets: BTreeMap<String, FsToolContext>,
    environment_targets: BTreeMap<String, EnvironmentToolContext>,
}

impl ToolTargets {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fs_execution_target(id: impl Into<String>) -> ToolExecutionTarget {
        ToolExecutionTarget::new(FS_TARGET_NAMESPACE, id)
    }

    pub fn session_fs_execution_target() -> ToolExecutionTarget {
        Self::fs_execution_target(SESSION_FS_TARGET_ID)
    }

    pub fn environment_execution_target(id: impl Into<String>) -> ToolExecutionTarget {
        ToolExecutionTarget::new(ENV_TARGET_NAMESPACE, id)
    }

    pub fn local_environment_execution_target() -> ToolExecutionTarget {
        Self::environment_execution_target(LOCAL_ENV_TARGET_ID)
    }

    pub fn insert_fs_context(&mut self, id: impl Into<String>, ctx: FsToolContext) {
        self.fs_targets.insert(id.into(), ctx);
    }

    pub fn with_fs_context(mut self, id: impl Into<String>, ctx: FsToolContext) -> Self {
        self.insert_fs_context(id, ctx);
        self
    }

    pub fn with_session_fs_context(self, ctx: FsToolContext) -> Self {
        self.with_fs_context(SESSION_FS_TARGET_ID, ctx)
    }

    pub fn insert_environment_context(
        &mut self,
        id: impl Into<String>,
        ctx: EnvironmentToolContext,
    ) {
        self.environment_targets.insert(id.into(), ctx);
    }

    pub fn with_environment_context(
        mut self,
        id: impl Into<String>,
        ctx: EnvironmentToolContext,
    ) -> Self {
        self.insert_environment_context(id, ctx);
        self
    }

    pub fn with_local_environment_context(self, ctx: EnvironmentToolContext) -> Self {
        self.with_environment_context(LOCAL_ENV_TARGET_ID, ctx)
    }

    pub fn get(&self, id: &str) -> Option<&FsToolContext> {
        self.fs_targets.get(id)
    }

    pub fn is_empty(&self) -> bool {
        self.fs_targets.is_empty() && self.environment_targets.is_empty()
    }

    pub fn blob_store(&self) -> Option<Arc<dyn BlobStore>> {
        self.fs_targets
            .values()
            .next()
            .map(|ctx| ctx.blobs.clone())
            .or_else(|| {
                self.environment_targets
                    .values()
                    .next()
                    .map(|ctx| ctx.blobs.clone())
            })
    }

    pub fn limits(&self) -> Option<ToolLimits> {
        self.fs_targets
            .values()
            .next()
            .map(|ctx| ctx.limits)
            .or_else(|| {
                self.environment_targets
                    .values()
                    .next()
                    .map(|ctx| ctx.limits)
            })
    }

    pub fn resolve(&self, target: &ToolExecutionTarget) -> ToolResult<ResolvedToolContext<'_>> {
        target
            .validate()
            .map_err(|error| ToolError::InvalidRequest {
                message: format!("invalid tool execution target: {error}"),
            })?;
        if !matches!(
            target.namespace.as_str(),
            FS_TARGET_NAMESPACE | ENV_TARGET_NAMESPACE
        ) {
            return Err(ToolError::InvalidRequest {
                message: format!(
                    "tool execution target namespace must be {FS_TARGET_NAMESPACE} or {ENV_TARGET_NAMESPACE}, got {}",
                    target.namespace,
                ),
            });
        }
        match target.namespace.as_str() {
            FS_TARGET_NAMESPACE => self.resolve_fs(&target.id),
            ENV_TARGET_NAMESPACE => self.resolve_environment(&target.id),
            _ => unreachable!("namespace checked above"),
        }
    }

    pub fn resolve_fs(&self, id: &str) -> ToolResult<ResolvedToolContext<'_>> {
        self.fs_targets
            .get(id)
            .map(ResolvedToolContext::Filesystem)
            .ok_or_else(|| ToolError::InvalidRequest {
                message: format!("unknown filesystem tool execution target id {id}"),
            })
    }

    pub fn resolve_environment(&self, id: &str) -> ToolResult<ResolvedToolContext<'_>> {
        self.environment_targets
            .get(id)
            .map(ResolvedToolContext::Environment)
            .ok_or_else(|| ToolError::InvalidRequest {
                message: format!(
                    "no execution environment target is configured for id {id}; file tools may still be available through fs:{SESSION_FS_TARGET_ID}, but process tools require an active environment"
                ),
            })
    }

    pub fn default_for_namespace(&self, namespace: &str) -> ToolResult<ResolvedToolContext<'_>> {
        match namespace {
            FS_TARGET_NAMESPACE => self.resolve_fs(SESSION_FS_TARGET_ID),
            ENV_TARGET_NAMESPACE => self.resolve_environment(LOCAL_ENV_TARGET_ID),
            _ => Err(ToolError::InvalidRequest {
                message: format!(
                    "tool execution target namespace must be {FS_TARGET_NAMESPACE} or {ENV_TARGET_NAMESPACE}, got {namespace}"
                ),
            }),
        }
    }
}

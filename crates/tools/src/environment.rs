//! Environment action tool context.

use std::sync::Arc;

use harness::storage::BlobStore;

use crate::{
    environment::{jobs::JobExecutor, process::ProcessExecutor},
    fs::{FsPath, FsToolContext},
    limits::ToolLimits,
};

pub mod attachments;
pub(crate) mod catalog_text;
pub mod control;
pub mod handles;
pub mod jobs;
pub mod output;
pub mod platform;
pub mod process;
pub mod projection;
pub mod sources;
pub mod tools;

#[derive(Clone)]
pub struct EnvironmentToolContext {
    pub platform: Option<environment_protocol::data::platform::PlatformFacts>,
    pub environment_id: Option<String>,
    pub filesystem: Option<FsToolContext>,
    pub transfer: Option<crate::transfer::SharedEnvironmentTransfer>,
    pub process: Option<Arc<dyn ProcessExecutor>>,
    pub jobs: Option<Arc<dyn JobExecutor>>,
    pub blobs: Arc<dyn BlobStore>,
    pub limits: ToolLimits,
    pub process_cwd: Option<FsPath>,
    pub session_id: Option<String>,
    /// Stable invocation identity used by resumable content transfers.
    pub operation_id: Option<String>,
}

impl EnvironmentToolContext {
    pub fn new(process: Option<Arc<dyn ProcessExecutor>>, blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            platform: None,
            environment_id: None,
            filesystem: None,
            transfer: None,
            process,
            jobs: None,
            blobs,
            limits: ToolLimits::default(),
            process_cwd: None,
            session_id: None,
            operation_id: None,
        }
    }

    pub fn with_environment_id(mut self, environment_id: impl Into<String>) -> Self {
        self.environment_id = Some(environment_id.into());
        self
    }

    pub fn shell_argv(
        &self,
        command: &str,
        options: environment_client::shell::ShellOptions,
    ) -> crate::error::ToolResult<Vec<String>> {
        if let Some(platform) = &self.platform {
            return environment_client::shell::command_argv(platform, command, options).map_err(
                |error| match error {
                    environment_client::shell::ShellError::NulCommand => {
                        crate::error::ToolError::InvalidRequest {
                            message: error.to_string(),
                        }
                    }
                    _ => crate::error::ToolError::UnsupportedCapability {
                        message: error.to_string(),
                    },
                },
            );
        }
        if options
            .shell
            .is_some_and(|kind| kind != environment_protocol::data::platform::ShellKind::Bash)
        {
            return Err(crate::error::ToolError::UnsupportedCapability {
                message: "shell selection requires remote platform discovery".into(),
            });
        }
        Ok(vec![
            "bash".into(),
            if options.login.unwrap_or(true) {
                "-lc"
            } else {
                "-c"
            }
            .into(),
            command.into(),
        ])
    }

    pub fn resolve_cwd(&self, value: &str) -> crate::error::ToolResult<FsPath> {
        if let Some(fs) = &self.filesystem {
            return fs
                .fs
                .resolve_input_path(self.process_cwd.as_ref(), value)
                .map_err(Into::into);
        }
        if let (Some(platform), Some(base)) = (&self.platform, &self.process_cwd) {
            let base = environment_protocol::paths::EnvironmentPath::from_native(
                base.as_str(),
                platform.path_convention,
            )
            .map_err(|e| crate::error::ToolError::InvalidRequest {
                message: e.to_string(),
            })?;
            return environment_client::paths::resolve(&base, value)
                .map(FsPath::from_environment)
                .map_err(|e| crate::error::ToolError::InvalidRequest {
                    message: e.to_string(),
                });
        }
        let path = FsPath::new(value).map_err(crate::fs::FsError::from)?;
        if path.is_absolute() {
            return Ok(path);
        }
        self.process_cwd.as_ref().map_or(Ok(path.clone()), |base| {
            base.join_path(&path)
                .map_err(crate::fs::FsError::from)
                .map_err(Into::into)
        })
    }

    pub fn with_operation_id(mut self, operation_id: impl Into<String>) -> Self {
        self.operation_id = Some(operation_id.into());
        self
    }

    pub fn with_filesystem(mut self, filesystem: FsToolContext) -> Self {
        self.filesystem = Some(filesystem);
        self
    }

    pub fn with_jobs(mut self, jobs: Arc<dyn JobExecutor>) -> Self {
        self.jobs = Some(jobs);
        self
    }

    pub fn with_limits(mut self, limits: ToolLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_process_cwd(mut self, cwd: FsPath) -> Self {
        self.process_cwd = Some(cwd);
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

//! Environment action tool context.

use std::sync::Arc;

use engine::storage::BlobStore;

use crate::{environment::process::ProcessExecutor, fs::FsPath, limits::ToolLimits};

pub mod process;
pub mod projection;
pub mod tools;

#[derive(Clone)]
pub struct EnvironmentToolContext {
    pub process: Option<Arc<dyn ProcessExecutor>>,
    pub blobs: Arc<dyn BlobStore>,
    pub limits: ToolLimits,
    pub process_cwd: Option<FsPath>,
}

impl EnvironmentToolContext {
    pub fn new(process: Option<Arc<dyn ProcessExecutor>>, blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            process,
            blobs,
            limits: ToolLimits::default(),
            process_cwd: None,
        }
    }

    pub fn with_limits(mut self, limits: ToolLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_process_cwd(mut self, cwd: FsPath) -> Self {
        self.process_cwd = Some(cwd);
        self
    }
}

//! Environment action tool context.

use std::sync::Arc;

use engine::storage::BlobStore;

use crate::{
    environment::{jobs::JobExecutor, process::ProcessExecutor},
    fs::FsPath,
    limits::ToolLimits,
};

pub mod jobs;
pub mod process;
pub mod projection;
pub mod tools;

#[derive(Clone)]
pub struct EnvironmentToolContext {
    pub process: Option<Arc<dyn ProcessExecutor>>,
    pub jobs: Option<Arc<dyn JobExecutor>>,
    pub blobs: Arc<dyn BlobStore>,
    pub limits: ToolLimits,
    pub process_cwd: Option<FsPath>,
    pub session_id: Option<String>,
}

impl EnvironmentToolContext {
    pub fn new(process: Option<Arc<dyn ProcessExecutor>>, blobs: Arc<dyn BlobStore>) -> Self {
        Self {
            process,
            jobs: None,
            blobs,
            limits: ToolLimits::default(),
            process_cwd: None,
            session_id: None,
        }
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

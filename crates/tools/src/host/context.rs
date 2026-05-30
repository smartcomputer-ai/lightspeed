//! Shared tool execution context.

use std::sync::Arc;

use engine::storage::BlobStore;

use crate::host::{fs::FileSystem, fs::FsPath, process::ProcessExecutor};

#[derive(Clone)]
pub struct HostToolContext {
    pub fs: Arc<dyn FileSystem>,
    pub process: Option<Arc<dyn ProcessExecutor>>,
    pub blobs: Arc<dyn BlobStore>,
    pub limits: HostToolLimits,
    pub cwd: Option<FsPath>,
}

impl HostToolContext {
    pub fn new(
        fs: Arc<dyn FileSystem>,
        process: Option<Arc<dyn ProcessExecutor>>,
        blobs: Arc<dyn BlobStore>,
    ) -> Self {
        Self {
            fs,
            process,
            blobs,
            limits: HostToolLimits::default(),
            cwd: None,
        }
    }

    pub fn with_limits(mut self, limits: HostToolLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_cwd(mut self, cwd: FsPath) -> Self {
        self.cwd = Some(cwd);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostToolLimits {
    pub max_file_read_bytes: u64,
    pub max_model_visible_output_bytes: u64,
    pub max_process_output_bytes: u64,
    pub default_process_timeout_ms: u64,
}

impl Default for HostToolLimits {
    fn default() -> Self {
        Self {
            max_file_read_bytes: 512 * 1024 * 1024,
            max_model_visible_output_bytes: 64 * 1024,
            max_process_output_bytes: 512 * 1024,
            default_process_timeout_ms: 60_000,
        }
    }
}

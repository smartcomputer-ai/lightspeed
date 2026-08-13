pub mod config;
pub mod filesystem;
pub mod jobs;
pub mod process;
mod process_group;
pub mod rpc;
pub mod server;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use host_protocol::shared::{
    CURRENT_PROTOCOL_VERSION, HostCapabilities, HostConnectionId, ImplementationInfo,
};

use crate::{
    config::DaemonConfig, filesystem::LocalFileSystem, jobs::JobManager, process::ProcessManager,
};

#[derive(Clone)]
pub struct DaemonRuntime {
    config: Arc<DaemonConfig>,
    capabilities: HostCapabilities,
    filesystem: LocalFileSystem,
    processes: ProcessManager,
    jobs: JobManager,
    next_connection_id: Arc<AtomicU64>,
}

impl DaemonRuntime {
    pub fn new(config: DaemonConfig) -> anyhow::Result<Self> {
        let capabilities = host_capabilities(&config);
        let filesystem = LocalFileSystem::new(
            config.fs_root.clone(),
            config.cwd.clone(),
            !config.read_only_fs,
        );
        let processes = ProcessManager::new(config.cwd.clone(), config.fs_root.clone());
        let jobs = JobManager::new(
            config.cwd.clone(),
            config.fs_root.clone(),
            config.state_dir.clone(),
        )?;
        Ok(Self {
            config: Arc::new(config),
            capabilities,
            filesystem,
            processes,
            jobs,
            next_connection_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn config(&self) -> &DaemonConfig {
        &self.config
    }

    pub fn implementation(&self) -> ImplementationInfo {
        ImplementationInfo {
            name: "lightspeed-envd".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }
    }

    pub fn capabilities(&self) -> HostCapabilities {
        self.capabilities.clone()
    }

    pub fn next_connection_id(&self) -> HostConnectionId {
        let id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        HostConnectionId::new(format!("envd-{id}"))
    }

    pub fn filesystem(&self) -> &LocalFileSystem {
        &self.filesystem
    }

    pub fn processes(&self) -> &ProcessManager {
        &self.processes
    }

    pub fn jobs(&self) -> &JobManager {
        &self.jobs
    }
}

fn host_capabilities(config: &DaemonConfig) -> HostCapabilities {
    HostCapabilities {
        filesystem_read: true,
        filesystem_write: !config.read_only_fs,
        filesystem_search: true,
        filesystem_glob: true,
        filesystem_ranged_read: true,
        process_start: true,
        process_stdin: true,
        process_terminate: true,
        process_output_polling: true,
        process_output_notifications: false,
        process_pty: true,
        job_start: true,
        job_list: true,
        job_read: true,
        job_cancel: true,
        job_wait_hint: false,
        job_dependencies: true,
        job_queue_keys: true,
        network: true,
    }
}

pub fn protocol_version() -> u32 {
    CURRENT_PROTOCOL_VERSION
}

pub mod config;
pub mod filesystem;
pub mod jobs;
pub mod process;
mod process_group;
pub mod rpc;
pub mod server;

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use host_protocol::{
    control::{
        handshake::ControllerCapabilities,
        targets::{HostTargetStatus, HostTargetSummary},
    },
    shared::{
        CURRENT_PROTOCOL_VERSION, HostCapabilities, HostConnectionId, HostConnectionSpec, HostPath,
        HostScope, HostTargetId, HostTransport, ImplementationInfo,
    },
};

use crate::{
    config::BridgeConfig, filesystem::LocalFileSystem, jobs::JobManager, process::ProcessManager,
};

#[derive(Clone)]
pub struct BridgeRuntime {
    config: Arc<BridgeConfig>,
    advertise_base_url: Arc<String>,
    capabilities: HostCapabilities,
    filesystem: LocalFileSystem,
    processes: ProcessManager,
    jobs: JobManager,
    closed: Arc<AtomicBool>,
    next_connection_id: Arc<AtomicU64>,
}

impl BridgeRuntime {
    pub fn new(config: BridgeConfig, local_addr: SocketAddr) -> anyhow::Result<Self> {
        let advertise_base_url = config.advertise_base_url(local_addr);
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
            advertise_base_url: Arc::new(advertise_base_url),
            capabilities,
            filesystem,
            processes,
            jobs,
            closed: Arc::new(AtomicBool::new(false)),
            next_connection_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn config(&self) -> &BridgeConfig {
        &self.config
    }

    pub fn controller_endpoint(&self) -> String {
        format!("{}/control", self.advertise_base_url)
    }

    pub fn data_endpoint(&self) -> String {
        format!(
            "{}/data?target={}",
            self.advertise_base_url, self.config.target_id
        )
    }

    pub fn implementation(&self) -> ImplementationInfo {
        ImplementationInfo {
            name: "host-bridge".to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }
    }

    pub fn controller_capabilities(&self) -> ControllerCapabilities {
        ControllerCapabilities {
            list_templates: false,
            list_targets: true,
            create_target: false,
            attach_target: true,
            get_target: true,
            close_target: true,
        }
    }

    pub fn capabilities(&self) -> HostCapabilities {
        self.capabilities.clone()
    }

    pub fn target_id(&self) -> HostTargetId {
        HostTargetId::new(self.config.target_id.clone())
    }

    pub fn target_summary(&self) -> anyhow::Result<HostTargetSummary> {
        Ok(HostTargetSummary {
            target_id: self.target_id(),
            display_name: Some(self.config.display_name()),
            status: if self.closed.load(Ordering::Acquire) {
                HostTargetStatus::Closed
            } else {
                HostTargetStatus::Ready
            },
            scope: HostScope::Default,
            capabilities: self.capabilities(),
            default_cwd: Some(self.host_cwd()?),
            metadata: {
                // Configured entries first; the bridge's own keys win.
                let mut metadata = self.config.metadata.clone();
                metadata.insert("kind".to_owned(), "attached_host".to_owned());
                metadata.insert(
                    "fsRoot".to_owned(),
                    self.config.fs_root.to_string_lossy().into_owned(),
                );
                metadata
            },
        })
    }

    pub fn connection_spec(&self) -> anyhow::Result<HostConnectionSpec> {
        Ok(HostConnectionSpec {
            target_id: self.target_id(),
            endpoint: self.data_endpoint(),
            transport: HostTransport::WebSocket,
            scope: HostScope::Default,
            default_cwd: Some(self.host_cwd()?),
            capabilities: self.capabilities(),
        })
    }

    pub fn next_connection_id(&self) -> HostConnectionId {
        let id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        HostConnectionId::new(format!("{}-{id}", self.config.target_id))
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

    pub fn mark_closed(&self) {
        self.closed.store(true, Ordering::Release);
    }

    fn host_cwd(&self) -> anyhow::Result<HostPath> {
        let cwd = self.config.cwd.to_string_lossy();
        Ok(HostPath::new(cwd.as_ref())?)
    }
}

fn host_capabilities(config: &BridgeConfig) -> HostCapabilities {
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
        process_pty: false,
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

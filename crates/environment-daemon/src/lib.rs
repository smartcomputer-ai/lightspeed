pub mod config;
pub mod filesystem;
pub mod identity;
pub mod jobs;
pub mod process;
mod process_group;
mod redaction;
pub mod registration;
pub mod rpc;
pub mod server;
pub mod upgrade;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use environment_protocol::{
    data::idle::IdleResponse,
    shared::{
        CURRENT_PROTOCOL_VERSION, EnvironmentCapabilities, EnvironmentConnectionId,
        ImplementationInfo,
    },
};

use crate::{
    config::DaemonConfig, filesystem::LocalFileSystem, jobs::JobManager, process::ProcessManager,
};

#[derive(Clone)]
pub struct DaemonRuntime {
    config: Arc<DaemonConfig>,
    capabilities: EnvironmentCapabilities,
    filesystem: LocalFileSystem,
    processes: ProcessManager,
    jobs: JobManager,
    next_connection_id: Arc<AtomicU64>,
    activity: Arc<ActivityClock>,
}

/// Monotonic record of the last data-plane activity. Deliberately not a
/// wall-clock timestamp: after a freeze or snapshot restore the guest clock
/// is stale, while `Instant` differences stay meaningful.
pub struct ActivityClock {
    last: Mutex<Instant>,
}

impl ActivityClock {
    pub fn new() -> Self {
        Self {
            last: Mutex::new(Instant::now()),
        }
    }

    pub fn touch(&self) {
        *self.last.lock().expect("activity clock poisoned") = Instant::now();
    }

    pub fn idle_for(&self) -> std::time::Duration {
        self.last.lock().expect("activity clock poisoned").elapsed()
    }
}

impl Default for ActivityClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Pick the process-level rustls provider once. The daemon's own gateway
/// dials name their provider explicitly; this covers `reqwest`, which does
/// not. A workspace build links both `ring` and `aws-lc-rs`, and without a
/// chosen default rustls panics on first use. Idempotent: a second install
/// is refused and ignored.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

impl DaemonRuntime {
    pub fn new(config: DaemonConfig) -> anyhow::Result<Self> {
        install_crypto_provider();
        let capabilities = environment_capabilities(&config);
        let filesystem = LocalFileSystem::new(
            config.fs_root.clone(),
            config.cwd.clone(),
            !config.read_only_fs,
        )
        .with_transfer_state_dir(config.state_dir.join("transfers"))
        .map_err(|e| anyhow::anyhow!("transfer state: {e:?}"))?;
        let processes = ProcessManager::new(config.cwd.clone(), config.fs_root.clone())
            .with_scrubbed_env(config.scrubbed_env.clone());
        let jobs = JobManager::new(
            config.cwd.clone(),
            config.fs_root.clone(),
            config.state_dir.clone(),
        )?
        .with_scrubbed_env(config.scrubbed_env.clone());
        Ok(Self {
            config: Arc::new(config),
            capabilities,
            filesystem,
            processes,
            jobs,
            next_connection_id: Arc::new(AtomicU64::new(1)),
            activity: Arc::new(ActivityClock::new()),
        })
    }

    /// Record one unit of data-plane activity.
    pub fn touch_activity(&self) {
        self.activity.touch();
    }

    /// Idle report for the power reaper: zero idle time while any process or
    /// job is still executing, otherwise the time since the last request.
    /// Live work keeps the clock fresh so a long-running job that finishes
    /// starts the idle countdown from its end, not its start. Leftover
    /// process groups are counted but are not work: a server waiting for
    /// requests must not keep the environment awake.
    pub async fn idle_report(&self) -> IdleResponse {
        let running_processes = self.processes.running_count().await;
        let running_jobs = self.jobs.running_count().await;
        let leftover_process_groups = self.processes.leftover_group_count();
        if running_processes > 0 || running_jobs > 0 {
            self.activity.touch();
            return IdleResponse {
                idle_for_ms: 0,
                running_processes,
                running_jobs,
                leftover_process_groups,
            };
        }
        IdleResponse {
            idle_for_ms: u64::try_from(self.activity.idle_for().as_millis()).unwrap_or(u64::MAX),
            running_processes,
            running_jobs,
            leftover_process_groups,
        }
    }

    pub fn config(&self) -> &DaemonConfig {
        &self.config
    }

    pub fn implementation(&self) -> ImplementationInfo {
        build_info().into()
    }

    pub fn capabilities(&self) -> EnvironmentCapabilities {
        self.capabilities.clone()
    }

    pub fn next_connection_id(&self) -> EnvironmentConnectionId {
        let id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        EnvironmentConnectionId::new(format!("envd-{id}"))
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

fn environment_capabilities(config: &DaemonConfig) -> EnvironmentCapabilities {
    EnvironmentCapabilities {
        filesystem_read: true,
        filesystem_write: !config.read_only_fs,
        filesystem_search: true,
        filesystem_glob: true,
        filesystem_ranged_read: true,
        filesystem_transfer: cfg!(any(target_os = "linux", target_os = "macos")),
        filesystem_scan: cfg!(any(target_os = "linux", target_os = "macos")),
        filesystem_capture: cfg!(any(target_os = "linux", target_os = "macos")),
        filesystem_materialize: cfg!(any(target_os = "linux", target_os = "macos"))
            && !config.read_only_fs,
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

/// The facts this binary reports about itself: to the gateway in every
/// handshake, and to an operator through `--print-build`. The protocol
/// version is the one that decides whether a gateway admits the daemon; the
/// rest is provenance.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub git_sha: &'static str,
    pub target: &'static str,
    pub protocol_version: u32,
}

pub fn build_info() -> BuildInfo {
    BuildInfo {
        name: "lightspeed-envd",
        version: release_info::VERSION,
        git_sha: release_info::GIT_SHA,
        target: release_info::TARGET,
        protocol_version: CURRENT_PROTOCOL_VERSION,
    }
}

impl From<BuildInfo> for ImplementationInfo {
    fn from(build: BuildInfo) -> Self {
        ImplementationInfo {
            name: build.name.to_owned(),
            version: Some(build.version.to_owned()),
            git_sha: Some(build.git_sha.to_owned()),
            target: Some(build.target.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use environment_protocol::{
        data::process::{
            ProcessSignal, ReadProcessParams, StartProcessParams, TerminateProcessParams,
        },
        shared::ProcessId,
    };

    use super::*;

    fn runtime() -> (tempfile::TempDir, DaemonRuntime) {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let runtime = DaemonRuntime::new(DaemonConfig {
            listen: Some("127.0.0.1:0".parse().expect("listen")),
            cwd: root.clone(),
            fs_root: root.clone(),
            state_dir: root.join("state"),
            read_only_fs: false,
            registration: None,
            scrubbed_env: vec!["LIGHTSPEED_ENVD_REGISTRATION_KEY".to_owned()],
        })
        .expect("runtime");
        (temp, runtime)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn idle_report_counts_running_work_and_resets_on_activity() {
        let (_temp, runtime) = runtime();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let quiet = runtime.idle_report().await;
        assert!(quiet.is_quiescent());
        assert!(quiet.idle_for_ms >= 30, "idle_for_ms={}", quiet.idle_for_ms);

        runtime.touch_activity();
        let touched = runtime.idle_report().await;
        assert!(
            touched.idle_for_ms < 30,
            "idle_for_ms={}",
            touched.idle_for_ms
        );

        let process_id = ProcessId::new("sleeper");
        runtime
            .processes()
            .start_process(StartProcessParams {
                process_id: process_id.clone(),
                argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 30".to_owned()],
                cwd: None,
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                stdin: None,
                timeout_ms: Some(60_000),
                tty: false,
            })
            .await
            .expect("start");
        let busy = runtime.idle_report().await;
        assert_eq!(busy.running_processes, 1);
        assert_eq!(busy.idle_for_ms, 0);
        assert!(!busy.is_quiescent());

        runtime
            .processes()
            .terminate_process(TerminateProcessParams {
                process_id,
                signal: ProcessSignal::Kill,
            })
            .await
            .expect("terminate");
        for _ in 0..100 {
            if runtime.idle_report().await.running_processes == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let after = runtime.idle_report().await;
        assert_eq!(after.running_processes, 0);
        assert!(after.idle_for_ms < 5_000);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn idle_report_counts_leftover_groups_without_treating_them_as_busy() {
        let (_temp, runtime) = runtime();
        let process_id = ProcessId::new("service");
        runtime
            .processes()
            .start_process(StartProcessParams {
                process_id: process_id.clone(),
                argv: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "nohup sleep 30 >/dev/null 2>&1 & exit 0".to_owned(),
                ],
                cwd: None,
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                stdin: None,
                timeout_ms: Some(60_000),
                tty: false,
            })
            .await
            .expect("start");
        let output = runtime
            .processes()
            .read_process(ReadProcessParams {
                process_id: process_id.clone(),
                after_seq: None,
                max_bytes: None,
                wait_ms: None,
            })
            .await
            .expect("read");
        assert!(output.exited);
        assert_eq!(output.leftover_processes.len(), 1);

        let report = runtime.idle_report().await;
        assert_eq!(report.running_processes, 0);
        assert_eq!(report.leftover_process_groups, 1);
        assert!(report.is_quiescent(), "leftovers are not running work");

        runtime
            .processes()
            .terminate_process(TerminateProcessParams {
                process_id,
                signal: ProcessSignal::Kill,
            })
            .await
            .expect("kill leftovers");
        for _ in 0..100 {
            if runtime.idle_report().await.leftover_process_groups == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(runtime.idle_report().await.leftover_process_groups, 0);
    }
}

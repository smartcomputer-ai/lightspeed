pub mod config;
pub mod dispatch;
mod exec;
pub mod filesystem;
pub mod identity;
pub mod jobs;
pub mod process;
mod process_group;
mod redaction;
pub mod registration;
pub mod server;
pub mod upgrade;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::Context as _;
use environment_protocol::{
    ENVD_PROTOCOL_VERSION,
    bootstrap::BuildFacts,
    bytes::encode_hex,
    data::{
        handshake::{
            DataCapabilities, DataLimits, FilesystemCapabilities, FilesystemLimits, GlobCeilings,
            InitializeResponse, JobCapabilities, JobLimits, ProcessCapabilities, ProcessLimits,
            ScanCeilings, SearchCeilings, TransferCeilings,
        },
        idle::GetIdleResponse,
    },
    error::{Capability, ProtocolError},
    handshake::{ImplementationInfo, Plane},
    ids::DaemonInstanceId,
    paths::EnvironmentPath,
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::DaemonConfig,
    filesystem::{Confinement, FilesystemConfig, LocalFileSystem},
    jobs::{JobConfig, JobManager},
    process::{ProcessConfig, ProcessManager},
};

/// Product name in `serverInfo`, the registration `implementation` and
/// `--print-build`.
pub const DAEMON_NAME: &str = "lightspeed-envd";

/// Everything one daemon serves. Cheap to clone; every clone shares the
/// same managers, activity clock and shutdown token.
#[derive(Clone)]
pub struct DaemonRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    config: DaemonConfig,
    instance_id: DaemonInstanceId,
    server_info: ImplementationInfo,
    capabilities: DataCapabilities,
    limits: DataLimits,
    default_cwd: EnvironmentPath,
    home_directory: Option<EnvironmentPath>,
    confinement: Arc<Confinement>,
    filesystem: LocalFileSystem,
    processes: ProcessManager,
    jobs: JobManager,
    next_connection_id: AtomicU64,
    activity: ActivityClock,
    shutdown: CancellationToken,
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
        *self
            .last
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Instant::now();
    }

    pub fn idle_for(&self) -> std::time::Duration {
        self.last
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .elapsed()
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
    /// Build the managers over one shared [`Confinement`]. A missing state
    /// directory is created `0700`.
    pub fn new(mut config: DaemonConfig) -> anyhow::Result<Self> {
        install_crypto_provider();
        config.state_dir = config::prepare_state_dir(&config.state_dir)?;
        let default_cwd = environment_path(&config.cwd).context("cwd")?;
        let home_directory = home_directory(&config);
        let instance_id = mint_instance_id()?;
        let capabilities = data_capabilities(&config);
        let limits = data_limits();
        let confinement = Arc::new(
            Confinement::new(config.fs_root.clone(), config.state_dir.clone())
                .map_err(|error| anyhow::anyhow!("filesystem confinement: {error}"))?,
        );
        let filesystem = LocalFileSystem::new(FilesystemConfig {
            confinement: confinement.clone(),
            cwd: config.cwd.clone(),
            writable: !config.read_only_fs,
            limits: limits.filesystem.clone(),
        })
        .map_err(|error| anyhow::anyhow!("filesystem: {error}"))?;
        let processes = ProcessManager::new(ProcessConfig {
            cwd: config.cwd.clone(),
            confinement: confinement.clone(),
            state_dir: config.state_dir.clone(),
            scrubbed_env: config.scrubbed_env.clone(),
            instance_id: instance_id.clone(),
            limits: limits.process.clone(),
            max_wait_ms: limits.max_wait_ms,
            pty_supported: PTY_SUPPORTED,
        })
        .context("process manager")?;
        let jobs = JobManager::new(JobConfig {
            cwd: config.cwd.clone(),
            confinement: confinement.clone(),
            state_dir: config.state_dir.clone(),
            scrubbed_env: config.scrubbed_env.clone(),
            instance_id: instance_id.clone(),
            limits: limits.job.clone(),
            max_wait_ms: limits.max_wait_ms,
        })
        .context("job manager")?;
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                config,
                instance_id,
                server_info: server_info(),
                capabilities,
                limits,
                default_cwd,
                home_directory,
                confinement,
                filesystem,
                processes,
                jobs,
                next_connection_id: AtomicU64::new(1),
                activity: ActivityClock::new(),
                shutdown: CancellationToken::new(),
            }),
        })
    }

    /// Record one unit of data-plane activity.
    pub fn touch_activity(&self) {
        self.inner.activity.touch();
    }

    /// `env/getIdle`: zero idle time while any process or job is still
    /// executing or any leftover process group has live members, otherwise
    /// the time since the last `use` request. Live work keeps the clock
    /// fresh so a long-running job that finishes starts the idle countdown
    /// from its end, not its start. A leftover group is a service a command
    /// started and left running, such as a dev server, so it holds the
    /// environment awake like running work.
    pub async fn idle_report(&self) -> GetIdleResponse {
        let running_process_count = self.inner.processes.running_count().await;
        let running_job_count = self.inner.jobs.running_count().await;
        let leftover_process_group_count = self.inner.processes.leftover_group_count();
        let working =
            running_process_count > 0 || running_job_count > 0 || leftover_process_group_count > 0;
        if working {
            self.inner.activity.touch();
        }
        let idle_for_ms = if working {
            0
        } else {
            u64::try_from(self.inner.activity.idle_for().as_millis()).unwrap_or(u64::MAX)
        };
        GetIdleResponse {
            idle_for_ms,
            running_process_count,
            running_job_count,
            leftover_process_group_count,
        }
    }

    /// The data-plane `initialize` response; identical on every connection
    /// of this daemon instance.
    pub fn initialize_response(&self) -> InitializeResponse {
        InitializeResponse {
            protocol_version: ENVD_PROTOCOL_VERSION,
            plane: Plane::Data,
            daemon_instance_id: self.inner.instance_id.clone(),
            server_info: self.inner.server_info.clone(),
            capabilities: self.inner.capabilities.clone(),
            limits: self.inner.limits.clone(),
            default_cwd: self.inner.default_cwd.clone(),
            home_directory: self.inner.home_directory.clone(),
        }
    }

    /// `capabilityUnavailable` unless this daemon serves `capability`.
    pub fn require_capability(&self, capability: Capability) -> Result<(), ProtocolError> {
        if dispatch::has_capability(&self.inner.capabilities, capability) {
            return Ok(());
        }
        Err(ProtocolError::capability_unavailable(
            capability,
            format!("this daemon does not serve {}", capability_name(capability)),
        ))
    }

    pub fn config(&self) -> &DaemonConfig {
        &self.inner.config
    }

    pub fn instance_id(&self) -> &DaemonInstanceId {
        &self.inner.instance_id
    }

    pub fn server_info(&self) -> &ImplementationInfo {
        &self.inner.server_info
    }

    pub fn capabilities(&self) -> &DataCapabilities {
        &self.inner.capabilities
    }

    pub fn limits(&self) -> &DataLimits {
        &self.inner.limits
    }

    pub fn confinement(&self) -> &Arc<Confinement> {
        &self.inner.confinement
    }

    /// Daemon-unique, increasing connection number.
    pub fn next_connection_id(&self) -> u64 {
        self.inner
            .next_connection_id
            .fetch_add(1, Ordering::Relaxed)
    }

    /// Cancelled when the daemon begins a deliberate shutdown: listeners stop
    /// accepting, data connections end and the registration channel says
    /// goodbye.
    pub fn shutdown(&self) -> &CancellationToken {
        &self.inner.shutdown
    }

    pub fn filesystem(&self) -> &LocalFileSystem {
        &self.inner.filesystem
    }

    pub fn processes(&self) -> &ProcessManager {
        &self.inner.processes
    }

    pub fn jobs(&self) -> &JobManager {
        &self.inner.jobs
    }
}

/// PTYs are served wherever `portable-pty` has a Unix backend.
const PTY_SUPPORTED: bool = cfg!(unix);

fn data_capabilities(config: &DaemonConfig) -> DataCapabilities {
    DataCapabilities {
        filesystem: Some(FilesystemCapabilities {
            write: !config.read_only_fs,
        }),
        process: Some(ProcessCapabilities { pty: PTY_SUPPORTED }),
        job: Some(JobCapabilities {}),
    }
}

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;
const TIB: u64 = 1024 * GIB;

/// The ceilings envd enforces and advertises. Every response they allow
/// fits below `MAX_MESSAGE_BYTES` after base64.
pub fn data_limits() -> DataLimits {
    DataLimits {
        max_concurrent_requests: 32,
        max_wait_ms: 1_800_000,
        process: ProcessLimits {
            max_read_bytes: 4 * MIB,
            max_retained_output_bytes: MIB,
            retention_after_exit_ms: 60_000,
            unread_retention_ms: 7_200_000,
            max_exited_retained_bytes: 64 * MIB,
            terminate_grace_ms: 5_000,
            max_input_queue_bytes: MIB,
        },
        job: JobLimits {
            max_read_bytes: 4 * MIB,
            max_request_jobs: 64,
            max_retained_output_bytes: 2 * MIB,
            record_retention_ms: 604_800_000,
            cancel_grace_ms: 5_000,
        },
        filesystem: FilesystemLimits {
            max_read_file_bytes: 8 * MIB,
            max_write_file_bytes: 8 * MIB,
            max_directory_entries: 1_000,
            search: SearchCeilings {
                max_matches: 10_000,
                max_files: 100_000,
                max_bytes: GIB,
                max_duration_ms: 60_000,
                max_line_bytes: 4_096,
            },
            glob: GlobCeilings {
                max_matches: 100_000,
                max_files: 1_000_000,
                max_duration_ms: 60_000,
            },
            scan: ScanCeilings {
                max_roots: 32,
                max_patterns: 32,
                max_entries: 10_000,
                max_file_bytes: 256 * KIB,
                max_total_bytes: 4 * MIB,
                max_duration_ms: 30_000,
            },
            transfer: TransferCeilings {
                max_active_operations: 8,
                max_content_chunk_bytes: 4 * MIB,
                max_inventory_page_entries: 1_000,
                receipt_retention_ms: 86_400_000,
                max_entries: 100_000,
                max_depth: 64,
                max_file_bytes: TIB,
                max_total_bytes: TIB,
                max_manifest_bytes: 32 * MIB,
                max_duration_ms: 86_400_000,
            },
        },
    }
}

fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::Filesystem => "the filesystem family",
        Capability::FilesystemWrite => "filesystem writes",
        Capability::Process => "the process family",
        Capability::ProcessPty => "process PTYs",
        Capability::Job => "the job family",
        Capability::Power => "power control",
        Capability::Ingress => "ingress",
    }
}

/// `daemonInstanceId`: `envd_` and 32 random hex digits, minted per start.
fn mint_instance_id() -> anyhow::Result<DaemonInstanceId> {
    let mut bytes = [0u8; 16];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut bytes)
        .context("generate daemon instance id")?;
    Ok(DaemonInstanceId::new(format!(
        "envd_{}",
        encode_hex(&bytes)
    ))?)
}

fn environment_path(path: &std::path::Path) -> anyhow::Result<EnvironmentPath> {
    let text = path
        .to_str()
        .with_context(|| format!("{} is not UTF-8", path.display()))?;
    Ok(EnvironmentPath::new(text)?)
}

/// `$HOME` when it is a valid path inside the filesystem root.
fn home_directory(config: &DaemonConfig) -> Option<EnvironmentPath> {
    let home = std::env::var_os("HOME")?;
    let home = std::path::PathBuf::from(home);
    if !home.starts_with(&config.fs_root) {
        return None;
    }
    environment_path(&home).ok()
}

/// `serverInfo` and the registration `implementation`: this build's facts,
/// leaving out any the build left empty.
pub fn server_info() -> ImplementationInfo {
    let info = ImplementationInfo::new(DAEMON_NAME).expect("the daemon name is a valid field");
    let info = match info.clone().with_version(release_info::VERSION) {
        Ok(with) => with,
        Err(_) => info,
    };
    let info = match info.clone().with_git_sha(release_info::GIT_SHA) {
        Ok(with) => with,
        Err(_) => info,
    };
    match info.clone().with_target(release_info::TARGET) {
        Ok(with) => with,
        Err(_) => info,
    }
}

/// What `--print-build` prints: the facts an orchestrator checks a
/// downloaded daemon against the deployment's discovery document with.
pub fn build_facts() -> BuildFacts {
    BuildFacts {
        name: DAEMON_NAME.to_owned(),
        version: release_info::VERSION.to_owned(),
        git_sha: release_info::GIT_SHA.to_owned(),
        target: release_info::TARGET.to_owned(),
        protocol_version: ENVD_PROTOCOL_VERSION,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A runtime rooted in a fresh temporary directory, its state dir
    /// beside the root rather than inside it.
    pub(crate) fn runtime() -> (tempfile::TempDir, DaemonRuntime) {
        runtime_with(|_| {})
    }

    pub(crate) fn runtime_with(
        adjust: impl FnOnce(&mut DaemonConfig),
    ) -> (tempfile::TempDir, DaemonRuntime) {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical temp");
        let root = base.join("root");
        let state = base.join("state");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&state).expect("state");
        let mut config = DaemonConfig {
            listen: None,
            cwd: root.clone(),
            fs_root: root,
            state_dir: state,
            read_only_fs: false,
            registration: None,
            scrubbed_env: vec!["LIGHTSPEED_ENVD_REGISTRATION_KEY".to_owned()],
        };
        adjust(&mut config);
        let runtime = DaemonRuntime::new(config).expect("runtime");
        (temp, runtime)
    }
}

#[cfg(test)]
mod tests {
    use environment_protocol::{
        data::handshake::DataLimits,
        error::ProtocolErrorKind,
        limits::{MAX_MESSAGE_BYTES, MIN_CONCURRENT_REQUESTS},
        manifest::{EnvGetIdle, ProcessStart, ProcessTerminate},
    };
    use serde_json::json;

    use super::{test_support::runtime, *};
    use crate::dispatch::{RequestContext, handle_request};

    async fn call(
        runtime: &DaemonRuntime,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        handle_request(runtime, &RequestContext::detached(), method, params)
            .await
            .unwrap_or_else(|error| panic!("{method}: {error}"))
            .value
    }

    #[test]
    fn instance_ids_are_random_per_start_and_use_the_general_grammar() {
        let first = mint_instance_id().unwrap();
        let second = mint_instance_id().unwrap();
        assert_ne!(first, second);
        assert!(first.as_str().starts_with("envd_"));
        assert_eq!(first.as_str().len(), "envd_".len() + 32);
    }

    /// Every advertised ceiling keeps its worst-case response, base64
    /// included, below the message limit.
    #[test]
    fn advertised_payload_ceilings_fit_the_message_limit() {
        let limits: DataLimits = data_limits();
        let base64 = |bytes: u64| bytes.div_ceil(3) * 4;
        for (name, bytes) in [
            ("fs.readFile", limits.filesystem.max_read_file_bytes),
            ("fs.writeFile", limits.filesystem.max_write_file_bytes),
            (
                "transfer.chunk",
                limits.filesystem.transfer.max_content_chunk_bytes,
            ),
            ("process.read", limits.process.max_read_bytes),
            ("job.read", limits.job.max_read_bytes),
        ] {
            assert!(base64(bytes) < MAX_MESSAGE_BYTES * 3 / 4, "{name}");
        }
        assert!(limits.max_concurrent_requests >= MIN_CONCURRENT_REQUESTS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn initialize_reports_the_instance_cwd_and_capability_families() {
        let (_temp, runtime) = runtime();
        let response = runtime.initialize_response();
        assert_eq!(response.protocol_version, ENVD_PROTOCOL_VERSION);
        assert_eq!(response.plane, Plane::Data);
        assert_eq!(&response.daemon_instance_id, runtime.instance_id());
        assert_eq!(
            response.default_cwd.as_str(),
            runtime.config().cwd.to_str().unwrap()
        );
        let filesystem = response.capabilities.filesystem.expect("filesystem");
        assert!(filesystem.write);
        assert!(response.capabilities.process.is_some());
        assert!(response.capabilities.job.is_some());
        assert_eq!(response.limits.job.max_request_jobs, 64);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_read_only_daemon_refuses_writes_before_any_side_effect() {
        let (_temp, runtime) = super::test_support::runtime_with(|config| {
            config.read_only_fs = true;
        });
        let path = runtime.config().cwd.join("written");
        let error = handle_request(
            &runtime,
            &RequestContext::detached(),
            "fs/writeFile",
            json!({"path": path.to_str().unwrap(), "data": "aGk="}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::CapabilityUnavailable);
        assert!(!path.exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn idle_report_holds_zero_while_a_process_runs_and_resets_on_use() {
        let (_temp, runtime) = runtime();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let quiet: GetIdleResponse =
            serde_json::from_value(call(&runtime, EnvGetIdle::NAME, json!({})).await).unwrap();
        assert!(quiet.idle_for_ms >= 30, "idle_for_ms={}", quiet.idle_for_ms);
        assert_eq!(quiet.running_process_count, 0);

        call(
            &runtime,
            ProcessStart::NAME,
            json!({
                "processId": "sleeper",
                "command": {"argv": ["/bin/sh", "-c", "sleep 30"], "timeoutMs": 60000},
            }),
        )
        .await;
        let busy = runtime.idle_report().await;
        assert_eq!(busy.running_process_count, 1);
        assert_eq!(busy.idle_for_ms, 0);

        call(
            &runtime,
            ProcessTerminate::NAME,
            json!({"processId": "sleeper", "signal": "kill"}),
        )
        .await;
        for _ in 0..100 {
            if runtime.idle_report().await.running_process_count == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let after = runtime.idle_report().await;
        assert_eq!(after.running_process_count, 0);
        assert!(after.idle_for_ms < 5_000);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_leftover_process_group_holds_the_idle_clock_like_running_work() {
        let (_temp, runtime) = runtime();
        call(
            &runtime,
            ProcessStart::NAME,
            json!({
                "processId": "service",
                "command": {
                    "argv": ["/bin/sh", "-c", "nohup sleep 30 >/dev/null 2>&1 & echo $! > leftover.pid; exit 0"],
                    "timeoutMs": 60000,
                },
            }),
        )
        .await;
        let mut held = runtime.idle_report().await;
        for _ in 0..250 {
            if held.running_process_count == 0 && held.leftover_process_group_count == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            held = runtime.idle_report().await;
        }
        assert_eq!(held.running_process_count, 0, "the root exited");
        assert_eq!(held.leftover_process_group_count, 1);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            runtime.idle_report().await.idle_for_ms,
            0,
            "a leftover service keeps the environment awake"
        );

        let pid: i32 = std::fs::read_to_string(runtime.config().cwd.join("leftover.pid"))
            .expect("pid file")
            .trim()
            .parse()
            .expect("pid");
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid).expect("pid"),
            rustix::process::Signal::KILL,
        );
        let mut released = runtime.idle_report().await;
        for _ in 0..250 {
            if released.leftover_process_group_count == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            released = runtime.idle_report().await;
        }
        assert_eq!(released.leftover_process_group_count, 0);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let idle = runtime.idle_report().await.idle_for_ms;
        assert!(
            (30..5_000).contains(&idle),
            "the clock restarts when the last leftover ends: idle_for_ms={idle}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observation_methods_leave_the_idle_clock_alone() {
        let (_temp, runtime) = runtime();
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        call(&runtime, EnvGetIdle::NAME, json!({})).await;
        let cwd = runtime.config().cwd.to_str().unwrap().to_owned();
        let _ = handle_request(
            &runtime,
            &RequestContext::detached(),
            "fs/scan",
            json!({"roots": [{"path": cwd, "maxDepth": 0}]}),
        )
        .await;
        assert!(runtime.idle_report().await.idle_for_ms >= 40);
    }
}

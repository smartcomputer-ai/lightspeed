use environment_client::{ConnectOptions, DataClient};
use environment_protocol::{
    data::{exec::*, job::*, process::*},
    handshake::ImplementationInfo,
    ids::{JobId, ProcessId},
    paths::{EnvironmentPath, PathConvention},
};
use std::{
    collections::BTreeMap,
    fs::File,
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{CREATE_NO_WINDOW, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

pub const DEADLINE: Duration = Duration::from_secs(30);

struct OwnedDaemon(Child);
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn wire(path: &Path) -> EnvironmentPath {
    let text = path.to_str().expect("UTF-8 fixture path");
    EnvironmentPath::from_native(
        text.strip_prefix(r"\\?\").unwrap_or(text),
        PathConvention::Windows,
    )
    .unwrap()
}

/// Each test owns an actual executable, ephemeral listener and private state.
/// Killing the Child on unwind also closes its Job Objects before TempDir drops.
pub struct Daemon {
    child: Option<OwnedDaemon>,
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub state: PathBuf,
    log: PathBuf,
    pub endpoint: String,
    pub client: DataClient,
}

impl Daemon {
    pub async fn start(read_only: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = PathBuf::from(wire(&temp.path().canonicalize().unwrap()).to_native());
        let root = base.join("workspace ü 空");
        std::fs::create_dir(&root).unwrap();
        let state = base.join("state");
        let log = base.join("envd.log");
        let (child, endpoint, client) = Self::launch(&root, &state, &log, read_only).await;
        Self {
            child: Some(child),
            _temp: temp,
            root,
            state,
            log,
            endpoint,
            client,
        }
    }

    async fn launch(
        root: &Path,
        state: &Path,
        log: &Path,
        read_only: bool,
    ) -> (OwnedDaemon, String, DataClient) {
        let executable = std::env::var_os("LIGHTSPEED_TEST_ENVD_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_lightspeed-envd").into());
        let mut command = Command::new(executable);
        command
            .args(["--listen", "127.0.0.1:0", "--cwd"])
            .arg(root)
            .arg("--fs-root")
            .arg(root)
            .arg("--state-dir")
            .arg(state)
            .current_dir(root)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(log).unwrap());
        for (name, _) in std::env::vars_os() {
            if name
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("LIGHTSPEED_ENVD_")
            {
                command.env_remove(name);
            }
        }
        if read_only {
            command.arg("--read-only-fs");
        }
        let mut child = OwnedDaemon(command.spawn().expect("launch the built envd executable"));
        let deadline = tokio::time::Instant::now() + DEADLINE;
        loop {
            let output = std::fs::read_to_string(log).unwrap_or_default();
            if let Some(address) = output.lines().find_map(|line| {
                line.split_once("listener ready on ")
                    .map(|(_, address)| address)
            }) {
                let endpoint = format!("ws://{address}");
                return (child, endpoint.clone(), connect(&endpoint).await);
            }
            if let Some(status) = child.0.try_wait().unwrap() {
                panic!("envd exited {status}: {output}");
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("envd listener startup timed out: {output}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn path(&self, relative: &str) -> EnvironmentPath {
        wire(&if relative.is_empty() {
            self.root.clone()
        } else {
            self.root.join(relative)
        })
    }
    pub fn outside(&self) -> PathBuf {
        self.root.parent().unwrap().join("outside")
    }
    pub fn crash(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.0.kill().unwrap();
            child.0.wait().unwrap();
        }
    }
    pub async fn restart(&mut self) {
        self.crash();
        let (child, endpoint, client) =
            Self::launch(&self.root, &self.state, &self.log, false).await;
        self.child = Some(child);
        self.endpoint = endpoint;
        self.client = client;
    }
    pub fn native(&self, mode: &str) -> CommandSpec {
        let exe = self.root.join("native fixture.exe");
        if !exe.exists() {
            let source = self.root.join("fixture.rs");
            std::fs::write(&source, include_str!("fixture.rs")).unwrap();
            let result = Command::new("rustc")
                .arg("--edition=2024")
                .arg(&source)
                .arg("-o")
                .arg(&exe)
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .expect("Windows live tests require rustc and MSVC on PATH");
            assert!(
                result.status.success(),
                "native fixture build failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        spec(vec![exe.to_str().unwrap().into(), mode.into()])
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        drop(self.child.take());
    }
}

pub async fn connect(endpoint: &str) -> DataClient {
    DataClient::connect_initialized(
        endpoint,
        ConnectOptions::new(ImplementationInfo::new("windows-live").unwrap())
            .with_request_timeout(DEADLINE),
    )
    .await
    .expect("real WebSocket handshake")
    .0
}
pub fn spec(argv: Vec<String>) -> CommandSpec {
    CommandSpec {
        argv,
        cwd: None,
        env: BTreeMap::new(),
        secret_env: BTreeMap::new(),
        input: ExecInput::None {},
        timeout_ms: Some(20_000),
    }
}
pub fn pid(id: &str) -> ProcessId {
    ProcessId::new(id).unwrap()
}
pub async fn start(client: &DataClient, id: &str, command: CommandSpec, pty: bool) -> u32 {
    client
        .start_process(&StartProcessParams {
            process_id: pid(id),
            command,
            pty: pty.then_some(PtySize {
                rows: 30,
                cols: 100,
            }),
        })
        .await
        .unwrap()
        .pid
}
pub fn read(id: &str, after: u64) -> ReadProcessParams {
    ReadProcessParams {
        process_id: pid(id),
        after: Some(after),
        tail_bytes: None,
        ack: None,
        max_bytes: 65536,
        wait_ms: 100,
        return_on: ReturnOn::AnyOutput,
    }
}
pub fn bytes(page: &OutputPage) -> Vec<u8> {
    page.chunks
        .iter()
        .flat_map(|chunk| chunk.data.as_slice())
        .copied()
        .collect()
}
pub async fn finished(client: &DataClient, id: &str) -> (Vec<u8>, ReadProcessResponse) {
    tokio::time::timeout(DEADLINE, async {
        let mut data = Vec::new();
        let mut after = 0;
        loop {
            let reply = client.read_process(&read(id, after)).await.unwrap();
            assert!(
                reply.output.gaps.is_empty(),
                "unexpected output gap: {:?}",
                reply.output
            );
            data.extend(bytes(&reply.output));
            after = reply.output.next;
            if reply.state == ExecState::Exited && after == reply.output.end {
                return (data, reply);
            }
        }
    })
    .await
    .expect("process failed to settle")
}
pub async fn contains(client: &DataClient, id: &str, needle: &str) -> String {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let reply = client.read_process(&read(id, 0)).await.unwrap();
            let text = String::from_utf8(bytes(&reply.output)).unwrap();
            if text.contains(needle) {
                return text;
            }
            assert_ne!(
                reply.state,
                ExecState::Exited,
                "exited before {needle}: {text}"
            );
        }
    })
    .await
    .expect("expected process output")
}
pub async fn job(client: &DataClient, id: &str, terminal: bool) -> (JobSummary, Vec<u8>) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let response = client
                .read_jobs(&ReadJobsParams {
                    jobs: vec![JobReadRequest {
                        job_id: JobId::new(id).unwrap(),
                        after: None,
                        tail_bytes: None,
                        max_bytes: None,
                    }],
                    max_bytes: 65536,
                    wait_ms: 100,
                    return_on: ReturnOn::Exit,
                })
                .await
                .unwrap();
            let JobReadEntry::Job { summary, output } = response.jobs.into_iter().next().unwrap()
            else {
                panic!("job disappeared");
            };
            if !terminal || summary.status.is_terminal() {
                return (*summary, bytes(&output));
            }
        }
    })
    .await
    .expect("job failed to settle")
}
pub struct ObservedProcess(OwnedHandle);
impl ObservedProcess {
    pub fn open(pid: u32) -> Self {
        // Retained handles prevent PID reuse from invalidating death assertions.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!handle.is_null(), "cannot retain process {pid}");
        Self(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
    pub fn alive(&self) -> bool {
        unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) == WAIT_TIMEOUT }
    }
    pub async fn dead(&self) {
        tokio::time::timeout(DEADLINE, async {
            while self.alive() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(
                unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) },
                WAIT_OBJECT_0
            );
        })
        .await
        .expect("owned process survived termination");
    }
}
pub fn child_pid(output: &str) -> u32 {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("CHILD:"))
        .expect("child pid")
        .parse()
        .unwrap()
}

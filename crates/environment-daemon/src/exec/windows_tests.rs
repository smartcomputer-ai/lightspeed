//! Native execution acceptance tests. The ignored helper is an explicitly
//! launched child process, not an optional external/credentialed test.

use super::*;
use environment_protocol::{
    data::exec::{ExitStatus, LeftoverDisposition, OutputStream, StoppedBy},
    error::ProtocolErrorKind,
};
use std::{
    io::{Read, Write},
    time::Duration,
};

const HELPER: &str = "exec::windows_tests::native_child";
pub(super) const MODE: &str = "LIGHTSPEED_NATIVE_TEST_MODE";

#[derive(Default)]
struct Capture(Vec<u8>);
impl OutputSink for Capture {
    fn append(&mut self, _: OutputStream, data: &[u8]) {
        self.0.extend_from_slice(data);
    }
}

pub(super) fn arguments() -> Vec<String> {
    vec![
        std::env::current_exe().unwrap().to_str().unwrap().into(),
        "--exact".into(),
        HELPER.into(),
        "--ignored".into(),
        "--nocapture".into(),
    ]
}

fn prepared(mode: &str, pty: bool, input: ExecInput) -> PreparedCommand {
    PreparedCommand::new(
        CommandSpec {
            argv: arguments(),
            cwd: None,
            env: BTreeMap::from([(MODE.into(), mode.into())]),
            secret_env: BTreeMap::new(),
            input,
            timeout_ms: None,
        },
        std::env::current_dir().unwrap(),
        &[],
        pty.then_some(PtySize { rows: 24, cols: 80 }),
    )
}

fn launch(
    mode: &str,
    pty: bool,
    input: ExecInput,
    after_exit: AfterExit,
    timeout: Option<Duration>,
) -> Execution<Capture> {
    let execution = prepared(mode, pty, input)
        .spawn(
            Capture::default(),
            ExecOptions {
                timeout,
                terminate_grace: Duration::ZERO,
                after_exit,
            },
            1024 * 1024,
        )
        .expect("native spawn");
    execution.start_input();
    execution
}

async fn finished(execution: &Execution<Capture>) {
    tokio::time::timeout(
        Duration::from_secs(10),
        execution.wait_until(|state| state.settled()),
    )
    .await
    .expect("settled exit deadline");
}
async fn output_contains(execution: &Execution<Capture>, needle: &str) {
    tokio::time::timeout(
        Duration::from_secs(10),
        execution.wait_until(|state| String::from_utf8_lossy(&state.sink.0).contains(needle)),
    )
    .await
    .expect("output deadline");
}
fn text(execution: &Execution<Capture>) -> String {
    execution.with_state(|state| String::from_utf8_lossy(&state.sink.0).into_owned())
}
fn child_pid(execution: &Execution<Capture>) -> u32 {
    let output = text(execution);
    let start = output.find("CHILD:").expect("child pid marker") + 6;
    output[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap()
}

#[test]
#[ignore = "child-process helper; launched explicitly by native execution tests"]
fn native_child() {
    let mode = std::env::var(MODE).expect("helper mode is required");
    match mode.as_str() {
        "exit" => {
            println!("snow: 雪");
            std::process::exit(37);
        }
        "sleep" => std::thread::sleep(Duration::from_secs(60)),
        "tree" => {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(&arguments()[1..])
                .env(MODE, "sleep")
                .spawn()
                .expect("grandchild");
            println!("CHILD:{}", child.id());
            std::io::stdout().flush().unwrap();
            std::process::exit(0);
        }
        "tree_running" => {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(&arguments()[1..])
                .env(MODE, "sleep")
                .spawn()
                .expect("grandchild");
            println!("CHILD:{}", child.id());
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(60));
            child
                .wait()
                .expect("grandchild exits if the ownership test times out");
        }
        "read" => {
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes).unwrap();
            println!("BYTES:{}", serde_json::to_string(&bytes).unwrap());
        }
        "interactive" => {
            println!("READY");
            std::io::stdout().flush().unwrap();
            loop {
                let mut line = String::new();
                assert_ne!(
                    std::io::stdin().read_line(&mut line).unwrap(),
                    0,
                    "interactive console input unexpectedly closed"
                );
                println!("LINE:{}", line.trim());
                std::io::stdout().flush().unwrap();
            }
        }
        "owner" => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let execution = launch(
                    "tree_running",
                    false,
                    ExecInput::None {},
                    AfterExit::Keep,
                    None,
                );
                output_contains(&execution, "CHILD:").await;
                std::fs::write(
                    std::env::var("LIGHTSPEED_NATIVE_PID_FILE").unwrap(),
                    format!("{} {}", execution.pid(), child_pid(&execution)),
                )
                .unwrap();
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
        }
        other => panic!("unknown helper mode {other}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_pipes_preserve_output_status_and_exact_input_bytes() {
    let execution = launch("exit", false, ExecInput::None {}, AfterExit::Keep, None);
    finished(&execution).await;
    assert_eq!(
        execution.with_state(|state| state.exit().cloned()),
        Some(ExitStatus::Code { code: 37 })
    );
    assert!(text(&execution).contains("snow: 雪"));
    let bytes = vec![0, 1, 10, 13, 255];
    let execution = launch(
        "read",
        false,
        ExecInput::Bytes {
            data: bytes.clone().into(),
        },
        AfterExit::Keep,
        None,
    );
    finished(&execution).await;
    assert!(
        text(&execution).contains(&format!("BYTES:{}", serde_json::to_string(&bytes).unwrap()))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn natural_root_exit_keeps_pipe_and_terminal_descendants_until_explicit_kill() {
    for pty in [false, true] {
        let execution = launch("tree", pty, ExecInput::None {}, AfterExit::Keep, None);
        finished(&execution).await;
        assert!(
            execution.group_alive(),
            "descendants must survive root exit (pty={pty})"
        );
        let child = child_pid(&execution);
        let report = execution
            .sample_leftovers(LeftoverDisposition::Kept)
            .expect("leftovers");
        assert!(report.processes.iter().any(|process| process.pid == child));
        let retained_group = execution.group();
        drop(execution);
        assert!(
            retained_group.alive(),
            "evicting completed output must retain the descendants and their console"
        );
        retained_group.signal(crate::process_group::GroupSignal::Kill);
        tokio::time::timeout(Duration::from_secs(10), async {
            while retained_group.alive() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn timeout_and_job_cleanup_sweep_grandchildren() {
    let execution = launch(
        "tree_running",
        false,
        ExecInput::None {},
        AfterExit::Keep,
        Some(Duration::from_millis(500)),
    );
    finished(&execution).await;
    assert_eq!(
        execution.with_state(|state| state.stopped_by()),
        Some(StoppedBy::Timeout)
    );
    assert!(!execution.group_alive());
    let job = launch("tree", false, ExecInput::None {}, AfterExit::Manual, None);
    tokio::time::timeout(
        Duration::from_secs(10),
        job.wait_until(|state| state.exited()),
    )
    .await
    .unwrap();
    assert!(job.sample_leftovers(LeftoverDisposition::Swept).is_some());
    job.group().terminate(Duration::ZERO).await;
    job.settle(true).await;
    assert!(!job.group_alive());
}

#[tokio::test(flavor = "current_thread")]
async fn conpty_input_resize_interrupt_and_eof_contract() {
    let execution = launch(
        "interactive",
        true,
        ExecInput::None {},
        AfterExit::Keep,
        None,
    );
    output_contains(&execution, "READY").await;
    execution.offer_input("雪\n".as_bytes(), false).unwrap();
    output_contains(&execution, "LINE:雪").await;
    execution
        .resize_for_test(portable_pty::PtySize {
            rows: 40,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    assert_eq!(
        execution.offer_input(b"", true).unwrap_err().kind(),
        ProtocolErrorKind::CapabilityUnavailable
    );
    assert!(execution.interrupt().unwrap());
    finished(&execution).await;
    assert!(
        execution.with_state(|state| state.stopped_by()).is_none(),
        "interrupt is not forced termination"
    );
    let pipe = launch("sleep", false, ExecInput::None {}, AfterExit::Keep, None);
    assert_eq!(
        pipe.interrupt().unwrap_err().kind(),
        ProtocolErrorKind::CapabilityUnavailable
    );
    pipe.stop(StoppedBy::Kill);
    finished(&pipe).await;
}

#[tokio::test(flavor = "current_thread")]
async fn daemon_death_closes_noninherited_job_handles_and_kills_tree() {
    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("pids");
    let mut owner = std::process::Command::new(std::env::current_exe().unwrap())
        .args(&arguments()[1..])
        .env(MODE, "owner")
        .env("LIGHTSPEED_NATIVE_PID_FILE", &pid_file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pids = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                break text
                    .split_whitespace()
                    .map(|pid| pid.parse::<u32>().unwrap())
                    .collect::<Vec<_>>();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("owner created its tree");
    assert_eq!(pids.len(), 2);
    owner.kill().unwrap();
    owner.wait().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while pids
            .iter()
            .any(|pid| crate::process_group::process_start_time(*pid).is_some())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("owned descendants killed at daemon death");
}

#[tokio::test(flavor = "current_thread")]
async fn native_argv_round_trips_empty_unicode_quotes_and_trailing_backslashes() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("argument recorder.ps1");
    std::fs::write(&script, "[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false); [Console]::Write((ConvertTo-Json -Compress -InputObject @([Environment]::GetCommandLineArgs())))").unwrap();
    let expected = vec![
        "",
        "space here",
        "雪",
        "embedded\"quote",
        "one\\",
        "two\\\\",
        "before\\\"after",
    ];
    let mut command = prepared("exit", false, ExecInput::None {});
    command.argv = vec![
        "powershell.exe".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-File".into(),
        script.to_str().unwrap().into(),
    ];
    command
        .argv
        .extend(expected.iter().map(|s| (*s).to_owned()));
    let execution = command
        .spawn(
            Capture::default(),
            ExecOptions {
                timeout: Some(Duration::from_secs(10)),
                terminate_grace: Duration::ZERO,
                after_exit: AfterExit::Keep,
            },
            1024,
        )
        .unwrap();
    execution.start_input();
    finished(&execution).await;
    let output = text(&execution);
    let args: Vec<String> =
        serde_json::from_str(&output).expect("PowerShell records CLR argv as JSON");
    assert_eq!(&args[args.len() - expected.len()..], expected.as_slice());
}

#[test]
fn environment_override_names_collide_using_windows_case_rules() {
    let mut command = CommandSpec {
        argv: arguments(),
        cwd: None,
        env: BTreeMap::from([("Path".into(), "one".into()), ("PATH".into(), "two".into())]),
        secret_env: BTreeMap::new(),
        input: ExecInput::None {},
        timeout_ms: None,
    };
    assert_eq!(
        validate_command(&command, "command").unwrap_err().kind(),
        ProtocolErrorKind::InvalidParams
    );
    command.env = BTreeMap::from([("Å".into(), "one".into())]);
    command.secret_env.insert(
        "å".into(),
        environment_protocol::bytes::SecretString::new("secret"),
    );
    assert_eq!(
        validate_command(&command, "command").unwrap_err().kind(),
        ProtocolErrorKind::InvalidParams
    );
}

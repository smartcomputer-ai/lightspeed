//! Opt-in tests of the real Windows executable, real WebSockets and OS objects.
//! Run with the MSVC/Rust environment loaded and PowerShell 7 plus Git for
//! Windows installed: cargo test -p environment-daemon --test windows_live -- --ignored
#![cfg(windows)]
#[path = "windows_live/support.rs"]
mod support;
#[path = "windows_live/transfers.rs"]
mod transfers;

use environment_client::shell::{ShellOptions, command_argv};
use environment_protocol::{
    data::{exec::*, fs::*, idle::GetIdleParams, job::*, platform::ShellKind, process::*},
    error::ProtocolErrorKind,
    ids::{JobId, JobRequestId},
};
use support::*;

#[tokio::test(flavor = "current_thread")]
#[ignore = "launches native envd; release builds supply the extracted executable"]
async fn packaged_release_smoke() {
    let executable = std::env::var_os("LIGHTSPEED_TEST_ENVD_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_lightspeed-envd").into());
    assert!(std::path::Path::new(&executable).is_absolute());
    let daemon = Daemon::start(false).await;
    let content = "release smoke ü 空".as_bytes();
    daemon
        .client
        .write_file(&WriteFileParams {
            path: daemon.path("smoke ü.txt"),
            data: content.into(),
            create: WriteCondition::Always,
            if_match: None,
            executable: false,
        })
        .await
        .unwrap();
    let read = daemon
        .client
        .read_file(&ReadFileParams {
            path: daemon.path("smoke ü.txt"),
            offset: 0,
            max_bytes: 1024,
        })
        .await
        .unwrap();
    assert_eq!(read.data.as_slice(), content);
    let shell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    start(
        &daemon.client,
        "release-smoke",
        spec(vec![
            shell.to_str().unwrap().into(),
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            "Write-Output release-smoke; exit 7".into(),
        ]),
        false,
    )
    .await;
    let (output, result) = finished(&daemon.client, "release-smoke").await;
    assert!(String::from_utf8(output).unwrap().contains("release-smoke"));
    assert_eq!(result.exit, Some(ExitStatus::Code { code: 7 }));
    assert!(result.error.is_none());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "launches native envd and requires Windows symlink privilege"]
async fn real_binary_passes_data_conformance_over_websockets() {
    let daemon = Daemon::start(false).await;
    std::fs::create_dir(daemon.outside()).unwrap();
    let mut suite = tools::environment_protocol::conformance::DataConformance::new(
        daemon.client.clone(),
        daemon.path(""),
    );
    suite.outside_root = Some(wire(&daemon.outside()));
    tools::environment_protocol::conformance::run(&suite)
        .await
        .into_result()
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires installed PowerShell 7, Windows PowerShell 5.1 and Git for Windows"]
async fn installed_shells_preserve_unicode_cwd_environment_and_failure_codes() {
    let daemon = Daemon::start(false).await;
    let platform = &daemon.client.initialize_response().platform;
    assert_eq!(platform.default_shell, Some(ShellKind::PowerShell));
    for (index, kind) in [
        ShellKind::PowerShell,
        ShellKind::WindowsPowerShell,
        ShellKind::GitBash,
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            platform.shells.iter().any(|shell| shell.kind == kind),
            "required installed shell missing: {kind:?}"
        );
        let script = if kind == ShellKind::GitBash {
            "printf '%s\\n' '空 ü' \"$LIVE_VALUE\"; pwd; exit 23"
        } else {
            "[Console]::WriteLine('空 ü'); [Console]::WriteLine($env:LIVE_VALUE); [Console]::WriteLine((Get-Location).Path); exit 23"
        };
        let mut command = spec(
            command_argv(
                platform,
                script,
                ShellOptions {
                    shell: Some(kind),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        command.cwd = Some(daemon.path(""));
        command
            .env
            .insert("LIVE_VALUE".into(), "value with spaces 空".into());
        let id = format!("shell-{index}");
        start(&daemon.client, &id, command, false).await;
        let (output, result) = finished(&daemon.client, &id).await;
        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("空 ü")
                && output.contains("value with spaces 空")
                && output.contains("workspace ü 空"),
            "{kind:?}: {output}"
        );
        assert_eq!(result.exit, Some(ExitStatus::Code { code: 23 }));
        assert!(result.error.is_none());
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "launches native envd and compiles a native argv/pipe fixture"]
async fn direct_argv_and_binary_pipe_survive_real_client_reconnection() {
    let daemon = Daemon::start(false).await;
    let mut command = daemon.native("argv");
    let arguments = [
        "",
        "two words",
        "空 ü",
        "a\"b",
        "trailing\\",
        "$(not-a-shell)",
    ];
    command.argv.extend(arguments.iter().map(|s| s.to_string()));
    start(&daemon.client, "argv", command, false).await;
    let (output, result) = finished(&daemon.client, "argv").await;
    let expected = arguments
        .iter()
        .map(|arg| format!("{}\n", hex::encode(arg.as_bytes())))
        .collect::<String>();
    assert_eq!(String::from_utf8(output).unwrap(), expected);
    assert_eq!(result.exit, Some(ExitStatus::Code { code: 0 }));

    let mut command = daemon.native("echo");
    command.input = ExecInput::Pipe {};
    start(&daemon.client, "binary", command, false).await;
    let content: Vec<u8> = (0..192 * 1024).map(|i| (i % 251) as u8).collect();
    let mut offset = 0;
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while offset < content.len() / 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "pipe input made no progress"
        );
        let end = (offset + 8192).min(content.len() / 2);
        let reply = daemon
            .client
            .write_process(&WriteProcessParams {
                process_id: pid("binary"),
                data: content[offset..end].into(),
                close_input: false,
            })
            .await
            .unwrap();
        offset += reply.accepted_bytes as usize;
    }
    daemon.client.close().await.unwrap();
    let client = connect(&daemon.endpoint).await;
    while offset < content.len() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "pipe input made no progress after reconnect"
        );
        let end = (offset + 8192).min(content.len());
        let reply = client
            .write_process(&WriteProcessParams {
                process_id: pid("binary"),
                data: content[offset..end].into(),
                close_input: false,
            })
            .await
            .unwrap();
        offset += reply.accepted_bytes as usize;
    }
    client
        .write_process(&WriteProcessParams {
            process_id: pid("binary"),
            data: Default::default(),
            close_input: true,
        })
        .await
        .unwrap();
    assert_eq!(finished(&client, "binary").await.0, content);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a working native ConPTY and Rust/MSVC"]
async fn conpty_input_interrupt_and_eof_cross_the_wire() {
    let daemon = Daemon::start(false).await;
    start(
        &daemon.client,
        "terminal",
        daemon.native("interactive"),
        true,
    )
    .await;
    contains(&daemon.client, "terminal", "READY").await;
    daemon
        .client
        .write_process(&WriteProcessParams {
            process_id: pid("terminal"),
            data: "空 ü\r\n".as_bytes().into(),
            close_input: false,
        })
        .await
        .unwrap();
    contains(&daemon.client, "terminal", "LINE:空 ü").await;
    let error = daemon
        .client
        .write_process(&WriteProcessParams {
            process_id: pid("terminal"),
            data: Default::default(),
            close_input: true,
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.protocol_kind(),
        Some(ProtocolErrorKind::CapabilityUnavailable)
    );
    let client = connect(&daemon.endpoint).await;
    client
        .terminate_process(&TerminateProcessParams {
            process_id: pid("terminal"),
            signal: ProcessSignal::Interrupt,
        })
        .await
        .unwrap();
    let (_, result) = finished(&client, "terminal").await;
    assert!(
        result.stopped_by.is_none(),
        "interrupt must not become a forced kill"
    );
}

fn jobs(request: &str, id: &str, command: CommandSpec) -> StartJobsParams {
    StartJobsParams {
        request_id: JobRequestId::new(request).unwrap(),
        jobs: vec![JobSpec {
            job_id: JobId::new(id).unwrap(),
            name: None,
            command,
            depends_on: vec![],
            dependency_policy: Default::default(),
            queue_key: None,
        }],
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "kills and restarts its own envd executable to verify OS ownership and durable jobs"]
async fn abrupt_daemon_death_kills_leftovers_and_recovers_jobs() {
    let mut daemon = Daemon::start(false).await;
    start(
        &daemon.client,
        "background",
        daemon.native("tree-exit"),
        false,
    )
    .await;
    let (output, response) = finished(&daemon.client, "background").await;
    let descendant = ObservedProcess::open(child_pid(&String::from_utf8(output).unwrap()));
    assert!(descendant.alive());
    assert!(response.leftovers.is_some());
    assert_eq!(
        daemon
            .client
            .get_idle(&GetIdleParams {})
            .await
            .unwrap()
            .leftover_process_group_count,
        1
    );
    let completed = jobs("complete-request", "complete", daemon.native("exit"));
    daemon.client.start_jobs(&completed).await.unwrap();
    let receipt = job(&daemon.client, "complete", true).await;
    assert_eq!(receipt.0.status, JobStatus::Succeeded);
    daemon
        .client
        .start_jobs(&jobs("running-request", "running", daemon.native("sleep")))
        .await
        .unwrap();
    let running = tokio::time::timeout(DEADLINE, async {
        loop {
            let output = String::from_utf8(job(&daemon.client, "running", false).await.1).unwrap();
            if output.contains("READY") {
                break ObservedProcess::open(
                    output
                        .lines()
                        .find_map(|line| line.strip_prefix("PID:"))
                        .unwrap()
                        .parse()
                        .unwrap(),
                );
            }
        }
    })
    .await
    .unwrap();
    daemon.crash();
    descendant.dead().await;
    running.dead().await;
    daemon.restart().await;
    assert_eq!(job(&daemon.client, "complete", true).await, receipt);
    assert_eq!(
        job(&daemon.client, "running", true).await.0.status,
        JobStatus::Interrupted
    );
    daemon.client.start_jobs(&completed).await.unwrap();
    assert_eq!(job(&daemon.client, "complete", true).await, receipt);
    assert_eq!(
        daemon
            .client
            .read_process(&read("background", 0))
            .await
            .unwrap_err()
            .protocol_kind(),
        Some(ProtocolErrorKind::NotFound)
    );
    assert_eq!(
        daemon
            .client
            .get_idle(&GetIdleParams {})
            .await
            .unwrap()
            .leftover_process_group_count,
        0
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "exercises read-only capabilities on the actual Windows executable"]
async fn read_only_filesystem_allows_capture_but_rejects_mutation() {
    let daemon = Daemon::start(true).await;
    std::fs::write(daemon.root.join("existing"), b"original").unwrap();
    assert!(
        !daemon
            .client
            .initialize_response()
            .capabilities
            .filesystem
            .as_ref()
            .unwrap()
            .write
    );
    let reply = daemon
        .client
        .read_file(&ReadFileParams {
            path: daemon.path("existing"),
            offset: 0,
            max_bytes: 64,
        })
        .await
        .unwrap();
    assert_eq!(reply.data.as_slice(), b"original");
    let error = daemon
        .client
        .write_file(&WriteFileParams {
            path: daemon.path("existing"),
            data: b"changed".as_slice().into(),
            create: WriteCondition::Always,
            if_match: None,
            executable: false,
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.protocol_kind(),
        Some(ProtocolErrorKind::CapabilityUnavailable)
    );
    transfers::read_only(&daemon).await;
    assert_eq!(
        std::fs::read(daemon.root.join("existing")).unwrap(),
        b"original"
    );
}

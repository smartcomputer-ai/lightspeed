use std::process::ExitCode;

use clap::Parser as _;
use environment_daemon::{
    DaemonRuntime,
    config::{DaemonArgs, DaemonCommand},
    server::{self, DaemonExit},
    upgrade,
};

/// Worker threads of the async runtime. Blocking work runs on the blocking
/// pool; the workers keep connections, heartbeats and idle reports moving
/// even if one of them is held up.
const WORKER_THREADS: usize = 2;

fn main() -> ExitCode {
    // Configuration is read, and secret-bearing variables are removed from
    // the process environment, before the async runtime starts any thread.
    let _ = dotenvy::dotenv();
    let args = DaemonArgs::parse();
    if args.print_build {
        return match serde_json::to_string(&environment_daemon::build_facts()) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("lightspeed-envd: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if args.command == Some(DaemonCommand::Upgrade) {
        return upgrade_command(&args);
    }
    let exit = match args.into_config() {
        Ok(config) => serve(config),
        Err(error) => DaemonExit::Configuration(error),
    };
    if !matches!(exit, DaemonExit::Shutdown) {
        eprintln!("lightspeed-envd {exit}");
    }
    ExitCode::from(exit.status())
}

fn serve(config: environment_daemon::config::DaemonConfig) -> DaemonExit {
    let runtime = match async_runtime() {
        Ok(runtime) => runtime,
        Err(error) => return DaemonExit::Failed(error.into()),
    };
    runtime.block_on(async move {
        let daemon = match DaemonRuntime::new(config) {
            Ok(daemon) => daemon,
            Err(error) => return DaemonExit::Configuration(error),
        };
        if let Err(error) = server::spawn_signal_listener(&daemon) {
            return DaemonExit::Failed(error);
        }
        server::run(daemon).await
    })
}

fn upgrade_command(args: &DaemonArgs) -> ExitCode {
    let request = match args.upgrade_request() {
        Ok(request) => request,
        Err(error) => {
            eprintln!("lightspeed-envd upgrade: {error:#}");
            return ExitCode::from(2);
        }
    };
    let installed = match async_runtime() {
        Ok(runtime) => runtime.block_on(upgrade::install(request)),
        Err(error) => {
            eprintln!("lightspeed-envd upgrade: {error}");
            return ExitCode::FAILURE;
        }
    };
    match installed {
        Ok(installed) => {
            println!(
                "installed lightspeed-envd {} ({}, protocol {}) at {}",
                installed.version,
                installed.git_sha,
                installed.protocol_version,
                installed.path.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("lightspeed-envd upgrade: {error}");
            ExitCode::FAILURE
        }
    }
}

fn async_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_name("lightspeed-envd")
        .enable_all()
        .build()
}

use anyhow::Context;
use clap::Parser;
use host_bridge::{BridgeRuntime, config::BridgeArgs, server};
use tokio::net::TcpListener;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let config = BridgeArgs::parse().into_config()?;
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("bind host-bridge listener at {}", config.listen))?;
    let local_addr = listener.local_addr().context("read listener address")?;
    let runtime = BridgeRuntime::new(config.clone(), local_addr)?;
    eprintln!(
        "host-bridge listening={} advertise={} target_id={} cwd={} fs_root={} state_dir={} read_only_fs={}",
        local_addr,
        runtime.controller_endpoint(),
        config.target_id,
        config.cwd.display(),
        config.fs_root.display(),
        config.state_dir.display(),
        config.read_only_fs,
    );

    let server_runtime = runtime.clone();
    let server_task = tokio::spawn(async move {
        if let Err(error) = server::run_server(listener, server_runtime).await {
            eprintln!("host-bridge server stopped: {error}");
        }
    });

    tokio::signal::ctrl_c()
        .await
        .context("wait for ctrl-c signal")?;
    eprintln!("host-bridge shutting down");

    server_task.abort();
    Ok(())
}

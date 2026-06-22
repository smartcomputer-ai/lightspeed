use anyhow::Context;
use clap::Parser;
use host_bridge::{BridgeRuntime, config::BridgeArgs, gateway::GatewayClient, server};
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
    let gateway = GatewayClient::new(config.gateway_url.clone(), config.provider_token.clone());

    eprintln!(
        "host-bridge listening={} advertise={} provider_id={} target_id={} cwd={} fs_root={} read_only_fs={}",
        local_addr,
        runtime.controller_endpoint(),
        config.provider_id,
        config.target_id,
        config.cwd.display(),
        config.fs_root.display(),
        config.read_only_fs,
    );

    let server_runtime = runtime.clone();
    let server_task = tokio::spawn(async move {
        if let Err(error) = server::run_server(listener, server_runtime).await {
            eprintln!("host-bridge server stopped: {error}");
        }
    });

    let registered = gateway
        .register(&config, &runtime)
        .await
        .map_err(|error| anyhow::anyhow!("register environment provider: {error}"))?;
    eprintln!(
        "host-bridge registered provider_id={} status={:?}",
        registered.result.provider.provider_id, registered.result.provider.status
    );

    let heartbeat_config = config.clone();
    let heartbeat_runtime = runtime.clone();
    let heartbeat_gateway =
        GatewayClient::new(config.gateway_url.clone(), config.provider_token.clone());
    let heartbeat_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(heartbeat_config.heartbeat_interval);
        loop {
            interval.tick().await;
            let target = match heartbeat_runtime.target_summary() {
                Ok(target) => target,
                Err(error) => {
                    eprintln!("host-bridge failed to build target heartbeat: {error}");
                    continue;
                }
            };
            if let Err(error) = heartbeat_gateway.heartbeat(&heartbeat_config, target).await {
                eprintln!("host-bridge heartbeat failed: {error}");
            }
        }
    });

    tokio::signal::ctrl_c()
        .await
        .context("wait for ctrl-c signal")?;
    eprintln!("host-bridge shutting down");

    heartbeat_task.abort();
    let _ = gateway.unregister(&config).await.map_err(|error| {
        eprintln!("host-bridge unregister failed: {error}");
    });
    server_task.abort();
    Ok(())
}

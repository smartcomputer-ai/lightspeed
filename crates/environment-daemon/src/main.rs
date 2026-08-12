use clap::Parser as _;
use environment_daemon::{DaemonRuntime, config::DaemonArgs, server};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let config = DaemonArgs::parse().into_config()?;
    let runtime = DaemonRuntime::new(config)?;
    server::run(runtime).await
}

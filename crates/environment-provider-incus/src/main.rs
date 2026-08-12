use clap::Parser as _;
use environment_provider_incus::{ProviderArgs, server};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let config = ProviderArgs::parse().load().await?;
    let backend = environment_provider_incus::IncusClient::new(config.clone()).await?;
    server::serve(config, backend).await
}

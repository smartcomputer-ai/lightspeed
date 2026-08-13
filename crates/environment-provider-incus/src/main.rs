use clap::Parser as _;
use environment_provider_incus::{ProviderArgs, server};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let config = ProviderArgs::parse().load().await?;
    let backend = environment_provider_incus::IncusClient::new(config.clone()).await?;
    if let Some(ingress) = config.ingress.as_ref() {
        let listener = tokio::net::TcpListener::bind(ingress.listen).await?;
        let edge = tokio::spawn(environment_provider_incus::edge::serve(
            listener,
            config.clone(),
            backend.clone(),
        ));
        tokio::select! {
            result = server::serve(config, backend) => result,
            result = edge => { result??; Ok(()) },
        }
    } else {
        server::serve(config, backend).await
    }
}

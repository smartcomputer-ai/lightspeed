use std::str::FromStr;

use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions};
use temporalio_sdk_core::Url;

pub async fn connect_temporal(target_url: &str, namespace: &str) -> anyhow::Result<Client> {
    let connection_options = ConnectionOptions::new(temporal_target_url(target_url)?).build();
    let connection = Connection::connect(connection_options).await?;
    Ok(Client::new(
        connection,
        ClientOptions::new(namespace.to_owned()).build(),
    )?)
}

fn temporal_target_url(target: &str) -> anyhow::Result<Url> {
    let target = target.trim();
    let normalized = if target.contains("://") {
        target.to_owned()
    } else {
        format!("http://{target}")
    };
    Ok(Url::from_str(&normalized)?)
}

#[cfg(test)]
mod tests {
    use super::temporal_target_url;

    #[test]
    fn accepts_the_shared_host_port_temporal_address() {
        let target = temporal_target_url("localhost:7233").expect("host:port target");
        assert_eq!(target.as_str(), "http://localhost:7233/");
    }

    #[test]
    fn preserves_an_explicit_temporal_url_scheme() {
        let target =
            temporal_target_url("https://temporal.example.test:7233").expect("absolute target URL");
        assert_eq!(target.as_str(), "https://temporal.example.test:7233/");
    }
}

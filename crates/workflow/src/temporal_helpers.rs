use std::str::FromStr;

use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions};
use temporalio_sdk_core::Url;

pub async fn connect_temporal(target_url: &str, namespace: &str) -> anyhow::Result<Client> {
    let connection_options = ConnectionOptions::new(Url::from_str(target_url)?).build();
    let connection = Connection::connect(connection_options).await?;
    Ok(Client::new(
        connection,
        ClientOptions::new(namespace.to_owned()).build(),
    )?)
}

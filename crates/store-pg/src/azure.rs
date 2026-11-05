use std::{fmt, sync::Arc};

use object_store::{
    ClientOptions, ObjectStore, RetryConfig,
    azure::{AzureConfigKey, MicrosoftAzureBuilder},
};

mod postgres;
#[cfg(test)]
mod test_support;
mod workload_identity;

pub use postgres::{AzurePostgresError, connect_postgres_with_workload_identity};
pub use workload_identity::{
    AzureWorkloadIdentity, AzureWorkloadIdentityError, TokenAcquisitionError,
};

/// Azure credentials supplied by the deployment. Debug output is redacted.
#[derive(Clone)]
pub enum AzureObjectStoreCredential {
    AccessKey(String),
    SasToken(String),
    WorkloadIdentity(AzureWorkloadIdentity),
}

impl fmt::Debug for AzureObjectStoreCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessKey(_) => f.write_str("AccessKey([REDACTED])"),
            Self::SasToken(_) => f.write_str("SasToken([REDACTED])"),
            Self::WorkloadIdentity(identity) => {
                f.debug_tuple("WorkloadIdentity").field(identity).finish()
            }
        }
    }
}

#[derive(Clone)]
pub struct AzureObjectStoreConfig {
    pub account: String,
    pub container: String,
    pub credential: AzureObjectStoreCredential,
    /// Account endpoint, including the account path when using an emulator.
    pub endpoint: Option<String>,
}

impl fmt::Debug for AzureObjectStoreConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AzureObjectStoreConfig")
            .field("account", &self.account)
            .field("container", &self.container)
            .field("credential", &self.credential)
            .field("endpoint_configured", &self.endpoint.is_some())
            .finish()
    }
}

/// Build the native Azure backend without changing CAS keys or metadata.
/// Credentials are explicit so a missing Secret cannot fall back to another identity.
pub fn build_azure_object_store(
    config: AzureObjectStoreConfig,
) -> Result<Arc<dyn ObjectStore>, object_store::Error> {
    build(config, ClientOptions::new(), RetryConfig::default())
}

fn build(
    config: AzureObjectStoreConfig,
    client_options: ClientOptions,
    retry: RetryConfig,
) -> Result<Arc<dyn ObjectStore>, object_store::Error> {
    let mut builder = MicrosoftAzureBuilder::new()
        .with_account(config.account)
        .with_container_name(config.container)
        .with_client_options(client_options)
        .with_retry(retry);
    // Bearer tokens never travel in plaintext, even to a loopback endpoint.
    let bearer = matches!(
        config.credential,
        AzureObjectStoreCredential::WorkloadIdentity(_)
    );
    if let Some(endpoint) = config.endpoint {
        builder = builder
            .with_allow_http(!bearer && endpoint.starts_with("http://"))
            .with_endpoint(endpoint);
    }
    builder = match config.credential {
        AzureObjectStoreCredential::AccessKey(key) => builder.with_access_key(key),
        AzureObjectStoreCredential::SasToken(token) => {
            builder.with_config(AzureConfigKey::SasKey, token.trim_start_matches('?'))
        }
        AzureObjectStoreCredential::WorkloadIdentity(identity) => {
            workload_identity::configure(builder, identity)?
        }
    };
    Ok(Arc::new(builder.build()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn azure_config_debug_redacts_both_credential_types_and_endpoint() {
        for credential in [
            AzureObjectStoreCredential::AccessKey("secret-key".into()),
            AzureObjectStoreCredential::SasToken("sig=secret-sas".into()),
        ] {
            let config = AzureObjectStoreConfig {
                account: "account".into(),
                container: "container".into(),
                credential,
                endpoint: Some("https://secret-endpoint.example".into()),
            };
            let debug = format!("{config:?}");
            assert!(!debug.contains("secret-"));
            assert!(debug.contains("[REDACTED]"));
        }
    }
}

use std::{fmt, sync::Arc};

use object_store::{
    ObjectStore,
    azure::{AzureConfigKey, MicrosoftAzureBuilder},
};

/// Static Azure credentials supplied by the deployment. Debug output is redacted.
#[derive(Clone)]
pub enum AzureObjectStoreCredential {
    AccessKey(String),
    SasToken(String),
}

impl fmt::Debug for AzureObjectStoreCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AccessKey(_) => "AccessKey([REDACTED])",
            Self::SasToken(_) => "SasToken([REDACTED])",
        })
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
    let mut builder = MicrosoftAzureBuilder::new()
        .with_account(config.account)
        .with_container_name(config.container);
    builder = match config.credential {
        AzureObjectStoreCredential::AccessKey(key) => builder.with_access_key(key),
        AzureObjectStoreCredential::SasToken(token) => {
            builder.with_config(AzureConfigKey::SasKey, token.trim_start_matches('?'))
        }
    };
    if let Some(endpoint) = config.endpoint {
        builder = builder
            .with_allow_http(endpoint.starts_with("http://"))
            .with_endpoint(endpoint);
    }
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

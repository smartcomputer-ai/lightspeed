use std::{env, sync::Arc};

use object_store::ObjectStore;
use store_pg::{
    AzureObjectStoreConfig, AzureObjectStoreCredential, S3ObjectStoreConfig,
    build_azure_object_store, build_s3_object_store,
};

const BACKEND: &str = "LIGHTSPEED_OBJECT_STORE_BACKEND";
const BUCKET: &str = "LIGHTSPEED_OBJECT_STORE_BUCKET";
const ENDPOINT: &str = "LIGHTSPEED_OBJECT_STORE_ENDPOINT";
const REGION: &str = "LIGHTSPEED_OBJECT_STORE_REGION";
const PREFIX: &str = "LIGHTSPEED_OBJECT_STORE_PREFIX";
const PATH_STYLE: &str = "LIGHTSPEED_OBJECT_STORE_FORCE_PATH_STYLE";
const ACCOUNT: &str = "LIGHTSPEED_OBJECT_STORE_AZURE_ACCOUNT";
const CONTAINER: &str = "LIGHTSPEED_OBJECT_STORE_AZURE_CONTAINER";
const ACCESS_KEY: &str = "LIGHTSPEED_OBJECT_STORE_AZURE_ACCESS_KEY";
const SAS_TOKEN: &str = "LIGHTSPEED_OBJECT_STORE_AZURE_SAS_TOKEN";
const AZURE_KEYS: [&str; 4] = [ACCOUNT, CONTAINER, ACCESS_KEY, SAS_TOKEN];
const S3_KEYS: [&str; 3] = [BUCKET, REGION, PATH_STYLE];

#[derive(Debug)]
pub(super) enum ObjectStorageConfig {
    S3(S3ObjectStoreConfig),
    Azure(AzureObjectStoreConfig),
}

impl ObjectStorageConfig {
    pub(super) fn build(self) -> anyhow::Result<Arc<dyn ObjectStore>> {
        match self {
            ObjectStorageConfig::S3(config) => Ok(build_s3_object_store(config)?),
            ObjectStorageConfig::Azure(config) => build_azure_object_store(config).map_err(|_| {
                // Builder errors can include malformed SAS values. Report setting names only.
                anyhow::anyhow!(
                    "invalid Azure object-store configuration; check {ACCOUNT}, {CONTAINER}, \
                     {ENDPOINT}, and the credential in {ACCESS_KEY} or {SAS_TOKEN}"
                )
            }),
        }
    }
}

pub(super) fn from_env() -> anyhow::Result<Option<ObjectStorageConfig>> {
    parse(|key| env::var(key).ok())
}

fn parse(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<ObjectStorageConfig>> {
    let value = |key: &str| get(key).filter(|value| !value.is_empty());
    let required = |key: &str| {
        value(key)
            .ok_or_else(|| anyhow::anyhow!("{key} must be set when object storage is configured"))
    };
    let configured = [BACKEND, ENDPOINT, PREFIX]
        .into_iter()
        .chain(S3_KEYS)
        .chain(AZURE_KEYS)
        .any(|key| get(key).is_some());
    if !configured {
        return Ok(None);
    }

    let backend = match value(BACKEND).as_deref().unwrap_or("s3") {
        "s3" => {
            for key in AZURE_KEYS {
                anyhow::ensure!(get(key).is_none(), "{key} requires {BACKEND}=azure");
            }
            let mut config = S3ObjectStoreConfig::new(required(BUCKET)?);
            config.endpoint = value(ENDPOINT);
            config.region = value(REGION).unwrap_or_else(|| "us-east-1".into());
            config.access_key_id = value("AWS_ACCESS_KEY_ID");
            config.secret_access_key = value("AWS_SECRET_ACCESS_KEY");
            config.force_path_style = value(PATH_STYLE)
                .map(|value| value.parse::<bool>())
                .transpose()
                .map_err(|_| anyhow::anyhow!("{PATH_STYLE} must be true or false"))?
                .unwrap_or(false);
            ObjectStorageConfig::S3(config)
        }
        "azure" => {
            for key in S3_KEYS {
                anyhow::ensure!(
                    get(key).is_none(),
                    "{key} is only supported with {BACKEND}=s3"
                );
            }
            let credential = match (value(ACCESS_KEY), value(SAS_TOKEN)) {
                (Some(key), None) => AzureObjectStoreCredential::AccessKey(key),
                (None, Some(token)) => AzureObjectStoreCredential::SasToken(token),
                _ => anyhow::bail!(
                    "set exactly one of {ACCESS_KEY} or {SAS_TOKEN} for Azure storage"
                ),
            };
            let endpoint = value(ENDPOINT);
            if let Some(endpoint) = &endpoint {
                validate_azure_endpoint(endpoint)?;
            }
            ObjectStorageConfig::Azure(AzureObjectStoreConfig {
                account: required(ACCOUNT)?,
                container: required(CONTAINER)?,
                credential,
                endpoint,
            })
        }
        _ => anyhow::bail!("{BACKEND} must be s3 or azure"),
    };
    Ok(Some(backend))
}

fn validate_azure_endpoint(endpoint: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(endpoint)
        .map_err(|_| anyhow::anyhow!("{ENDPOINT} must be a valid account endpoint URL"))?;
    anyhow::ensure!(
        url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "{ENDPOINT} must have a host and contain no credentials, query, or fragment; use {SAS_TOKEN} for SAS"
    );
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    anyhow::ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "{ENDPOINT} must use HTTPS; HTTP is supported only for loopback emulators"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(entries: &[(&str, &str)]) -> anyhow::Result<Option<ObjectStorageConfig>> {
        parse(|key| {
            entries
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).into())
        })
    }

    fn azure() -> Vec<(&'static str, &'static str)> {
        vec![
            (BACKEND, "azure"),
            (ACCOUNT, "account"),
            (CONTAINER, "container"),
            (ACCESS_KEY, "secret-key"),
        ]
    }

    #[test]
    fn storage_stays_optional_and_aws_credentials_do_not_enable_it() {
        assert!(config(&[]).unwrap().is_none());
        assert!(
            config(&[
                ("AWS_ACCESS_KEY_ID", "key"),
                ("AWS_SECRET_ACCESS_KEY", "secret")
            ])
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn legacy_s3_settings_preserve_defaults_and_credentials() {
        let config = config(&[
            (BUCKET, "bucket"),
            (ENDPOINT, "http://minio:9000"),
            (PATH_STYLE, "true"),
            ("AWS_ACCESS_KEY_ID", "key"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
        ])
        .unwrap()
        .unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("secret"));
        let ObjectStorageConfig::S3(config) = config else {
            panic!("expected S3")
        };
        assert_eq!(config.bucket, "bucket");
        assert_eq!(config.region, "us-east-1");
        assert_eq!(config.access_key_id.as_deref(), Some("key"));
        assert_eq!(config.secret_access_key.as_deref(), Some("secret"));
        assert!(config.force_path_style);
    }

    #[test]
    fn incomplete_or_invalid_configuration_is_rejected_without_values() {
        for entries in [
            vec![(PREFIX, "prefix")],
            vec![(BUCKET, "")],
            vec![(BACKEND, "unknown-secret")],
            vec![(BUCKET, "bucket"), (PATH_STYLE, "secret-bool")],
            vec![(ACCOUNT, "account")],
        ] {
            let error = config(&entries).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn azure_requires_account_container_and_exactly_one_credential() {
        for key in [ACCOUNT, CONTAINER, ACCESS_KEY] {
            let mut entries = azure();
            entries.retain(|(name, _)| *name != key);
            assert!(config(&entries).is_err(), "missing {key}");
        }
        let mut entries = azure();
        entries.push((SAS_TOKEN, "sig=secret-sas"));
        let error = config(&entries).unwrap_err().to_string();
        assert!(!error.contains("secret-sas"));
        assert!(!error.contains("secret-key"));
    }

    #[test]
    fn azure_selects_account_key_or_sas_and_keeps_shared_settings() {
        for (key, secret) in [(ACCESS_KEY, "secret-key"), (SAS_TOKEN, "?sig=secret-sas")] {
            let entries = [
                (BACKEND, "azure"),
                (ACCOUNT, "account"),
                (CONTAINER, "container"),
                (key, secret),
                (PREFIX, "deployment"),
            ];
            let config = config(&entries).unwrap().unwrap();
            assert!(!format!("{config:?}").contains(secret));
            assert!(matches!(config, ObjectStorageConfig::Azure(_)));
        }
    }

    #[test]
    fn azure_rejects_s3_settings_and_unsafe_endpoints() {
        for key in S3_KEYS {
            let mut entries = azure();
            entries.push((key, "value"));
            assert!(config(&entries).is_err());
        }
        for endpoint in [
            "http://blob.example",
            "file:///tmp/blob",
            "https://blob.example?sig=secret-sas",
            "https://user:secret-key@blob.example",
            "https://blob.example/#secret",
        ] {
            let mut entries = azure();
            entries.push((ENDPOINT, endpoint));
            let error = config(&entries).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
        for endpoint in [
            "https://account.blob.core.windows.net",
            "http://127.0.0.1:10000/account",
            "http://[::1]:10000/account",
        ] {
            let mut entries = azure();
            entries.push((ENDPOINT, endpoint));
            assert!(config(&entries).is_ok());
        }
    }

    #[test]
    fn malformed_sas_builder_error_does_not_expose_credential() {
        let entries = [
            (BACKEND, "azure"),
            (ACCOUNT, "account"),
            (CONTAINER, "container"),
            (SAS_TOKEN, "invalid-secret-sas"),
        ];
        let error = config(&entries).unwrap().unwrap().build().unwrap_err();
        assert!(!format!("{error:#}").contains("invalid-secret-sas"));
    }
}

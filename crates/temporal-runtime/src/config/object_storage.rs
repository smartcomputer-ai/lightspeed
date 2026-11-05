use std::{env, sync::Arc};

use object_store::ObjectStore;
use store_pg::{
    AzureObjectStoreConfig, AzureObjectStoreCredential, AzureWorkloadIdentity, S3ObjectStoreConfig,
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
const AUTH_MODE: &str = "LIGHTSPEED_OBJECT_STORE_AZURE_AUTH_MODE";
// The standard identity variables are read only in workload identity mode. They
// are not in AZURE_KEYS because other software in the pod may set them.
const CLIENT_ID: &str = "AZURE_CLIENT_ID";
const TENANT_ID: &str = "AZURE_TENANT_ID";
const TOKEN_FILE: &str = "AZURE_FEDERATED_TOKEN_FILE";
const AUTHORITY_HOST: &str = "AZURE_AUTHORITY_HOST";
const AZURE_KEYS: [&str; 5] = [ACCOUNT, CONTAINER, ACCESS_KEY, SAS_TOKEN, AUTH_MODE];
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
                     {ENDPOINT}, and the credential selected by {AUTH_MODE}"
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
            let credential = azure_credential(&get)?;
            let endpoint = value(ENDPOINT);
            if let Some(endpoint) = &endpoint {
                validate_azure_endpoint(endpoint)?;
                anyhow::ensure!(
                    !matches!(credential, AzureObjectStoreCredential::WorkloadIdentity(_))
                        || endpoint.starts_with("https://"),
                    "{ENDPOINT} must use HTTPS with {AUTH_MODE}=workload_identity"
                );
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

fn azure_credential(
    get: &impl Fn(&str) -> Option<String>,
) -> anyhow::Result<AzureObjectStoreCredential> {
    let value = |key: &str| get(key).filter(|value| !value.is_empty());
    let (access_key, sas_token) = (value(ACCESS_KEY), value(SAS_TOKEN));
    match value(AUTH_MODE).as_deref() {
        Some("workload_identity") => {
            for key in [ACCESS_KEY, SAS_TOKEN] {
                anyhow::ensure!(
                    get(key).is_none(),
                    "{key} conflicts with {AUTH_MODE}=workload_identity"
                );
            }
            let required = |key: &str| {
                value(key).ok_or_else(|| {
                    anyhow::anyhow!("{key} must be set when {AUTH_MODE}=workload_identity")
                })
            };
            let identity = AzureWorkloadIdentity {
                client_id: required(CLIENT_ID)?,
                tenant_id: required(TENANT_ID)?,
                federated_token_file: required(TOKEN_FILE)?,
                authority_host: value(AUTHORITY_HOST),
            };
            identity.validate()?;
            Ok(AzureObjectStoreCredential::WorkloadIdentity(identity))
        }
        Some("access_key") => match (access_key, sas_token) {
            (Some(key), None) => Ok(AzureObjectStoreCredential::AccessKey(key)),
            _ => anyhow::bail!("{AUTH_MODE}=access_key requires {ACCESS_KEY} and no {SAS_TOKEN}"),
        },
        Some("sas_token") => match (access_key, sas_token) {
            (None, Some(token)) => Ok(AzureObjectStoreCredential::SasToken(token)),
            _ => anyhow::bail!("{AUTH_MODE}=sas_token requires {SAS_TOKEN} and no {ACCESS_KEY}"),
        },
        None => match (access_key, sas_token) {
            (Some(key), None) => Ok(AzureObjectStoreCredential::AccessKey(key)),
            (None, Some(token)) => Ok(AzureObjectStoreCredential::SasToken(token)),
            _ => anyhow::bail!(
                "set exactly one of {ACCESS_KEY} or {SAS_TOKEN} for Azure storage, \
                 or select another credential with {AUTH_MODE}"
            ),
        },
        Some(_) => {
            anyhow::bail!("{AUTH_MODE} must be access_key, sas_token, or workload_identity")
        }
    }
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

    fn workload_identity() -> Vec<(&'static str, &'static str)> {
        vec![
            (BACKEND, "azure"),
            (AUTH_MODE, "workload_identity"),
            (ACCOUNT, "account"),
            (CONTAINER, "container"),
            (PREFIX, "deployment"),
            (CLIENT_ID, "client-id"),
            (TENANT_ID, "tenant-id"),
            (
                TOKEN_FILE,
                "/var/run/secrets/azure/tokens/azure-identity-token",
            ),
        ]
    }

    fn selected_identity(entries: &[(&str, &str)]) -> AzureWorkloadIdentity {
        let Some(ObjectStorageConfig::Azure(config)) = config(entries).unwrap() else {
            panic!("expected Azure")
        };
        let AzureObjectStoreCredential::WorkloadIdentity(identity) = config.credential else {
            panic!("expected workload identity")
        };
        identity
    }

    #[test]
    fn workload_identity_reads_the_standard_azure_settings() {
        let identity = selected_identity(&workload_identity());
        assert_eq!(identity.client_id, "client-id");
        assert_eq!(identity.tenant_id, "tenant-id");
        assert_eq!(
            identity.federated_token_file,
            "/var/run/secrets/azure/tokens/azure-identity-token"
        );
        assert_eq!(identity.authority_host, None);
    }

    #[test]
    fn workload_identity_requires_each_identity_setting() {
        for key in [CLIENT_ID, TENANT_ID, TOKEN_FILE] {
            let mut entries = workload_identity();
            entries.retain(|(name, _)| *name != key);
            let error = config(&entries).unwrap_err().to_string();
            assert!(error.contains(key), "{error}");

            let mut entries = workload_identity();
            entries.retain(|(name, _)| *name != key);
            entries.push((key, ""));
            let error = config(&entries).unwrap_err().to_string();
            assert!(error.contains(key), "{error}");
        }
    }

    #[test]
    fn workload_identity_rejects_conflicting_credentials_without_values() {
        for key in [ACCESS_KEY, SAS_TOKEN] {
            let mut entries = workload_identity();
            entries.push((key, "secret-value"));
            let error = config(&entries).unwrap_err().to_string();
            assert!(error.contains(key), "{error}");
            assert!(!error.contains("secret-value"));
        }
    }

    #[test]
    fn workload_identity_validates_token_path_tenant_and_authority() {
        for (key, invalid) in [
            (TOKEN_FILE, "relative/token"),
            (TENANT_ID, "../other-tenant"),
            (TENANT_ID, "tenant/path"),
            (AUTHORITY_HOST, "http://login.microsoftonline.com"),
            (AUTHORITY_HOST, "login.microsoftonline.com"),
            (AUTHORITY_HOST, "https://login.microsoftonline.com/tenant"),
            (AUTHORITY_HOST, "https://login.microsoftonline.com/?x=1"),
            (
                AUTHORITY_HOST,
                "https://user:secret@login.microsoftonline.com/",
            ),
        ] {
            let mut entries = workload_identity();
            entries.retain(|(name, _)| *name != key);
            entries.push((key, invalid));
            let error = config(&entries).unwrap_err().to_string();
            assert!(error.contains(key), "{invalid}: {error}");
            assert!(!error.contains("secret"));
        }
        for authority in [
            "https://login.microsoftonline.com",
            "https://login.microsoftonline.com/",
            "https://login.microsoftonline.us:8443/",
        ] {
            let mut entries = workload_identity();
            entries.push((AUTHORITY_HOST, authority));
            assert_eq!(
                selected_identity(&entries).authority_host.as_deref(),
                Some(authority)
            );
        }
    }

    #[test]
    fn workload_identity_refuses_plaintext_endpoints() {
        let mut entries = workload_identity();
        entries.push((ENDPOINT, "http://127.0.0.1:10000/account"));
        assert!(config(&entries).is_err());

        let mut entries = workload_identity();
        entries.push((ENDPOINT, "https://account.blob.core.windows.net"));
        selected_identity(&entries);
    }

    #[test]
    fn building_workload_identity_needs_neither_token_file_nor_network() {
        let entries = workload_identity();
        config(&entries).unwrap().unwrap().build().unwrap();
    }

    #[test]
    fn unknown_auth_modes_are_rejected_without_echoing_the_value() {
        let mut entries = azure();
        entries.push((AUTH_MODE, "managed-identity-secret"));
        let error = config(&entries).unwrap_err().to_string();
        assert!(error.contains(AUTH_MODE), "{error}");
        assert!(!error.contains("managed-identity-secret"));
    }

    #[test]
    fn explicit_static_modes_select_only_their_credential() {
        let select = |mode, extra: &[(&'static str, &'static str)]| {
            let mut entries = vec![
                (BACKEND, "azure"),
                (ACCOUNT, "account"),
                (CONTAINER, "container"),
                (AUTH_MODE, mode),
            ];
            entries.extend_from_slice(extra);
            config(&entries)
        };
        let key = (ACCESS_KEY, "secret-key");
        let sas = (SAS_TOKEN, "sig=secret-sas");
        assert!(select("access_key", &[key]).is_ok());
        assert!(select("sas_token", &[sas]).is_ok());
        for result in [
            select("access_key", &[sas]),
            select("access_key", &[key, sas]),
            select("sas_token", &[key]),
            select("sas_token", &[key, sas]),
        ] {
            assert!(result.is_err());
        }
    }

    #[test]
    fn standard_azure_variables_do_not_activate_storage_or_change_the_selection() {
        let identity_variables = [
            (CLIENT_ID, "client-id"),
            (TENANT_ID, "tenant-id"),
            (TOKEN_FILE, "/token"),
            (AUTHORITY_HOST, "not-a-url"),
            ("AZURE_CLIENT_SECRET", "secret"),
        ];
        assert!(config(&identity_variables).unwrap().is_none());

        let mut s3 = vec![(BUCKET, "bucket")];
        s3.extend_from_slice(&identity_variables);
        assert!(matches!(
            config(&s3).unwrap().unwrap(),
            ObjectStorageConfig::S3(_)
        ));

        let mut access_key = azure();
        access_key.extend_from_slice(&identity_variables);
        let Some(ObjectStorageConfig::Azure(selected)) = config(&access_key).unwrap() else {
            panic!("expected Azure")
        };
        assert!(matches!(
            selected.credential,
            AzureObjectStoreCredential::AccessKey(_)
        ));
    }

    #[test]
    fn auth_mode_requires_the_azure_backend() {
        for entries in [
            vec![(AUTH_MODE, "workload_identity")],
            vec![(BUCKET, "bucket"), (AUTH_MODE, "workload_identity")],
        ] {
            let error = config(&entries).unwrap_err().to_string();
            assert!(error.contains(BACKEND) || error.contains(BUCKET), "{error}");
        }
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

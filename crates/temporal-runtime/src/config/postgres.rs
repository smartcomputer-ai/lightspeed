use std::env;

use sqlx::PgPool;
use store_pg::{
    AzureWorkloadIdentity, connect_postgres_with_workload_identity, postgres_pool_options,
};

use super::object_storage::workload_identity_settings;

const AUTH_MODE: &str = "LIGHTSPEED_POSTGRES_AUTH_MODE";

/// Connect to the deployment database without inspecting or changing its
/// schema. Migration and diagnostic commands use this lower-level boundary.
pub async fn postgres_pool_from_env() -> anyhow::Result<PgPool> {
    let database_url = env::var("LIGHTSPEED_POSTGRES_URL")
        .or_else(|_| env::var("LIGHTSPEED_TEST_POSTGRES_URL"))
        .map_err(|_| {
            anyhow::anyhow!("LIGHTSPEED_POSTGRES_URL or LIGHTSPEED_TEST_POSTGRES_URL must be set")
        })?;
    match workload_identity(|key| env::var(key).ok())? {
        Some(identity) => {
            Ok(connect_postgres_with_workload_identity(&database_url, identity).await?)
        }
        None => Ok(postgres_pool_options().connect(&database_url).await?),
    }
}

fn workload_identity(
    get: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<Option<AzureWorkloadIdentity>> {
    match get(AUTH_MODE).filter(|mode| !mode.is_empty()).as_deref() {
        None | Some("password") => Ok(None),
        Some("workload_identity") => workload_identity_settings(&get, AUTH_MODE).map(Some),
        Some(_) => anyhow::bail!("{AUTH_MODE} must be password or workload_identity"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [(&str, &str); 3] = [
        ("AZURE_CLIENT_ID", "client-id"),
        ("AZURE_TENANT_ID", "tenant-id"),
        ("AZURE_FEDERATED_TOKEN_FILE", "/var/run/token"),
    ];

    fn selected(entries: &[(&str, &str)]) -> anyhow::Result<Option<AzureWorkloadIdentity>> {
        workload_identity(|key| {
            entries
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn password_stays_the_default_and_azure_variables_do_not_change_it() {
        assert!(selected(&[]).unwrap().is_none());
        assert!(selected(&IDENTITY).unwrap().is_none());
        assert!(selected(&[(AUTH_MODE, "")]).unwrap().is_none());
        let mut explicit = IDENTITY.to_vec();
        explicit.push((AUTH_MODE, "password"));
        assert!(selected(&explicit).unwrap().is_none());
    }

    #[test]
    fn workload_identity_reads_the_standard_settings() {
        let mut entries = IDENTITY.to_vec();
        entries.push((AUTH_MODE, "workload_identity"));
        let identity = selected(&entries).unwrap().unwrap();
        assert_eq!(identity.client_id, "client-id");
        assert_eq!(identity.tenant_id, "tenant-id");
        assert_eq!(identity.federated_token_file, "/var/run/token");
    }

    #[test]
    fn workload_identity_requires_each_setting() {
        for (key, _) in IDENTITY {
            let mut entries = IDENTITY.to_vec();
            entries.retain(|(name, _)| *name != key);
            entries.push((AUTH_MODE, "workload_identity"));
            let error = selected(&entries).unwrap_err().to_string();
            assert!(error.contains(key), "{error}");
            assert!(error.contains(AUTH_MODE), "{error}");
        }
    }

    #[test]
    fn invalid_settings_and_unknown_modes_are_rejected_without_echoing_values() {
        let mut entries = IDENTITY.to_vec();
        entries.push((AUTH_MODE, "workload_identity"));
        entries.push(("AZURE_AUTHORITY_HOST", "http://login.example"));
        assert!(selected(&entries).is_err());

        let error = selected(&[(AUTH_MODE, "secret-mode")])
            .unwrap_err()
            .to_string();
        assert!(error.contains(AUTH_MODE), "{error}");
        assert!(!error.contains("secret-mode"));
    }
}

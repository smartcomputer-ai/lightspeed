use std::{env, sync::Arc};

use engine::{ModelSelection, ProviderApiKind};
use store_pg::{
    PgStore, PgStoreConfig, S3ObjectStoreConfig, SecretsMasterKey, build_s3_object_store,
};
use temporal_workflow::DEFAULT_MODEL;
use uuid::Uuid;

pub fn default_model_from_env() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: env::var("FORGE_CHAT_PROVIDER").unwrap_or_else(|_| "openai".to_owned()),
        model: env::var("FORGE_CHAT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned()),
    }
}

pub async fn pg_store_from_env() -> anyhow::Result<Arc<PgStore>> {
    let database_url = env::var("FORGE_POSTGRES_URL")
        .or_else(|_| env::var("FORGE_TEST_POSTGRES_URL"))
        .map_err(|_| {
            anyhow::anyhow!("FORGE_POSTGRES_URL or FORGE_TEST_POSTGRES_URL must be set")
        })?;
    let universe_id = env::var("FORGE_PG_UNIVERSE_ID")
        .map_err(|_| anyhow::anyhow!("FORGE_PG_UNIVERSE_ID must be set"))?;
    let universe_id = Uuid::parse_str(&universe_id)
        .map_err(|error| anyhow::anyhow!("invalid FORGE_PG_UNIVERSE_ID: {error}"))?;
    let config = pg_store_config_from_env(universe_id)?;
    let store = match object_store_config_from_env()? {
        Some(object_config) => {
            let object_store = build_s3_object_store(object_config)?;
            PgStore::connect_with_object_store(&database_url, object_store, config).await?
        }
        None => PgStore::connect(&database_url, config).await?,
    };
    Ok(Arc::new(store))
}

fn pg_store_config_from_env(universe_id: Uuid) -> anyhow::Result<PgStoreConfig> {
    let mut config = PgStoreConfig::new(universe_id);
    if let Ok(prefix) = env::var("FORGE_OBJECT_STORE_PREFIX") {
        config = config.with_object_prefix(prefix);
    }
    if let Some(master_key) = optional_env("FORGE_SECRETS_MASTER_KEY") {
        let master_key = SecretsMasterKey::from_base64(&master_key)
            .map_err(|error| anyhow::anyhow!("invalid FORGE_SECRETS_MASTER_KEY: {error}"))?;
        config = config.with_secrets_master_key(master_key);
    }
    Ok(config)
}

fn object_store_config_from_env() -> anyhow::Result<Option<S3ObjectStoreConfig>> {
    let object_env_present = [
        "FORGE_OBJECT_STORE_BUCKET",
        "FORGE_OBJECT_STORE_ENDPOINT",
        "FORGE_OBJECT_STORE_REGION",
        "FORGE_OBJECT_STORE_PREFIX",
        "FORGE_OBJECT_STORE_FORCE_PATH_STYLE",
    ]
    .into_iter()
    .any(|key| env::var_os(key).is_some());
    let Some(bucket) = optional_env("FORGE_OBJECT_STORE_BUCKET") else {
        return if object_env_present {
            Err(anyhow::anyhow!(
                "FORGE_OBJECT_STORE_BUCKET must be set when object store env is configured"
            ))
        } else {
            Ok(None)
        };
    };

    let mut config = S3ObjectStoreConfig::new(bucket);
    if let Some(endpoint) = optional_env("FORGE_OBJECT_STORE_ENDPOINT") {
        config = config.with_endpoint(endpoint);
    }
    config = config.with_region(
        optional_env("FORGE_OBJECT_STORE_REGION").unwrap_or_else(|| "us-east-1".to_owned()),
    );
    if let Some(access_key_id) = optional_env("AWS_ACCESS_KEY_ID") {
        config = config.with_access_key_id(access_key_id);
    }
    if let Some(secret_access_key) = optional_env("AWS_SECRET_ACCESS_KEY") {
        config = config.with_secret_access_key(secret_access_key);
    }
    if let Some(force_path_style) = optional_env("FORGE_OBJECT_STORE_FORCE_PATH_STYLE") {
        config =
            config.with_force_path_style(force_path_style.parse::<bool>().map_err(|error| {
                anyhow::anyhow!("invalid FORGE_OBJECT_STORE_FORCE_PATH_STYLE: {error}")
            })?);
    }
    Ok(Some(config))
}

fn optional_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.is_empty())
}

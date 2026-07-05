use std::{env, sync::Arc};

use engine::{ModelSelection, ProviderApiKind};
use object_store::ObjectStore;
use sqlx::{PgPool, postgres::PgPoolOptions};
use store_pg::{
    BlobCache, PgStore, PgStoreConfig, S3ObjectStoreConfig, SecretsMasterKey, build_s3_object_store,
};
use temporal_workflow::{DEFAULT_MODEL, DEFAULT_TASK_QUEUE};
use uuid::Uuid;

pub fn default_model_from_env() -> ModelSelection {
    ModelSelection {
        api_kind: ProviderApiKind::OpenAiResponses,
        provider_id: env::var("LIGHTSPEED_CHAT_PROVIDER").unwrap_or_else(|_| "openai".to_owned()),
        model: env::var("LIGHTSPEED_CHAT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned()),
    }
}

pub fn universe_id_from_env() -> anyhow::Result<Uuid> {
    let universe_id = env::var("LIGHTSPEED_PG_UNIVERSE_ID")
        .map_err(|_| anyhow::anyhow!("LIGHTSPEED_PG_UNIVERSE_ID must be set"))?;
    Uuid::parse_str(&universe_id)
        .map_err(|error| anyhow::anyhow!("invalid LIGHTSPEED_PG_UNIVERSE_ID: {error}"))
}

/// How the gateway resolves the universe (tenant) and principal of each
/// request.
///
/// Lightspeed requires a resolved universe per request but is unopinionated
/// about how it is produced. `Single` pins the whole deployment to one
/// configured universe (the pre-P90 behavior). `TrustedHeader` reads
/// `x-lightspeed-universe` (and optionally `x-lightspeed-principal`) injected
/// by an upstream gateway that owns authentication; requests without the
/// header are rejected (fail closed), and unknown universes are never
/// auto-created — universes exist only through explicit creation
/// (`operator/universes/create` or `server universe create`). `ApiKey`
/// resolves `Authorization: Bearer lsk_…` against the deployment-level
/// api_keys table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatewayAuthMode {
    Single { universe_id: Uuid },
    TrustedHeader,
    ApiKey,
}

pub fn gateway_auth_mode_from_env() -> anyhow::Result<GatewayAuthMode> {
    if optional_env("LIGHTSPEED_UNIVERSE_AUTO_CREATE").is_some() {
        anyhow::bail!(
            "LIGHTSPEED_UNIVERSE_AUTO_CREATE is retired: universes are created explicitly \
             via operator/universes/create (or `server universe create`); remove the variable"
        );
    }
    let mode = env::var("LIGHTSPEED_AUTH_MODE").unwrap_or_else(|_| "single".to_owned());
    match mode.as_str() {
        "single" | "" => Ok(GatewayAuthMode::Single {
            universe_id: universe_id_from_env()?,
        }),
        "trusted-header" => Ok(GatewayAuthMode::TrustedHeader),
        "api-key" => Ok(GatewayAuthMode::ApiKey),
        other => anyhow::bail!(
            "invalid LIGHTSPEED_AUTH_MODE={other:?}; expected one of: single, trusted-header, api-key"
        ),
    }
}

/// Default CAS blob-cache budget per process. One default for every role:
/// in `both` mode the gateway and worker share a single cache anyway, and
/// gateway-only deployments that want less set `LIGHTSPEED_BLOB_CACHE_BYTES`.
pub const BLOB_CACHE_DEFAULT_BYTES: u64 = 256 * 1024 * 1024;

/// Blobs larger than this bypass the cache so one media blob cannot flush
/// the working set of small hot blobs (context entries, schemas, prompts).
const BLOB_CACHE_MAX_ENTRY_BYTES: usize = 2 * 1024 * 1024;

/// CAS blob cache from the environment: `LIGHTSPEED_BLOB_CACHE_BYTES`
/// overrides the default; `0` disables caching.
pub fn blob_cache_from_env() -> anyhow::Result<Option<Arc<BlobCache>>> {
    let bytes = match optional_env("LIGHTSPEED_BLOB_CACHE_BYTES") {
        Some(value) => value
            .parse::<u64>()
            .map_err(|error| anyhow::anyhow!("invalid LIGHTSPEED_BLOB_CACHE_BYTES: {error}"))?,
        None => BLOB_CACHE_DEFAULT_BYTES,
    };
    if bytes == 0 {
        return Ok(None);
    }
    Ok(Some(Arc::new(BlobCache::new(
        bytes,
        BLOB_CACHE_MAX_ENTRY_BYTES,
    ))))
}

/// Resolve the Temporal task queue for this deployment: an explicit
/// `LIGHTSPEED_TASK_QUEUE` wins, otherwise the shared deployment queue
/// (`lightspeed-agent`). All universes of a deployment share one queue; the
/// universe-prefixed workflow id keeps their sessions apart. Deployments
/// sharing a Temporal namespace must set distinct explicit queues.
pub fn task_queue_from_env() -> anyhow::Result<String> {
    if let Some(task_queue) = optional_env("LIGHTSPEED_TASK_QUEUE") {
        return Ok(task_queue);
    }
    Ok(DEFAULT_TASK_QUEUE.to_owned())
}

/// Deployment-scoped storage handles shared by every universe: one Postgres
/// pool, one optional object store, and the per-universe `PgStoreConfig`
/// template (object prefix, secrets master key). Universe-bound `PgStore`
/// instances are stamped out of this via [`DeploymentStores::store_for`].
#[derive(Clone)]
pub struct DeploymentStores {
    pool: PgPool,
    object_store: Option<Arc<dyn ObjectStore>>,
    object_prefix: Option<String>,
    secrets_master_key: Option<SecretsMasterKey>,
    blob_cache: Option<Arc<BlobCache>>,
}

impl DeploymentStores {
    pub async fn from_env() -> anyhow::Result<Self> {
        let database_url = env::var("LIGHTSPEED_POSTGRES_URL")
            .or_else(|_| env::var("LIGHTSPEED_TEST_POSTGRES_URL"))
            .map_err(|_| {
                anyhow::anyhow!(
                    "LIGHTSPEED_POSTGRES_URL or LIGHTSPEED_TEST_POSTGRES_URL must be set"
                )
            })?;
        let pool = PgPoolOptions::new().connect(&database_url).await?;
        PgStore::migrate(&pool).await?;
        let object_store = match object_store_config_from_env()? {
            Some(object_config) => Some(build_s3_object_store(object_config)?),
            None => None,
        };
        let secrets_master_key = match optional_env("LIGHTSPEED_SECRETS_MASTER_KEY") {
            Some(master_key) => {
                Some(SecretsMasterKey::from_base64(&master_key).map_err(|error| {
                    anyhow::anyhow!("invalid LIGHTSPEED_SECRETS_MASTER_KEY: {error}")
                })?)
            }
            None => None,
        };
        Ok(Self {
            pool,
            object_store,
            object_prefix: optional_env("LIGHTSPEED_OBJECT_STORE_PREFIX"),
            secrets_master_key,
            blob_cache: None,
        })
    }

    /// Attach the deployment's shared CAS blob cache. Universe-bound stores
    /// stamped from these deployment stores all share it; entries are keyed
    /// by `(universe_id, blob_ref)`, so tenancy isolation is preserved.
    pub fn with_blob_cache(mut self, blob_cache: Option<Arc<BlobCache>>) -> Self {
        self.blob_cache = blob_cache;
        self
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn object_store(&self) -> Option<&Arc<dyn ObjectStore>> {
        self.object_store.as_ref()
    }

    /// Build the universe-bound store. Does not create the universe row;
    /// callers decide existence policy first (see `UniverseRuntime`).
    pub fn store_for(&self, universe_id: Uuid) -> Arc<PgStore> {
        self.store_for_with_slug(universe_id, None)
    }

    pub fn store_for_with_slug(
        &self,
        universe_id: Uuid,
        universe_slug: Option<String>,
    ) -> Arc<PgStore> {
        let mut config = PgStoreConfig::new(universe_id);
        if let Some(slug) = universe_slug {
            config = config.with_universe_slug(slug);
        }
        if let Some(prefix) = &self.object_prefix {
            config = config.with_object_prefix(prefix.clone());
        }
        if let Some(master_key) = &self.secrets_master_key {
            config = config.with_secrets_master_key(master_key.clone());
        }
        let store = match &self.object_store {
            Some(object_store) => {
                PgStore::with_object_store(self.pool.clone(), object_store.clone(), config)
            }
            None => PgStore::new(self.pool.clone(), config),
        };
        let store = match &self.blob_cache {
            Some(blob_cache) => store.with_blob_cache(blob_cache.clone()),
            None => store,
        };
        Arc::new(store)
    }
}

/// Single-universe store bound to `LIGHTSPEED_PG_UNIVERSE_ID`. Used by
/// `single`-mode deployments, tests, and tools that operate on one universe.
pub async fn pg_store_from_env() -> anyhow::Result<Arc<PgStore>> {
    let universe_id = universe_id_from_env()?;
    let stores = DeploymentStores::from_env()
        .await?
        .with_blob_cache(blob_cache_from_env()?);
    let store = stores.store_for(universe_id);
    store.ensure_universe().await?;
    Ok(store)
}

fn object_store_config_from_env() -> anyhow::Result<Option<S3ObjectStoreConfig>> {
    let object_env_present = [
        "LIGHTSPEED_OBJECT_STORE_BUCKET",
        "LIGHTSPEED_OBJECT_STORE_ENDPOINT",
        "LIGHTSPEED_OBJECT_STORE_REGION",
        "LIGHTSPEED_OBJECT_STORE_PREFIX",
        "LIGHTSPEED_OBJECT_STORE_FORCE_PATH_STYLE",
    ]
    .into_iter()
    .any(|key| env::var_os(key).is_some());
    let Some(bucket) = optional_env("LIGHTSPEED_OBJECT_STORE_BUCKET") else {
        return if object_env_present {
            Err(anyhow::anyhow!(
                "LIGHTSPEED_OBJECT_STORE_BUCKET must be set when object store env is configured"
            ))
        } else {
            Ok(None)
        };
    };

    let mut config = S3ObjectStoreConfig::new(bucket);
    if let Some(endpoint) = optional_env("LIGHTSPEED_OBJECT_STORE_ENDPOINT") {
        config = config.with_endpoint(endpoint);
    }
    config = config.with_region(
        optional_env("LIGHTSPEED_OBJECT_STORE_REGION").unwrap_or_else(|| "us-east-1".to_owned()),
    );
    if let Some(access_key_id) = optional_env("AWS_ACCESS_KEY_ID") {
        config = config.with_access_key_id(access_key_id);
    }
    if let Some(secret_access_key) = optional_env("AWS_SECRET_ACCESS_KEY") {
        config = config.with_secret_access_key(secret_access_key);
    }
    if let Some(force_path_style) = optional_env("LIGHTSPEED_OBJECT_STORE_FORCE_PATH_STYLE") {
        config =
            config.with_force_path_style(force_path_style.parse::<bool>().map_err(|error| {
                anyhow::anyhow!("invalid LIGHTSPEED_OBJECT_STORE_FORCE_PATH_STYLE: {error}")
            })?);
    }
    Ok(Some(config))
}

fn optional_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.is_empty())
}

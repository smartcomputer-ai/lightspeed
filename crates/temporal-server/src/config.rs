use std::{env, sync::Arc, time::Duration};

use engine::{ModelSelection, ProviderApiKind};
use object_store::ObjectStore;
use sqlx::{PgPool, postgres::PgPoolOptions};
use store_pg::{
    BlobCache, PgStore, PgStoreConfig, PgStoreError, S3ObjectStoreConfig, SchemaStatus,
    SecretsMasterKey, build_s3_object_store,
};
use temporal_workflow::{DEFAULT_MODEL, DEFAULT_TASK_QUEUE, bots::DEFAULT_BOTS_TASK_QUEUE};
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
/// configured universe (the legacy single-universe behavior). `TrustedHeader` reads
/// `x-lightspeed-universe` (and optionally `x-lightspeed-principal`) injected
/// by an upstream gateway that owns authentication; requests without the
/// header are rejected (fail closed), and unknown universes are never
/// auto-created — universes exist only through explicit creation
/// (`deployment/universes/create` or `server universe create`). `ApiKey`
/// resolves `Authorization: Bearer lsk_…` against the deployment-level
/// api_keys table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatewayAuthMode {
    Single { universe_id: Uuid },
    TrustedHeader,
    ApiKey,
}

/// Optional base URL outbound daemons are told to dial for data connections
/// when it differs from the general public base URL (for example a dedicated
/// hostname in front of the environment gateway).
pub fn environment_public_url_from_env() -> anyhow::Result<Option<String>> {
    let Some(value) = optional_env("LIGHTSPEED_ENVIRONMENT_PUBLIC_URL") else {
        return Ok(None);
    };
    if !["https://", "http://", "wss://", "ws://"]
        .iter()
        .any(|scheme| value.starts_with(scheme))
    {
        anyhow::bail!(
            "LIGHTSPEED_ENVIRONMENT_PUBLIC_URL must be an http(s) or ws(s) URL, got {value:?}"
        );
    }
    Ok(Some(value.trim_end_matches('/').to_owned()))
}

pub fn gateway_auth_mode_from_env() -> anyhow::Result<GatewayAuthMode> {
    if optional_env("LIGHTSPEED_UNIVERSE_AUTO_CREATE").is_some() {
        anyhow::bail!(
            "LIGHTSPEED_UNIVERSE_AUTO_CREATE is retired: universes are created explicitly \
             via deployment/universes/create (or `server universe create`); remove the variable"
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

/// `LIGHTSPEED_LLM_DEBUG_DUMPS`: store every generation's raw provider
/// request and response as unrooted debug blobs. Off unless set to `true`.
pub fn llm_debug_dumps_from_env() -> anyhow::Result<bool> {
    match optional_env("LIGHTSPEED_LLM_DEBUG_DUMPS") {
        None => Ok(false),
        Some(value) => value.parse::<bool>().map_err(|error| {
            anyhow::anyhow!(
                "invalid LIGHTSPEED_LLM_DEBUG_DUMPS={value:?}: {error}; expected true or false"
            )
        }),
    }
}

/// Default minimum age since a blob's last put before a sweep may collect
/// it. Long enough to cover the longest activity, sub-agent, or environment
/// job that holds a ref before appending it, and a human uploading through
/// the blob API before starting a run.
pub const CAS_SWEEP_DEFAULT_GRACE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Blob-collection grace from `LIGHTSPEED_CAS_SWEEP_GRACE_MS`; `None` when
/// the variable is `0`, which disables the sweeper.
pub fn cas_sweep_grace_from_env() -> anyhow::Result<Option<Duration>> {
    let grace_ms = match optional_env("LIGHTSPEED_CAS_SWEEP_GRACE_MS") {
        Some(value) => value
            .parse::<u64>()
            .map_err(|error| anyhow::anyhow!("invalid LIGHTSPEED_CAS_SWEEP_GRACE_MS: {error}"))?,
        None => return Ok(Some(CAS_SWEEP_DEFAULT_GRACE)),
    };
    Ok((grace_ms > 0).then(|| Duration::from_millis(grace_ms)))
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

/// Default task queue of the `channels` worker role.
pub const DEFAULT_CHANNELS_TASK_QUEUE: &str = "lightspeed-channels";

/// One Temporal task queue per worker role. The gateway knows all of them
/// because it starts sessions, wakes bot controllers, and starts
/// conversations; a worker role serves only its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskQueues {
    pub sessions: String,
    pub bots: String,
    pub channels: String,
}

impl TaskQueues {
    pub fn defaults() -> Self {
        Self {
            sessions: DEFAULT_TASK_QUEUE.to_owned(),
            bots: DEFAULT_BOTS_TASK_QUEUE.to_owned(),
            channels: DEFAULT_CHANNELS_TASK_QUEUE.to_owned(),
        }
    }

    /// Every queue derived from one sessions queue name — for tests that
    /// isolate a whole deployment under a random prefix.
    pub fn derived_from(sessions: impl Into<String>) -> Self {
        let sessions = sessions.into();
        Self {
            bots: format!("{sessions}-bots"),
            channels: format!("{sessions}-channels"),
            sessions,
        }
    }

    pub fn for_role(&self, role: crate::roles::Role) -> Option<&str> {
        match role {
            crate::roles::Role::Gateway | crate::roles::Role::EnvironmentGateway => None,
            crate::roles::Role::Sessions => Some(&self.sessions),
            crate::roles::Role::Bots => Some(&self.bots),
            crate::roles::Role::Channels => Some(&self.channels),
        }
    }
}

/// `LIGHTSPEED_TASK_QUEUE` (sessions), `LIGHTSPEED_TASK_QUEUE_BOTS`, and
/// `LIGHTSPEED_TASK_QUEUE_CHANNELS`, with the deployment defaults.
pub fn task_queues_from_env() -> anyhow::Result<TaskQueues> {
    let defaults = TaskQueues::defaults();
    Ok(TaskQueues {
        sessions: task_queue_from_env()?,
        bots: optional_env("LIGHTSPEED_TASK_QUEUE_BOTS").unwrap_or(defaults.bots),
        channels: optional_env("LIGHTSPEED_TASK_QUEUE_CHANNELS").unwrap_or(defaults.channels),
    })
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
        let allow_unledgered_schema = allow_unledgered_schema_from_env()?;
        let pool = postgres_pool_from_env().await?;
        verify_runtime_schema(&pool, allow_unledgered_schema).await?;
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

    /// Key prefix every object of this deployment lives under; empty when
    /// the bucket is used bare.
    pub fn object_prefix(&self) -> &str {
        self.object_prefix.as_deref().unwrap_or("")
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

/// Explicit compatibility escape hatch for deployments that provision the
/// Lightspeed tables outside the embedded migrator. It affects runtime startup
/// only: migration and schema diagnostic commands continue to inspect and
/// enforce the ledger normally.
pub fn allow_unledgered_schema_from_env() -> anyhow::Result<bool> {
    optional_env("LIGHTSPEED_ALLOW_UNLEDGERED_SCHEMA")
        .map(|value| {
            value.parse::<bool>().map_err(|error| {
                anyhow::anyhow!(
                    "invalid LIGHTSPEED_ALLOW_UNLEDGERED_SCHEMA={value:?}: {error}; expected true or false"
                )
            })
        })
        .transpose()
        .map(|value| value.unwrap_or(false))
}

async fn verify_runtime_schema(pool: &PgPool, allow_unledgered: bool) -> anyhow::Result<()> {
    if let Some(relations) =
        evaluate_schema_verification(store_pg::verify_schema(pool).await, allow_unledgered)?
    {
        tracing::warn!(
            target: "temporal_server",
            tables = ?relations,
            "running with an externally managed, unledgered PostgreSQL schema; Lightspeed cannot verify its compatibility"
        );
    }
    Ok(())
}

fn evaluate_schema_verification(
    result: Result<SchemaStatus, PgStoreError>,
    allow_unledgered: bool,
) -> Result<Option<Vec<String>>, PgStoreError> {
    match result {
        Ok(_) => Ok(None),
        Err(PgStoreError::UnledgeredSchema { relations }) if allow_unledgered => {
            Ok(Some(relations))
        }
        Err(error) => Err(error),
    }
}

/// Connect to the deployment database without inspecting or changing its
/// schema. Migration and diagnostic commands use this lower-level boundary.
pub async fn postgres_pool_from_env() -> anyhow::Result<PgPool> {
    let database_url = env::var("LIGHTSPEED_POSTGRES_URL")
        .or_else(|_| env::var("LIGHTSPEED_TEST_POSTGRES_URL"))
        .map_err(|_| {
            anyhow::anyhow!("LIGHTSPEED_POSTGRES_URL or LIGHTSPEED_TEST_POSTGRES_URL must be set")
        })?;
    Ok(PgPoolOptions::new().connect(&database_url).await?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unledgered_schema_bypass_is_narrow() {
        let relations = vec!["sessions".to_owned(), "universes".to_owned()];
        let accepted = evaluate_schema_verification(
            Err(PgStoreError::UnledgeredSchema {
                relations: relations.clone(),
            }),
            true,
        )
        .expect("explicit bypass accepts an unledgered schema");
        assert_eq!(accepted, Some(relations));

        assert!(matches!(
            evaluate_schema_verification(
                Err(PgStoreError::UnledgeredSchema {
                    relations: vec!["sessions".to_owned()],
                }),
                false,
            ),
            Err(PgStoreError::UnledgeredSchema { .. })
        ));
        assert!(matches!(
            evaluate_schema_verification(
                Err(PgStoreError::MigrationRequired {
                    current_revision: 6,
                    required_revision: 7,
                }),
                true,
            ),
            Err(PgStoreError::MigrationRequired { .. })
        ));
    }
}

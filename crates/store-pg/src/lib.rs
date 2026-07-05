//! PostgreSQL-backed storage adapters.
//!
//! `PgStore` is scoped to one universe. Within that universe, sessions share a
//! CAS catalog; across universes, both metadata and object keys are isolated.

mod api_keys;
mod auth;
mod blob;
mod blob_cache;
mod environment;
mod environment_jobs;
mod mcp;
mod messaging;
mod oauth;
mod object;
mod operator;
mod profile;
mod providers;
mod session;
mod shared;
mod vfs;

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use sqlx::{Executor, PgPool, postgres::PgPoolOptions};
use thiserror::Error;
use uuid::Uuid;

pub const CORE_SCHEMA_SQL: &str = include_str!("../migrations/001_core.sql");
pub const VFS_SCHEMA_SQL: &str = include_str!("../migrations/002_vfs.sql");
pub const MCP_SCHEMA_SQL: &str = include_str!("../migrations/003_mcp.sql");
pub const AUTH_SCHEMA_SQL: &str = include_str!("../migrations/004_auth.sql");
pub const MESSAGING_SCHEMA_SQL: &str = include_str!("../migrations/005_messaging.sql");
pub const ENVIRONMENT_SCHEMA_SQL: &str = include_str!("../migrations/006_environments.sql");
pub const PROFILE_SCHEMA_SQL: &str = include_str!("../migrations/007_agent_profiles.sql");
pub const API_KEYS_SCHEMA_SQL: &str = include_str!("../migrations/008_api_keys.sql");

pub const DEFAULT_INLINE_THRESHOLD_BYTES: usize = 64 * 1024;

/// 32-byte AES-256-GCM master key for the secret store. `Debug` output is
/// redacted; construct from base64 deployment config.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretsMasterKey([u8; 32]);

impl SecretsMasterKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn from_base64(value: &str) -> Result<Self, PgStoreError> {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(value.trim())
            .map_err(|error| PgStoreError::Store {
                message: format!("secrets master key is not valid base64: {error}"),
            })?;
        let bytes: [u8; 32] =
            decoded
                .try_into()
                .map_err(|decoded: Vec<u8>| PgStoreError::Store {
                    message: format!(
                        "secrets master key must decode to 32 bytes, got {}",
                        decoded.len()
                    ),
                })?;
        Ok(Self(bytes))
    }

    pub(crate) fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for SecretsMasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretsMasterKey(<redacted>)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PgStoreConfig {
    pub universe_id: Uuid,
    pub universe_slug: Option<String>,
    pub inline_threshold_bytes: usize,
    pub object_prefix: String,
    pub secrets_master_key: Option<SecretsMasterKey>,
}

impl PgStoreConfig {
    pub fn new(universe_id: Uuid) -> Self {
        Self {
            universe_id,
            universe_slug: None,
            inline_threshold_bytes: DEFAULT_INLINE_THRESHOLD_BYTES,
            object_prefix: String::new(),
            secrets_master_key: None,
        }
    }

    pub fn with_universe_slug(mut self, slug: impl Into<String>) -> Self {
        self.universe_slug = Some(slug.into());
        self
    }

    pub fn with_inline_threshold_bytes(mut self, threshold: usize) -> Self {
        self.inline_threshold_bytes = threshold;
        self
    }

    pub fn with_object_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.object_prefix = prefix.into();
        self
    }

    pub fn with_secrets_master_key(mut self, key: SecretsMasterKey) -> Self {
        self.secrets_master_key = Some(key);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct S3ObjectStoreConfig {
    pub bucket: String,
    pub endpoint: Option<String>,
    pub region: String,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub force_path_style: bool,
}

impl S3ObjectStoreConfig {
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            endpoint: None,
            region: "us-east-1".to_owned(),
            access_key_id: None,
            secret_access_key: None,
            force_path_style: false,
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    pub fn with_access_key_id(mut self, access_key_id: impl Into<String>) -> Self {
        self.access_key_id = Some(access_key_id.into());
        self
    }

    pub fn with_secret_access_key(mut self, secret_access_key: impl Into<String>) -> Self {
        self.secret_access_key = Some(secret_access_key.into());
        self
    }

    pub fn with_force_path_style(mut self, force_path_style: bool) -> Self {
        self.force_path_style = force_path_style;
        self
    }
}

pub fn build_s3_object_store(
    config: S3ObjectStoreConfig,
) -> Result<Arc<dyn ObjectStore>, object_store::Error> {
    let mut builder = AmazonS3Builder::new()
        .with_bucket_name(config.bucket)
        .with_region(config.region)
        .with_virtual_hosted_style_request(!config.force_path_style);
    if let Some(endpoint) = config.endpoint {
        let allow_http = endpoint.starts_with("http://");
        builder = builder.with_endpoint(endpoint).with_allow_http(allow_http);
    }
    if let Some(access_key_id) = config.access_key_id {
        builder = builder.with_access_key_id(access_key_id);
    }
    if let Some(secret_access_key) = config.secret_access_key {
        builder = builder.with_secret_access_key(secret_access_key);
    }
    Ok(Arc::new(builder.build()?))
}

#[derive(Clone)]
pub struct PgStore {
    pub(crate) pool: PgPool,
    pub(crate) object_store: Option<Arc<dyn ObjectStore>>,
    pub(crate) config: PgStoreConfig,
    pub(crate) blob_cache: Option<Arc<BlobCache>>,
}

#[derive(Debug, Error)]
pub enum PgStoreError {
    #[error("postgres failure: {0}")]
    Postgres(#[from] sqlx::Error),

    #[error("postgres store failure: {message}")]
    Store { message: String },
}

impl PgStore {
    pub fn new(pool: PgPool, config: PgStoreConfig) -> Self {
        Self {
            pool,
            object_store: None,
            config,
            blob_cache: None,
        }
    }

    pub fn with_object_store(
        pool: PgPool,
        object_store: Arc<dyn ObjectStore>,
        config: PgStoreConfig,
    ) -> Self {
        Self {
            pool,
            object_store: Some(object_store),
            config,
            blob_cache: None,
        }
    }

    /// Attach a shared in-memory blob cache. The cache may be shared across
    /// universe-bound stores: entries are keyed by `(universe_id, blob_ref)`,
    /// so tenancy isolation matches the `cas_blobs` primary key.
    pub fn with_blob_cache(mut self, blob_cache: Arc<BlobCache>) -> Self {
        self.blob_cache = Some(blob_cache);
        self
    }

    pub async fn connect(database_url: &str, config: PgStoreConfig) -> Result<Self, PgStoreError> {
        let pool = PgPoolOptions::new().connect(database_url).await?;
        let store = Self::new(pool, config);
        store.initialize().await?;
        Ok(store)
    }

    pub async fn connect_with_object_store(
        database_url: &str,
        object_store: Arc<dyn ObjectStore>,
        config: PgStoreConfig,
    ) -> Result<Self, PgStoreError> {
        let pool = PgPoolOptions::new().connect(database_url).await?;
        let store = Self::with_object_store(pool, object_store, config);
        store.initialize().await?;
        Ok(store)
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn config(&self) -> &PgStoreConfig {
        &self.config
    }

    pub fn object_store(&self) -> Option<&Arc<dyn ObjectStore>> {
        self.object_store.as_ref()
    }

    pub async fn migrate(pool: &PgPool) -> Result<(), PgStoreError> {
        pool.execute(CORE_SCHEMA_SQL).await?;
        pool.execute(VFS_SCHEMA_SQL).await?;
        pool.execute(MCP_SCHEMA_SQL).await?;
        pool.execute(AUTH_SCHEMA_SQL).await?;
        pool.execute(MESSAGING_SCHEMA_SQL).await?;
        pool.execute(ENVIRONMENT_SCHEMA_SQL).await?;
        pool.execute(PROFILE_SCHEMA_SQL).await?;
        pool.execute(API_KEYS_SCHEMA_SQL).await?;
        Ok(())
    }

    pub async fn initialize(&self) -> Result<(), PgStoreError> {
        Self::migrate(&self.pool).await?;
        self.ensure_universe().await?;
        Ok(())
    }

    pub async fn ensure_universe(&self) -> Result<(), PgStoreError> {
        sqlx::query(
            r#"
            INSERT INTO universes (universe_id, slug)
            VALUES ($1, $2)
            ON CONFLICT (universe_id) DO NOTHING
            "#,
        )
        .bind(self.config.universe_id)
        .bind(self.config.universe_slug.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

pub use api_keys::PgApiKeyStore;
pub use blob_cache::BlobCache;
pub use operator::{
    UniverseOutboundMessage, UniverseStats, create_universe, delete_universe,
    list_universe_object_keys, list_universe_session_ids, list_universe_stats,
    read_pending_outbound_all_universes, read_universe_stats,
};

/// Deployment-level universe listing for admin surfaces.
pub async fn list_universes(pool: &PgPool) -> Result<Vec<(Uuid, Option<String>)>, PgStoreError> {
    let rows: Vec<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT universe_id, slug FROM universes ORDER BY universe_id")
            .fetch_all(pool)
            .await?;
    Ok(rows)
}

/// Deployment-level check whether a universe exists. Runs above the
/// per-universe `PgStore` boundary: multi-universe deployments consult it
/// before lazily constructing a universe's store.
pub async fn universe_exists(pool: &PgPool, universe_id: Uuid) -> Result<bool, PgStoreError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT universe_id FROM universes WHERE universe_id = $1")
            .bind(universe_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.is_some())
}

/// Deployment-level reverse lookup from an OAuth authorization callback's
/// hashed `state` parameter to the universe that owns the flow.
///
/// The OAuth callback is hit by external authorization servers and carries no
/// tenant header; its universe must be resolved from server-side state. Flow
/// rows are universe-scoped, so this is the one query that intentionally
/// searches across universes — `state_hash` values are high-entropy and
/// unique per flow.
pub async fn find_auth_flow_universe(
    pool: &PgPool,
    state_hash: &str,
) -> Result<Option<Uuid>, PgStoreError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT universe_id FROM auth_flows WHERE state_hash = $1")
            .bind(state_hash)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(universe_id,)| universe_id))
}

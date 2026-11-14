//! PostgreSQL-backed storage adapters.
//!
//! `PgStore` is scoped to one universe. Within that universe, sessions share a
//! CAS catalog; across universes, both metadata and object keys are isolated.

mod access;
mod api_keys;
mod auth;
mod azure;
mod blob;
mod blob_cache;
mod bots;
mod cas_sweep;
pub mod changes;
mod channels;
mod deployment;
mod environment;
mod environment_registration;
mod mcp;
mod migrations;
mod model_defaults;
mod oauth;
mod object;
mod profile;
mod providers;
mod runtime_instances;
mod s3;
mod session;
pub use session::PurgeSessionError;
mod shared;
mod vfs;

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use object_store::ObjectStore;
use sqlx::{PgPool, postgres::PgPoolOptions};
use thiserror::Error;
use uuid::Uuid;

pub use access::AccessFilter;
pub use access::{AccessStoreError, PgAccessStore, ResourceAccess};
pub use azure::{
    AzureObjectStoreConfig, AzureObjectStoreCredential, AzurePostgresError, AzureWorkloadIdentity,
    AzureWorkloadIdentityError, TokenAcquisitionError, build_azure_object_store,
    connect_postgres_with_workload_identity,
};
pub use model_defaults::ModelDefaultsStoreError;
pub use s3::{S3ObjectStoreConfig, build_s3_object_store};

/// A session page with the access summary each view carries.
#[derive(Clone, Debug)]
pub struct SessionListPageWithAccess {
    pub sessions: Vec<(harness::storage::SessionRecord, api::ResourceAccessSummary)>,
    pub next_cursor: Option<harness::storage::SessionListCursor>,
}

pub const CORE_SCHEMA_SQL: &str = include_str!("../migrations/001_core.sql");
pub const VFS_SCHEMA_SQL: &str = include_str!("../migrations/002_vfs.sql");
pub const MCP_SCHEMA_SQL: &str = include_str!("../migrations/003_mcp.sql");
pub const AUTH_SCHEMA_SQL: &str = include_str!("../migrations/004_auth.sql");
pub const ENVIRONMENT_SCHEMA_SQL: &str = include_str!("../migrations/005_environments.sql");
pub const PROFILE_SCHEMA_SQL: &str = include_str!("../migrations/006_agent_profiles.sql");
pub const API_KEYS_SCHEMA_SQL: &str = include_str!("../migrations/007_api_keys.sql");
pub const BOTS_SCHEMA_SQL: &str = include_str!("../migrations/008_bots.sql");
pub const CHANNELS_SCHEMA_SQL: &str = include_str!("../migrations/009_channels.sql");

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
    pub universe_handle: Option<String>,
    pub inline_threshold_bytes: usize,
    pub object_prefix: String,
    pub secrets_master_key: Option<SecretsMasterKey>,
}

impl PgStoreConfig {
    pub fn new(universe_id: Uuid) -> Self {
        Self {
            universe_id,
            universe_handle: None,
            inline_threshold_bytes: DEFAULT_INLINE_THRESHOLD_BYTES,
            object_prefix: String::new(),
            secrets_master_key: None,
        }
    }

    pub fn with_universe_handle(mut self, handle: impl Into<String>) -> Self {
        self.universe_handle = Some(handle.into());
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

/// Pool settings for deployment processes. They limit how often connections open,
/// because each one costs a TLS handshake and server-side authentication. Connections
/// are still checked before use, so a longer lifetime does not hand out dead ones.
pub fn postgres_pool_options() -> PgPoolOptions {
    PgPoolOptions::new()
        // Stay warm through quiet periods instead of reconnecting for the next query.
        .min_connections(2)
        .max_lifetime(std::time::Duration::from_secs(60 * 60))
}

#[derive(Clone)]
pub struct PgStore {
    pub(crate) pool: PgPool,
    pub(crate) object_store: Option<Arc<dyn ObjectStore>>,
    pub(crate) config: PgStoreConfig,
    pub(crate) blob_cache: Option<Arc<BlobCache>>,
    pub(crate) changes: Arc<changes::ChangeBus>,
}

#[derive(Debug, Error)]
pub enum PgStoreError {
    #[error("postgres failure: {0}")]
    Postgres(#[from] sqlx::Error),

    #[error("postgres store failure: {message}")]
    Store { message: String },

    #[error(
        "database schema revision {current_revision} is stale; revision {required_revision} is required (run `lightspeed-runtime migrate`)"
    )]
    MigrationRequired {
        current_revision: i64,
        required_revision: i64,
    },

    #[error(
        "found unledgered Lightspeed tables {relations:?}; refusing to baseline an existing schema (reset the full Lightspeed schema or database before running `lightspeed-runtime migrate`)"
    )]
    UnledgeredSchema { relations: Vec<String> },

    #[error(
        "database schema revision {current_revision} is newer than this server's required revision {required_revision}"
    )]
    SchemaTooNew {
        current_revision: i64,
        required_revision: i64,
    },

    #[error("migration {version} name changed: expected {expected:?}, found {actual:?}")]
    MigrationNameChanged {
        version: i64,
        expected: &'static str,
        actual: String,
    },

    #[error(
        "migration {version} ({name}) checksum changed: expected {expected}, found {actual}; shipped migrations are immutable"
    )]
    MigrationChecksumChanged {
        version: i64,
        name: &'static str,
        expected: String,
        actual: String,
    },

    #[error("migration ledger is not a contiguous prefix: revision {missing_version} is missing")]
    MigrationLedgerGap { missing_version: i64 },
}

impl PgStore {
    pub fn with_changes(mut self, changes: Arc<changes::ChangeBus>) -> Self {
        self.changes = changes;
        self
    }

    pub fn changes(&self) -> &Arc<changes::ChangeBus> {
        &self.changes
    }

    pub(crate) fn changed(&self, kind: changes::ChangeKind, id: impl Into<String>) {
        self.changes
            .publish(changes::Change::new(self.config.universe_id, kind, id));
    }

    pub fn new(pool: PgPool, config: PgStoreConfig) -> Self {
        Self {
            pool,
            object_store: None,
            config,
            blob_cache: None,
            changes: Arc::new(changes::ChangeBus::default()),
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
            changes: Arc::new(changes::ChangeBus::default()),
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
        migrations::migrate(pool).await?;
        Ok(())
    }

    pub async fn initialize(&self) -> Result<(), PgStoreError> {
        Self::migrate(&self.pool).await?;
        self.ensure_universe().await?;
        Ok(())
    }

    /// Create this store's universe when it does not exist yet, under the
    /// configured handle or the default one. An existing universe keeps its
    /// handle: provisioning never renames.
    pub async fn ensure_universe(&self) -> Result<(), PgStoreError> {
        deployment::create_universe_with_handle(
            &self.pool,
            self.config.universe_id,
            self.config.universe_handle.as_deref(),
        )
        .await?;
        Ok(())
    }
}

pub use api_keys::{PgApiKeyStore, PutApiKey};
pub use blob_cache::BlobCache;
pub use cas_sweep::{
    CasObjectDeletion, CasSweepCandidate, CasSweepCursor, CasSweepError, CasSweepLeader,
    CasSweepPage,
};
pub use channels::list_channel_accounts_all;
pub use deployment::{
    UNIVERSE_HANDLE_MAX_LEN, UniverseStats, create_universe, create_universe_with_handle,
    default_universe_handle, delete_universe, is_valid_universe_handle, list_universe_object_keys,
    list_universe_session_ids, list_universe_stats, put_universe_configuration,
    put_universe_handle, read_universe_stats, resolve_universe_reference,
};
pub use environment::{
    list_revoked_environment_routes, list_universes_with_environment_work,
    list_universes_with_open_provisioned_environments, list_universes_with_registered_environments,
    repair_registered_connections,
};
pub use environment_registration::{
    find_registered_environment_universe, find_registration_key_universe,
};
pub use migrations::{
    MIGRATIONS, REQUIRED_SCHEMA_REVISION, SchemaStatus, schema_status, verify_schema,
};
pub use object::{delete_objects_under_prefix, universe_cas_object_prefix};
pub use runtime_instances::{
    RegisteredConnectionOwner, RuntimeInstance, RuntimeLoop, RuntimeLoopLeader,
    delete_runtime_instance, disconnect_runtime_instance_connections, read_live_runtime_instance,
    read_registered_connection_owner, read_registered_connection_owners, register_runtime_instance,
    renew_runtime_instance, sweep_expired_registered_connections,
};

/// Deployment-level universe listing for admin surfaces.
pub async fn list_universes(pool: &PgPool) -> Result<Vec<(Uuid, String)>, PgStoreError> {
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT universe_id, handle FROM universes ORDER BY universe_id")
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

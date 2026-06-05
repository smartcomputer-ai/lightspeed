//! PostgreSQL-backed storage adapters.
//!
//! `PgStore` is scoped to one universe. Within that universe, sessions share a
//! CAS catalog; across universes, both metadata and object keys are isolated.

mod blob;
mod object;
mod session;
mod shared;
mod vfs;

use std::sync::Arc;

use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use sqlx::{Executor, PgPool, postgres::PgPoolOptions};
use thiserror::Error;
use uuid::Uuid;

pub const INITIAL_SCHEMA_SQL: &str = include_str!("../migrations/001_initial.sql");

pub const DEFAULT_INLINE_THRESHOLD_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PgStoreConfig {
    pub universe_id: Uuid,
    pub universe_slug: Option<String>,
    pub inline_threshold_bytes: usize,
    pub object_prefix: String,
}

impl PgStoreConfig {
    pub fn new(universe_id: Uuid) -> Self {
        Self {
            universe_id,
            universe_slug: None,
            inline_threshold_bytes: DEFAULT_INLINE_THRESHOLD_BYTES,
            object_prefix: String::new(),
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
        }
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
        pool.execute(INITIAL_SCHEMA_SQL).await?;
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

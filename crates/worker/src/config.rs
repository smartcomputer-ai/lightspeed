use std::{env, sync::Arc};

use store_pg::{PgStore, PgStoreConfig};
use uuid::Uuid;

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
    Ok(Arc::new(
        PgStore::connect(&database_url, PgStoreConfig::new(universe_id)).await?,
    ))
}

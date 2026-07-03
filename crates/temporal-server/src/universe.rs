//! Per-universe runtime state over shared deployment resources.
//!
//! One deployment (gateway + worker) serves many universes. Postgres rows,
//! object keys, and workflow ids are universe-scoped already; this module owns
//! the runtime side: a lazy registry that stamps out one `PgStore`,
//! `GatewayAgentApi`, and `ActivityState` per universe over the shared pool,
//! Temporal client, and task queue.

use std::{collections::BTreeMap, sync::Arc};

use store_pg::PgStore;
use temporalio_client::Client;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    config::DeploymentStores,
    fleet::AgentApiFleetRuntime,
    gateway::GatewayAgentApi,
    worker::ActivityState,
};

#[derive(Debug, Error)]
pub enum UniverseError {
    #[error("unknown universe: {universe_id}")]
    Unknown { universe_id: Uuid },

    #[error("universe runtime failure: {0}")]
    Runtime(#[from] anyhow::Error),
}

/// Everything the runtime holds for one universe: the universe-bound store,
/// the gateway service instance (also used by fleet spawns), and the worker
/// activity dependencies. Child sessions spawned through fleet inherit the
/// universe because the fleet runtime wraps this universe's `api`.
pub struct UniverseState {
    pub universe_id: Uuid,
    pub store: Arc<PgStore>,
    pub api: Arc<GatewayAgentApi>,
    pub activities: Arc<ActivityState>,
}

/// Lazy universe registry. `state_for` returns the cached state or builds it,
/// applying the caller's existence policy: the gateway may auto-create
/// universes (`trusted-header` mode with auto-create, or `single` mode for its
/// pinned universe); the worker never creates, because a workflow for a
/// universe the gateway did not create is a routing error, not a provisioning
/// request.
pub struct UniverseRuntime {
    client: Client,
    task_queue: String,
    public_base_url: Option<String>,
    stores: DeploymentStores,
    states: tokio::sync::Mutex<BTreeMap<Uuid, Arc<UniverseState>>>,
}

impl UniverseRuntime {
    pub fn new(
        client: Client,
        task_queue: String,
        public_base_url: Option<String>,
        stores: DeploymentStores,
    ) -> Self {
        Self {
            client,
            task_queue,
            public_base_url,
            stores,
            states: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }

    pub fn task_queue(&self) -> &str {
        &self.task_queue
    }

    pub fn stores(&self) -> &DeploymentStores {
        &self.stores
    }

    pub async fn state_for(
        &self,
        universe_id: Uuid,
        create_missing: bool,
    ) -> Result<Arc<UniverseState>, UniverseError> {
        let mut states = self.states.lock().await;
        if let Some(state) = states.get(&universe_id) {
            return Ok(state.clone());
        }
        let exists = store_pg::universe_exists(self.stores.pool(), universe_id)
            .await
            .map_err(|error| UniverseError::Runtime(error.into()))?;
        if !exists && !create_missing {
            return Err(UniverseError::Unknown { universe_id });
        }
        let state = Arc::new(self.build_state(universe_id).await?);
        states.insert(universe_id, state.clone());
        Ok(state)
    }

    async fn build_state(&self, universe_id: Uuid) -> Result<UniverseState, UniverseError> {
        let store = self.stores.store_for(universe_id);
        store
            .ensure_universe()
            .await
            .map_err(|error| UniverseError::Runtime(error.into()))?;
        let mut api = GatewayAgentApi::builder(self.client.clone(), store.clone())
            .with_task_queue(self.task_queue.clone());
        if let Some(public_base_url) = &self.public_base_url {
            api = api.with_public_base_url(public_base_url.clone());
        }
        let api = Arc::new(api.build());
        let fleet_runtime = Arc::new(AgentApiFleetRuntime::new(api.clone()));
        let activities = Arc::new(ActivityState::from_pg_store_with_default_runtime_and_fleet(
            store.clone(),
            fleet_runtime,
        )?);
        Ok(UniverseState {
            universe_id,
            store,
            api,
            activities,
        })
    }
}

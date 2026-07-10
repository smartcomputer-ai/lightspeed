//! Per-universe runtime state over shared deployment resources.
//!
//! One deployment (gateway + worker) serves many universes. Postgres rows,
//! object keys, and workflow ids are universe-scoped already; this module owns
//! the runtime side: a lazy registry that stamps out one `PgStore`,
//! `GatewayAgentApi`, and `ActivityState` per universe over the shared pool,
//! Temporal client, HTTP clients, and task queue.
//!
//! Registry lifecycle: states build on first touch, are stamped with a
//! last-used time on every touch, and are evicted opportunistically on the
//! next touch of any universe — after hours of idleness, or LRU-first beyond
//! a large cap. There is deliberately no background sweeper: with all HTTP
//! clients deployment-shared, a lingering idle state is only resolver
//! wrappers and a tool registry, so a fully quiet process holds nothing
//! worth a task. Eviction is safe because states hold no durable data:
//! in-flight work keeps its own `Arc` alive, and the next touch rebuilds
//! from the shared pool and clients.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use auth::{GitHubApiClient, HttpGitHubApiClient, HttpOAuthTokenClient, OAuthTokenClient};
use llm_clients::{
    anthropic::messages as am, openai::audio as oai_audio, openai::responses as oai,
};
use store_pg::PgStore;
use temporalio_client::Client;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    config::DeploymentStores,
    fleet::AgentApiFleetRuntime,
    gateway::GatewayAgentApi,
    worker::{ActivityState, AudioTranscoder},
};

/// Evict universe states idle longer than this. Rebuilds are cheap, so this
/// only needs to be long enough that busy tenants never churn.
const UNIVERSE_IDLE_EVICT_MS: u64 = 4 * 60 * 60 * 1000;

/// Hard cap on cached universe states, evicted LRU-first. Sized far above any
/// expected concurrently-active tenant count; the idle sweep does the real
/// work.
const UNIVERSE_CACHE_CAP: usize = 1024;

#[derive(Debug, Error)]
pub enum UniverseError {
    #[error("unknown universe: {universe_id}")]
    Unknown { universe_id: Uuid },

    #[error("universe runtime failure: {0}")]
    Runtime(#[from] anyhow::Error),
}

/// Deployment-scoped HTTP clients shared by every universe's runtime state.
/// All of these are universe-agnostic — per-universe behavior (stored keys,
/// secrets, grants) comes from the resolver layers wrapped around them — so
/// constructing them once keeps the marginal cost of a cached universe near
/// zero.
pub struct DeploymentClients {
    pub(crate) openai: Arc<oai::Client>,
    pub(crate) openai_audio: Arc<oai_audio::Client>,
    pub(crate) anthropic: Arc<am::Client>,
    pub(crate) oauth_token: Arc<dyn OAuthTokenClient>,
    pub(crate) oauth_metadata: Arc<dyn auth::OAuthMetadataClient>,
    pub(crate) github: Arc<dyn GitHubApiClient>,
    pub(crate) audio_transcoder: Option<Arc<dyn AudioTranscoder>>,
}

impl DeploymentClients {
    pub fn from_env() -> anyhow::Result<Self> {
        let openai = Arc::new(
            oai::Client::new(oai::Config::from_env_allow_missing_key())
                .map_err(|error| anyhow::anyhow!("construct OpenAI client: {error}"))?,
        );
        let openai_audio = Arc::new(
            oai_audio::Client::new(oai_audio::Config::from_env_allow_missing_key())
                .map_err(|error| anyhow::anyhow!("construct OpenAI audio client: {error}"))?,
        );
        let anthropic = Arc::new(
            am::Client::new(am::Config::from_env_allow_missing_key())
                .map_err(|error| anyhow::anyhow!("construct Anthropic client: {error}"))?,
        );
        let oauth_token: Arc<dyn OAuthTokenClient> = Arc::new(
            HttpOAuthTokenClient::new()
                .map_err(|error| anyhow::anyhow!("construct oauth token client: {error}"))?,
        );
        let oauth_metadata: Arc<dyn auth::OAuthMetadataClient> = Arc::new(
            auth::HttpOAuthMetadataClient::new()
                .map_err(|error| anyhow::anyhow!("construct oauth metadata client: {error}"))?,
        );
        let github: Arc<dyn GitHubApiClient> = Arc::new(
            HttpGitHubApiClient::new()
                .map_err(|error| anyhow::anyhow!("construct github api client: {error}"))?,
        );
        let audio_transcoder = crate::worker::default_audio_transcoder_from_env()?;
        Ok(Self {
            openai,
            openai_audio,
            anthropic,
            oauth_token,
            oauth_metadata,
            github,
            audio_transcoder,
        })
    }
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

struct UniverseEntry {
    state: Arc<UniverseState>,
    last_used_ms: u64,
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
    clients: DeploymentClients,
    states: tokio::sync::Mutex<BTreeMap<Uuid, UniverseEntry>>,
}

impl UniverseRuntime {
    pub fn new(
        client: Client,
        task_queue: String,
        public_base_url: Option<String>,
        stores: DeploymentStores,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            client,
            task_queue,
            public_base_url,
            stores,
            clients: DeploymentClients::from_env()?,
            states: tokio::sync::Mutex::new(BTreeMap::new()),
        })
    }

    pub fn task_queue(&self) -> &str {
        &self.task_queue
    }

    pub fn stores(&self) -> &DeploymentStores {
        &self.stores
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Drop the universe's cached runtime state (operator purge). In-flight
    /// work holding an `Arc` finishes on the old state; nothing durable is
    /// lost because states hold no durable data.
    pub async fn evict(&self, universe_id: Uuid) {
        self.states.lock().await.remove(&universe_id);
    }

    pub async fn state_for(
        &self,
        universe_id: Uuid,
        create_missing: bool,
    ) -> Result<Arc<UniverseState>, UniverseError> {
        let now_ms = now_ms();
        let mut states = self.states.lock().await;
        if let Some(entry) = states.get_mut(&universe_id) {
            entry.last_used_ms = now_ms;
            let state = entry.state.clone();
            evict_universe_states(&mut states, now_ms, universe_id);
            return Ok(state);
        }
        let exists = store_pg::universe_exists(self.stores.pool(), universe_id)
            .await
            .map_err(|error| UniverseError::Runtime(error.into()))?;
        if !exists && !create_missing {
            return Err(UniverseError::Unknown { universe_id });
        }
        let state = Arc::new(self.build_state(universe_id).await?);
        states.insert(
            universe_id,
            UniverseEntry {
                state: state.clone(),
                last_used_ms: now_ms,
            },
        );
        evict_universe_states(&mut states, now_ms, universe_id);
        Ok(state)
    }

    async fn build_state(&self, universe_id: Uuid) -> Result<UniverseState, UniverseError> {
        let store = self.stores.store_for(universe_id);
        store
            .ensure_universe()
            .await
            .map_err(|error| UniverseError::Runtime(error.into()))?;
        let mut api = GatewayAgentApi::builder(self.client.clone(), store.clone())
            .with_task_queue(self.task_queue.clone())
            .with_oauth_token_client(self.clients.oauth_token.clone())
            .with_oauth_metadata_client(self.clients.oauth_metadata.clone())
            .with_github_api_client(self.clients.github.clone())
            .with_model_discovery_clients(
                self.clients.openai.clone(),
                self.clients.anthropic.clone(),
            );
        if let Some(public_base_url) = &self.public_base_url {
            api = api.with_public_base_url(public_base_url.clone());
        }
        let api = Arc::new(api.build());
        let fleet_runtime = Arc::new(AgentApiFleetRuntime::new(api.clone()));
        let activities = Arc::new(ActivityState::from_pg_store_with_shared_clients(
            store.clone(),
            Some(fleet_runtime),
            &self.clients,
            self.client.clone(),
            self.task_queue.clone(),
            universe_id,
        )?);
        Ok(UniverseState {
            universe_id,
            store,
            api,
            activities,
        })
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn evict_universe_states(states: &mut BTreeMap<Uuid, UniverseEntry>, now_ms: u64, just_used: Uuid) {
    let last_used = states
        .iter()
        .map(|(universe_id, entry)| (*universe_id, entry.last_used_ms))
        .collect::<BTreeMap<_, _>>();
    for universe_id in plan_universe_evictions(
        &last_used,
        now_ms,
        UNIVERSE_IDLE_EVICT_MS,
        UNIVERSE_CACHE_CAP,
        just_used,
    ) {
        states.remove(&universe_id);
        tracing::debug!(
            target: "temporal_server",
            universe_id = %universe_id,
            "evicted idle universe state"
        );
    }
}

/// Pure eviction plan over last-used timestamps: drop everything idle longer
/// than `idle_ms`, then drop LRU-first down to `cap`. `just_used` (the entry
/// the current call touched) is never evicted.
fn plan_universe_evictions(
    last_used: &BTreeMap<Uuid, u64>,
    now_ms: u64,
    idle_ms: u64,
    cap: usize,
    just_used: Uuid,
) -> Vec<Uuid> {
    let mut evict = Vec::new();
    let mut remaining = Vec::new();
    for (&universe_id, &used_ms) in last_used {
        if universe_id == just_used {
            continue;
        }
        if now_ms.saturating_sub(used_ms) > idle_ms {
            evict.push(universe_id);
        } else {
            remaining.push((used_ms, universe_id));
        }
    }
    let kept = remaining.len() + 1; // + the just-used entry
    if kept > cap {
        remaining.sort_unstable();
        evict.extend(
            remaining
                .iter()
                .take(kept - cap)
                .map(|(_, universe_id)| *universe_id),
        );
    }
    evict
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn idle_entries_are_evicted_but_never_the_just_used_one() {
        let now = 10_000_000;
        let idle = 1_000;
        let last_used = BTreeMap::from([
            (uuid(1), now - 5_000), // idle
            (uuid(2), now - 500),   // fresh
            (uuid(3), now - 5_000), // idle but just used
        ]);
        let evicted = plan_universe_evictions(&last_used, now, idle, 100, uuid(3));
        assert_eq!(evicted, vec![uuid(1)]);
    }

    #[test]
    fn over_cap_evicts_lru_first() {
        let now = 10_000_000;
        let last_used = BTreeMap::from([
            (uuid(1), now - 40), // oldest
            (uuid(2), now - 30),
            (uuid(3), now - 20),
            (uuid(4), now - 10), // just used
        ]);
        let evicted = plan_universe_evictions(&last_used, now, u64::MAX, 2, uuid(4));
        assert_eq!(evicted, vec![uuid(1), uuid(2)]);
    }

    #[test]
    fn under_cap_and_fresh_evicts_nothing() {
        let now = 10_000_000;
        let last_used = BTreeMap::from([(uuid(1), now - 10), (uuid(2), now)]);
        assert!(plan_universe_evictions(&last_used, now, 1_000, 100, uuid(2)).is_empty());
    }
}

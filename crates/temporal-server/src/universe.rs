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
    anthropic::messages as am,
    openai::{audio as oai_audio, completions as oai_completions, responses as oai},
};
use store_pg::PgStore;
use temporalio_client::Client;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    config::{DeploymentStores, TaskQueues},
    environment_gateway::EnvironmentGatewayClientConfig,
    gateway::GatewayAgentApi,
    subagents::AgentApiSubagentRuntime,
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
    pub(crate) openai_completions: Arc<oai_completions::Client>,
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
        let openai_completions = Arc::new(
            oai_completions::Client::new(oai_completions::Config::from_env_allow_missing_key())
                .map_err(|error| anyhow::anyhow!("construct OpenAI Completions client: {error}"))?,
        );
        let openai_audio = Arc::new(
            oai_audio::Client::new(oai_audio::Config::from_env_allow_missing_key())
                .map_err(|error| anyhow::anyhow!("construct OpenAI audio client: {error}"))?,
        );
        let anthropic = Arc::new(
            am::Client::new(am::Config::from_env_allow_missing_key())
                .map_err(|error| anyhow::anyhow!("construct Anthropic client: {error}"))?,
        );
        let allow_private_mcp = std::env::var("LIGHTSPEED_MCP_OAUTH_ALLOW_PRIVATE_NETWORKS")
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"));
        let oauth_metadata: Arc<dyn auth::OAuthMetadataClient> = Arc::new(
            auth::HttpOAuthMetadataClient::with_private_networks(allow_private_mcp),
        );
        let oauth_token: Arc<dyn OAuthTokenClient> = Arc::new(
            HttpOAuthTokenClient::new_with_mcp_http(oauth_metadata.clone())
                .map_err(|error| anyhow::anyhow!("construct oauth token client: {error}"))?,
        );
        let github: Arc<dyn GitHubApiClient> = Arc::new(
            HttpGitHubApiClient::new()
                .map_err(|error| anyhow::anyhow!("construct github api client: {error}"))?,
        );
        let audio_transcoder = crate::worker::default_audio_transcoder_from_env()?;
        Ok(Self {
            openai,
            openai_completions,
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
/// the gateway service instance (also used by sub-agent spawns), and the
/// worker activity dependencies. Sub-agent children inherit the universe
/// because the sub-agent runtime wraps this universe's `api`.
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
/// How often the power reaper polls daemon idle reports. Idle thresholds are
/// minutes to hours, so a coarse cadence keeps the data-plane load negligible.
pub const POWER_REAPER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// How often bot trigger Schedules are re-converged in the background.
pub const BOT_SCHEDULE_RECONCILE_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(10 * 60);

pub struct UniverseRuntime {
    client: Client,
    task_queues: TaskQueues,
    public_base_url: Option<String>,
    stores: DeploymentStores,
    clients: DeploymentClients,
    environment_gateway: EnvironmentGatewayClientConfig,
    states: tokio::sync::Mutex<BTreeMap<Uuid, UniverseEntry>>,
}

impl UniverseRuntime {
    pub fn new(
        client: Client,
        task_queue: String,
        public_base_url: Option<String>,
        stores: DeploymentStores,
    ) -> anyhow::Result<Self> {
        Self::new_with_environment_gateway(client, task_queue, public_base_url, stores, true)
    }

    /// `local_environment_gateway` says whether this process serves the
    /// environment routes itself; only then may the worker-side route default
    /// to the local public base URL. Every other process must be told where
    /// the environment gateway is.
    pub fn new_with_environment_gateway(
        client: Client,
        task_queue: String,
        public_base_url: Option<String>,
        stores: DeploymentStores,
        local_environment_gateway: bool,
    ) -> anyhow::Result<Self> {
        let environment_gateway = EnvironmentGatewayClientConfig::from_env(
            public_base_url
                .as_deref()
                .filter(|_| local_environment_gateway),
        )?;
        Ok(Self {
            client,
            task_queues: TaskQueues::derived_from(task_queue),
            public_base_url,
            stores,
            clients: DeploymentClients::from_env()?,
            environment_gateway,
            states: tokio::sync::Mutex::new(BTreeMap::new()),
        })
    }

    /// Use explicit per-role task queues instead of the ones derived from
    /// the sessions queue.
    pub fn with_task_queues(mut self, task_queues: TaskQueues) -> Self {
        self.task_queues = task_queues;
        self
    }

    /// The sessions task queue.
    pub fn task_queue(&self) -> &str {
        &self.task_queues.sessions
    }

    pub fn task_queues(&self) -> &TaskQueues {
        &self.task_queues
    }

    pub fn stores(&self) -> &DeploymentStores {
        &self.stores
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn environment_gateway(&self) -> &EnvironmentGatewayClientConfig {
        &self.environment_gateway
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

    /// Run the one active deployment lifecycle reconciler. Provider calls are
    /// idempotent, so process restart safely resumes any persisted intent.
    pub async fn run_environment_reconciler(self: Arc<Self>) {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut failures = crate::gateway::ReconcileFailureLog::default();
        loop {
            interval.tick().await;
            let universe_ids = match store_pg::list_universes_with_pending_environments(
                self.stores.pool(),
            )
            .await
            {
                Ok(ids) => ids,
                Err(error) => {
                    tracing::warn!(target: "temporal_server", %error, "environment reconciler scan failed");
                    continue;
                }
            };
            for universe_id in universe_ids {
                let state = match self.state_for(universe_id, false).await {
                    Ok(state) => state,
                    Err(error) => {
                        tracing::warn!(target: "temporal_server", %universe_id, %error, "environment reconciler could not resolve universe");
                        continue;
                    }
                };
                match state
                    .api
                    .environment_service()
                    .reconcile_environment_lifecycle_once()
                    .await
                {
                    Ok(_) => failures.succeeded(universe_id),
                    Err(error) => failures.failed(universe_id, &error),
                }
            }
        }
    }

    /// Run the one active power reaper. Every pass reads daemon idle
    /// reports for ready environments with an idle policy and records power
    /// intent; the lifecycle reconciler converges the provider. Failures are
    /// logged and retried on the next tick.
    pub async fn run_power_reaper(self: Arc<Self>) {
        let mut interval = tokio::time::interval(POWER_REAPER_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut failures = crate::gateway::ReconcileFailureLog::default();
        loop {
            interval.tick().await;
            let universe_ids = match store_pg::list_universes_with_idle_policies(self.stores.pool())
                .await
            {
                Ok(ids) => ids,
                Err(error) => {
                    tracing::warn!(target: "temporal_server", %error, "power reaper scan failed");
                    continue;
                }
            };
            for universe_id in universe_ids {
                let state = match self.state_for(universe_id, false).await {
                    Ok(state) => state,
                    Err(error) => {
                        tracing::warn!(target: "temporal_server", %universe_id, %error, "power reaper could not resolve universe");
                        continue;
                    }
                };
                match state
                    .api
                    .environment_service()
                    .reconcile_idle_power_once()
                    .await
                {
                    Ok(_) => failures.succeeded(universe_id),
                    Err(error) => failures.failed(universe_id, &error),
                }
            }
        }
    }

    /// Converge every bot trigger Schedule of every universe at boot and
    /// on a slow sweep afterwards. Trigger writes reconcile synchronously;
    /// this pass repairs what a crash between the row and the Schedule left
    /// behind.
    pub async fn run_bot_schedule_reconciler(self: Arc<Self>) {
        let mut interval = tokio::time::interval(BOT_SCHEDULE_RECONCILE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut failures = crate::gateway::ReconcileFailureLog::default();
        loop {
            interval.tick().await;
            let universes = match store_pg::list_universes(self.stores.pool()).await {
                Ok(universes) => universes,
                Err(error) => {
                    tracing::warn!(target: "temporal_server", %error, "bot schedule reconciler scan failed");
                    continue;
                }
            };
            for (universe_id, _slug) in universes {
                let state = match self.state_for(universe_id, false).await {
                    Ok(state) => state,
                    Err(error) => {
                        tracing::warn!(target: "temporal_server", %universe_id, %error, "bot schedule reconciler could not resolve universe");
                        continue;
                    }
                };
                match state.api.reconcile_bot_schedules_once().await {
                    Ok(_) => failures.succeeded(universe_id),
                    Err(error) => failures.failed(universe_id, &error),
                }
            }
        }
    }

    async fn build_state(&self, universe_id: Uuid) -> Result<UniverseState, UniverseError> {
        let store = self.stores.store_for(universe_id);
        store
            .ensure_universe()
            .await
            .map_err(|error| UniverseError::Runtime(error.into()))?;
        let mut api = GatewayAgentApi::builder(self.client.clone(), store.clone())
            .with_task_queue(self.task_queues.sessions.clone())
            .with_bot_task_queue(self.task_queues.bots.clone())
            .with_channel_task_queue(self.task_queues.channels.clone())
            .with_oauth_token_client(self.clients.oauth_token.clone())
            .with_oauth_metadata_client(self.clients.oauth_metadata.clone())
            .with_github_api_client(self.clients.github.clone())
            .with_model_discovery_clients(
                self.clients.openai.clone(),
                self.clients.anthropic.clone(),
            )
            .with_environment_gateway(self.environment_gateway.clone());
        if let Some(public_base_url) = &self.public_base_url {
            api = api.with_public_base_url(public_base_url.clone());
        }
        let api = Arc::new(api.build());
        let subagent_runtime = Arc::new(AgentApiSubagentRuntime::new(api.clone()));
        let activities = Arc::new(ActivityState::from_pg_store_with_shared_clients(
            store.clone(),
            Some(subagent_runtime),
            &self.clients,
            self.client.clone(),
            self.environment_gateway.clone(),
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

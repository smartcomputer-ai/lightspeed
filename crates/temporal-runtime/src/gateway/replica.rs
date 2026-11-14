//! Process identity, lease fencing, and leadership for environment gateways.
//!
//! PostgreSQL decides whether an instance's lease is live. A conservative
//! monotonic deadline also fences the process when PostgreSQL is unreachable.

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use sqlx::PgPool;
use tokio::{sync::watch, time::Instant};
use uuid::Uuid;

use super::http::GatewayState;

pub const INSTANCE_LEASE: Duration = Duration::from_secs(20);
pub const LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(5);
const DATABASE_TIMEOUT: Duration = Duration::from_secs(5);
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// One process's advertised address and admission state. Embedded test
/// gateways can remain local; production listeners always register a lease.
pub struct GatewayReplica {
    instance_id: Uuid,
    peer_url: Option<String>,
    registered: bool,
    lease_deadline: Mutex<Instant>,
    draining: AtomicBool,
    stopped: watch::Sender<bool>,
}

impl GatewayReplica {
    pub fn local() -> Self {
        Self::new(None, false, Instant::now())
    }

    fn new(peer_url: Option<String>, registered: bool, started: Instant) -> Self {
        Self {
            instance_id: Uuid::new_v4(),
            peer_url,
            registered,
            lease_deadline: Mutex::new(started + INSTANCE_LEASE),
            draining: AtomicBool::new(false),
            stopped: watch::channel(false).0,
        }
    }

    /// Register before accepting any daemon or worker sockets. The deadline
    /// starts before the SQL call, so a slow response cannot extend a lease.
    pub async fn register(
        pool: &PgPool,
        roles: &[String],
        peer_url: Option<&str>,
    ) -> anyhow::Result<Arc<Self>> {
        let peer_url = validate_peer_url(peer_url)?;
        let replica = Arc::new(Self::new(peer_url, true, Instant::now()));
        tokio::time::timeout(
            DATABASE_TIMEOUT,
            store_pg::register_runtime_instance(
                pool,
                replica.instance_id,
                roles,
                replica.peer_url(),
                INSTANCE_LEASE,
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("register environment gateway instance timed out"))??;
        Ok(replica)
    }

    pub fn peer_url_from_env() -> anyhow::Result<Option<String>> {
        let value = std::env::var("LIGHTSPEED_ENVIRONMENT_GATEWAY_PEER_URL").ok();
        let peer_url = validate_peer_url(value.as_deref())?;
        if peer_url.is_some() {
            anyhow::ensure!(
                std::env::var("LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN")
                    .ok()
                    .is_some_and(|token| !token.trim().is_empty()),
                "LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN must be explicitly configured and shared by all replicas when LIGHTSPEED_ENVIRONMENT_GATEWAY_PEER_URL is set"
            );
        }
        Ok(peer_url)
    }

    pub fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    pub fn peer_url(&self) -> Option<&str> {
        self.peer_url.as_deref()
    }

    pub fn is_registered(&self) -> bool {
        self.registered
    }

    pub fn lease_valid(&self) -> bool {
        !*self.stopped.borrow()
            && (!self.registered
                || Instant::now() < *self.lease_deadline.lock().expect("gateway lease poisoned"))
    }

    pub fn is_accepting(&self) -> bool {
        self.lease_valid() && !self.draining.load(Ordering::Acquire)
    }

    pub fn begin_draining(&self) {
        self.draining.store(true, Ordering::Release);
    }

    pub async fn stopped(&self) {
        let mut stopped = self.stopped.subscribe();
        let _ = stopped.wait_for(|stopped| *stopped).await;
    }

    fn fence(&self, state: &GatewayState) {
        self.begin_draining();
        self.stopped.send_replace(true);
        state.registrations().end_all_routes();
        state.registrations().close_controls();
    }

    /// Renew the instance and discard handles superseded on another
    /// replica. A lost or expired lease is terminal for this instance ID.
    pub async fn maintain(self: Arc<Self>, state: Arc<GatewayState>) {
        if !self.registered {
            return;
        }
        let mut interval = tokio::time::interval(LEASE_RENEW_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            let deadline = *self.lease_deadline.lock().expect("gateway lease poisoned");
            tokio::select! {
                _ = self.stopped() => return,
                _ = tokio::time::sleep_until(deadline) => break,
                _ = interval.tick() => {}
            }
            let started = Instant::now();
            let renewal = tokio::time::timeout_at(
                deadline.min(started + DATABASE_TIMEOUT),
                store_pg::renew_runtime_instance(state.pool(), self.instance_id, INSTANCE_LEASE),
            )
            .await;
            match renewal {
                Ok(Ok(true)) if Instant::now() < deadline => {
                    *self.lease_deadline.lock().expect("gateway lease poisoned") =
                        started + INSTANCE_LEASE;
                    // Ownership checks cannot hold up lease expiry fencing.
                    let checked =
                        tokio::time::timeout(DATABASE_TIMEOUT, self.revalidate(&state)).await;
                    match checked {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "gateway ownership revalidation failed")
                        }
                        Err(_) => tracing::warn!("gateway ownership revalidation timed out"),
                    }
                }
                Ok(Ok(_)) => break,
                Ok(Err(error)) => tracing::warn!(%error, "gateway lease renewal failed"),
                Err(_) => tracing::warn!("gateway lease renewal timed out"),
            }
        }
        tracing::error!(instance = %self.instance_id, "gateway lease lost; closing connections");
        self.fence(&state);
    }

    async fn revalidate(&self, state: &GatewayState) -> Result<(), store_pg::PgStoreError> {
        let snapshots = state.registrations().control_snapshots();
        if !snapshots.is_empty() {
            let keys: Vec<_> = snapshots
                .iter()
                .map(|(universe, environment, _)| (*universe, environment.clone()))
                .collect();
            let owners: HashMap<_, _> =
                store_pg::read_registered_connection_owners(state.pool(), &keys)
                    .await?
                    .into_iter()
                    .map(|(universe, environment, owner)| ((universe, environment), owner))
                    .collect();
            for (universe, environment, connection) in snapshots {
                let owner = owners.get(&(universe, environment.clone()));
                match owner {
                    Some(owner)
                        if owner.owner_instance_id == self.instance_id
                            && owner.control_connection_id == connection.as_str() => {}
                    Some(_) => state.registrations().remove_stale_control(
                        universe,
                        &environment,
                        &connection,
                    ),
                    None => state.registrations().close_stale_control(
                        universe,
                        &environment,
                        &connection,
                    ),
                }
            }
        }
        state.registrations().revalidate_routes(state.pool()).await
    }

    /// Existing data streams may finish while controls and the instance
    /// lease remain live. Once the deadline arrives, ordinary socket closure
    /// lets daemons reconnect using their existing protocol behavior.
    pub async fn drain(&self, state: &GatewayState) {
        self.begin_draining();
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        while (state.registrations().active_route_count() != 0
            || !state.registrations().control_snapshots().is_empty())
            && self.lease_valid()
        {
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.fence(state);
    }

    pub async fn unregister(&self, pool: &PgPool) {
        if !self.registered {
            return;
        }
        let cleanup = async {
            store_pg::disconnect_runtime_instance_connections(pool, self.instance_id).await?;
            store_pg::delete_runtime_instance(pool, self.instance_id).await
        };
        match tokio::time::timeout(DATABASE_TIMEOUT, cleanup).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, "gateway instance cleanup failed; lease expiry will repair it")
            }
            Err(_) => {
                tracing::warn!("gateway instance cleanup timed out; lease expiry will repair it")
            }
        }
    }
}

fn validate_peer_url(value: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let url = url::Url::parse(value).map_err(|error| {
        anyhow::anyhow!("invalid LIGHTSPEED_ENVIRONMENT_GATEWAY_PEER_URL: {error}")
    })?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && matches!(url.path(), "" | "/"),
        "LIGHTSPEED_ENVIRONMENT_GATEWAY_PEER_URL must be an HTTP(S) origin without credentials, path, query, or fragment"
    );
    Ok(Some(url.as_str().trim_end_matches('/').to_owned()))
}

/// Hold one session-level advisory lock for the entire loop, including
/// provider I/O. Losing that database session cancels the loop before a new
/// leadership attempt; dropping this future releases the dedicated session.
pub(crate) async fn run_as_leader<F, Fut>(pool: &PgPool, kind: store_pg::RuntimeLoop, run: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        match tokio::time::timeout(
            DATABASE_TIMEOUT,
            store_pg::RuntimeLoopLeader::try_acquire(pool, kind),
        )
        .await
        {
            Ok(Ok(Some(mut leader))) => {
                let check_leadership = async {
                    loop {
                        tokio::time::sleep(LEASE_RENEW_INTERVAL).await;
                        match tokio::time::timeout(DATABASE_TIMEOUT, leader.check()).await {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => {
                                tracing::warn!(%error, ?kind, "environment loop leadership lost");
                                return;
                            }
                            Err(_) => {
                                tracing::warn!(
                                    ?kind,
                                    "environment loop leadership check timed out"
                                );
                                return;
                            }
                        }
                    }
                };
                tokio::select! {
                    _ = run() => {}
                    _ = check_leadership => {}
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, ?kind, "environment loop leadership acquisition failed")
            }
            Err(_) => tracing::warn!(?kind, "environment loop leadership acquisition timed out"),
        }
        tokio::time::sleep(LEASE_RENEW_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_url_is_an_optional_direct_http_origin() {
        assert_eq!(validate_peer_url(None).unwrap(), None);
        assert_eq!(validate_peer_url(Some(" ")).unwrap(), None);
        assert_eq!(
            validate_peer_url(Some(" http://10.0.0.7:8080/ ")).unwrap(),
            Some("http://10.0.0.7:8080".to_owned())
        );
        assert_eq!(
            validate_peer_url(Some("https://[fd00::7]:8080")).unwrap(),
            Some("https://[fd00::7]:8080".to_owned())
        );
        for value in [
            "ws://gateway:8080",
            "http://user:secret@gateway",
            "http://gateway/peer",
            "http://gateway?x=1",
            "http://gateway#fragment",
            "not a URL",
        ] {
            assert!(validate_peer_url(Some(value)).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn draining_stops_admission_but_preserves_existing_connections() {
        let replica = GatewayReplica::local();
        assert!(replica.is_accepting());
        replica.begin_draining();
        assert!(!replica.is_accepting());
        assert!(replica.lease_valid());
    }

    #[test]
    fn expired_instance_cannot_accept_connections() {
        let replica = GatewayReplica::new(None, true, Instant::now() - INSTANCE_LEASE);
        assert!(!replica.lease_valid());
        assert!(!replica.is_accepting());
    }
}

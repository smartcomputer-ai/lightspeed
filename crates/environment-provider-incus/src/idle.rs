//! The provider's idle loop: it powers idle targets down and closes them on
//! the provider's own clock.
//!
//! Every `idleCheckSeconds` the loop visits each owned instance that has an
//! idle policy (recorded as `user.lightspeed.idle` at `target/create`) and
//! no data connection in progress. A running instance's idle duration is
//! envd's `env/getIdle` `idleForMs`, read over the guest network, so running
//! processes, jobs and leftover process groups keep it awake. A paused or
//! stopped instance has no envd to ask: the loop continues from the idle
//! duration it recorded when it powered the instance down, plus the time
//! since, kept in memory. A provider restart, or an instance powered down
//! by anyone else, starts that clock at zero.
//!
//! A data route dial that woke an instance gives it a grace of at least one
//! `idleCheckSeconds` once the dial's connection ends ([`Wakes`]). Unpausing
//! does not reset envd's clock, and a dial that only observes (the
//! readiness probe's `initialize`) is no `use`, so without the grace the
//! next pass could power the instance down again before the caller's first
//! call arrives.
//!
//! The most escalated due stage ([`IdlePolicy::due_action`]) beyond the
//! instance's current power runs as the target's background job, so it is
//! serialized with controller calls and with the wake a data route dial
//! starts. An idle close deletes the instance; `target/get` then answers
//! `notFound`, which Lightspeed records as the provider ending the target.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail};
use environment_protocol::{
    ENVD_PROTOCOL_VERSION,
    control::target::PowerState,
    data::{
        handshake::InitializeParams,
        idle::{GetIdleParams, GetIdleResponse},
    },
    idle::{IdleAction, IdlePolicy},
    jsonrpc::{self, Id, Request},
    manifest::{DataInitialize, EnvGetIdle},
};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use crate::{
    Config, IncusBackend,
    incus::{InstanceStatus, OwnedTarget, SUPPORTED_POWER_STATES},
    relay::{self, GuestSocket},
    server::{self, App, WorkKind},
};

/// Bound on one guest's whole `initialize` and `env/getIdle` exchange.
const IDLE_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The least wake grace, whatever `idleCheckSeconds`: room for a caller's
/// retries after a `waking` refusal and its first call.
const MIN_WAKE_GRACE: Duration = Duration::from_secs(30);

/// The wake grace for an idle loop that looks every `idle_check_seconds`.
pub(crate) fn wake_grace(idle_check_seconds: u64) -> Duration {
    Duration::from_secs(idle_check_seconds).max(MIN_WAKE_GRACE)
}

/// When data route dials last woke instances, by instance name. The idle
/// loop leaves an instance alone for the grace after its last wake.
#[derive(Clone, Debug)]
pub(crate) struct Wakes {
    grace: Duration,
    at: Arc<Mutex<HashMap<String, Instant>>>,
}

impl Wakes {
    pub(crate) fn new(grace: Duration) -> Self {
        Self {
            grace,
            at: Arc::default(),
        }
    }

    /// Record that a dial which woke `name` ended at `at`.
    pub(crate) fn record(&self, name: &str, at: Instant) {
        let mut wakes = self.lock();
        let entry = wakes.entry(name.to_owned()).or_insert(at);
        *entry = (*entry).max(at);
    }

    /// True while `name` is within the grace of its last wake at `now`.
    pub(crate) fn within_grace(&self, name: &str, now: Instant) -> bool {
        self.lock()
            .get(name)
            .is_some_and(|at| now.saturating_duration_since(*at) < self.grace)
    }

    /// Forget the wakes whose grace ended by `now`.
    fn prune(&self, now: Instant) {
        self.lock()
            .retain(|_, at| now.saturating_duration_since(*at) < self.grace);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Instant>> {
        self.at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Run the idle loop until the task is aborted.
pub(crate) async fn run<B: IncusBackend>(app: App<B>) {
    let interval = Duration::from_secs(app.config.idle_check_seconds);
    let mut clock = IdleClock::default();
    loop {
        tokio::time::sleep(interval).await;
        if let Err(error) = check_once(&app, &mut clock, Instant::now()).await {
            eprintln!("warning: idle check failed: {error:#}");
        }
    }
}

/// Idle durations of the instances the loop powered down, by instance name.
#[derive(Debug, Default)]
pub(crate) struct IdleClock(HashMap<String, PoweredDown>);

#[derive(Clone, Copy, Debug)]
struct PoweredDown {
    /// The idle duration when the clock started.
    idle_for_ms: u64,
    since: Instant,
}

impl IdleClock {
    /// The idle duration of a powered-down instance at `now`, starting its
    /// clock at zero when none runs.
    fn powered_down_idle(&mut self, name: &str, now: Instant) -> u64 {
        let entry = *self.0.entry(name.to_owned()).or_insert(PoweredDown {
            idle_for_ms: 0,
            since: now,
        });
        let elapsed = u64::try_from(now.saturating_duration_since(entry.since).as_millis())
            .unwrap_or(u64::MAX);
        entry.idle_for_ms.saturating_add(elapsed)
    }
}

/// One pass over every owned instance.
pub(crate) async fn check_once<B: IncusBackend>(
    app: &App<B>,
    clock: &mut IdleClock,
    now: Instant,
) -> anyhow::Result<()> {
    let targets = app.backend.list_all_owned().await?;
    app.wakes.prune(now);
    clock
        .0
        .retain(|name, _| targets.iter().any(|target| target.name() == name));
    for target in targets {
        let name = target.name().to_owned();
        let Some(policy) = target.idle.as_ref() else {
            clock.0.remove(&name);
            continue;
        };
        if app.relays.active(&name) {
            // In use; a dial wakes the instance, so no powered-down clock
            // applies any more.
            clock.0.remove(&name);
            continue;
        }
        if app.work.busy(&name) {
            continue;
        }
        if app.wakes.within_grace(&name, now) {
            // Just woken; its powered-down clock ended with the wake.
            clock.0.remove(&name);
            continue;
        }
        let (current, idle_for_ms) = match target.status {
            InstanceStatus::Running => {
                clock.0.remove(&name);
                match read_idle(&app.config, &target).await {
                    Ok(report) => (PowerState::Running, report.idle_for_ms),
                    // Booting, or envd not answering: nothing to decide on.
                    Err(_) => continue,
                }
            }
            InstanceStatus::Frozen => (PowerState::Paused, clock.powered_down_idle(&name, now)),
            InstanceStatus::Stopped => (PowerState::Stopped, clock.powered_down_idle(&name, now)),
            // Transitional, failed or unknown: look again next pass.
            _ => continue,
        };
        let Some(action) = escalation(policy, current, idle_for_ms) else {
            continue;
        };
        // A data connection may have started while envd was asked.
        if app.relays.active(&name) || app.work.busy(&name) {
            continue;
        }
        apply(app, &target, action);
        match action.power_state() {
            Some(_) => {
                clock.0.insert(
                    name,
                    PoweredDown {
                        idle_for_ms,
                        since: now,
                    },
                );
            }
            None => {
                clock.0.remove(&name);
            }
        }
    }
    Ok(())
}

/// The due stage of `policy` that takes a target beyond `current`, if any:
/// a paused target may still be stopped or closed, a stopped one only
/// closed.
fn escalation(policy: &IdlePolicy, current: PowerState, idle_for_ms: u64) -> Option<IdleAction> {
    let action = policy.due_action(idle_for_ms, &SUPPORTED_POWER_STATES)?;
    let depth = |power: PowerState| match power {
        PowerState::Running => 0,
        PowerState::Paused => 1,
        PowerState::Suspended => 2,
        PowerState::Stopped => 3,
    };
    let target_depth = action.power_state().map_or(4, depth);
    (target_depth > depth(current)).then_some(action)
}

/// Start `action` as the target's background job.
fn apply<B: IncusBackend>(app: &App<B>, target: &OwnedTarget, action: IdleAction) {
    let universe_id = target.universe();
    let name = target.name();
    match action.power_state() {
        Some(power) => app.work.start(
            name,
            WorkKind::Power(power),
            server::power_job(&app.backend, universe_id, name, power),
        ),
        None => app.work.start(
            name,
            WorkKind::Close,
            server::close_job(&app.backend, universe_id, name),
        ),
    }
}

/// envd's idle report of a running guest, over a data connection of the
/// provider's own. `initialize` and `env/getIdle` are observations, so the
/// exchange does not reset the idle clock it reads.
pub(crate) async fn read_idle(
    config: &Config,
    target: &OwnedTarget,
) -> anyhow::Result<GetIdleResponse> {
    tokio::time::timeout(IDLE_READ_TIMEOUT, async {
        let mut guest = relay::dial_guest(config, target).await?;
        let report = exchange(&mut guest).await;
        let _ = guest.close(None).await;
        report
    })
    .await
    .context("env/getIdle timed out")?
}

async fn exchange(guest: &mut GuestSocket) -> anyhow::Result<GetIdleResponse> {
    let initialized: Value = call(
        guest,
        1,
        DataInitialize::NAME,
        &InitializeParams {
            protocol_version: ENVD_PROTOCOL_VERSION,
            client_info: server::server_info(),
        },
    )
    .await
    .context("initialize")?;
    // envd refuses another version with `versionMismatch`; a matching
    // version is all the provider needs from the handshake.
    let version = initialized.get("protocolVersion").and_then(Value::as_u64);
    if version != Some(u64::from(ENVD_PROTOCOL_VERSION)) {
        bail!("envd answered initialize with protocol version {version:?}")
    }
    call(guest, 2, EnvGetIdle::NAME, &GetIdleParams::default())
        .await
        .context("env/getIdle")
}

/// Send one request and read frames until its response arrives.
async fn call<P: Serialize, R: DeserializeOwned>(
    guest: &mut GuestSocket,
    id: i64,
    method: &str,
    params: &P,
) -> anyhow::Result<R> {
    let request = Request::new(Id::Number(id), method, params)?;
    guest.send(Message::Text(request.to_text().into())).await?;
    while let Some(frame) = guest.next().await {
        let Message::Text(text) = frame? else {
            continue;
        };
        match jsonrpc::parse(text.as_str()) {
            Ok(jsonrpc::Message::Response(response)) if response.id == Some(Id::Number(id)) => {
                return Ok(response.decode()?);
            }
            // Notifications, and anything not answering this request.
            Ok(_) => {}
            Err(rejection) => bail!("envd sent an unreadable frame: {}", rejection.error),
        }
    }
    bail!("envd closed the connection before answering {method}")
}

#[cfg(test)]
mod tests {
    use environment_protocol::jsonrpc::Response;

    use super::*;
    use crate::{
        policy::{self, tests::owner},
        server::tests::{CREDENTIAL, FakeBackend, config_with},
    };

    fn policy() -> IdlePolicy {
        IdlePolicy {
            pause_after_ms: Some(1_000),
            stop_after_ms: Some(10_000),
            close_after_ms: Some(100_000),
            ..IdlePolicy::default()
        }
    }

    #[test]
    fn only_stages_beyond_the_current_power_are_due() {
        let policy = policy();
        assert_eq!(escalation(&policy, PowerState::Running, 999), None);
        assert_eq!(
            escalation(&policy, PowerState::Running, 1_000),
            Some(IdleAction::Pause)
        );
        assert_eq!(escalation(&policy, PowerState::Paused, 5_000), None);
        assert_eq!(
            escalation(&policy, PowerState::Paused, 10_000),
            Some(IdleAction::Stop)
        );
        assert_eq!(
            escalation(&policy, PowerState::Running, 10_000),
            Some(IdleAction::Stop)
        );
        assert_eq!(escalation(&policy, PowerState::Stopped, 50_000), None);
        assert_eq!(
            escalation(&policy, PowerState::Stopped, 100_000),
            Some(IdleAction::Close)
        );
        // Incus cannot suspend, so a suspend stage never fires here.
        let suspend = IdlePolicy {
            suspend_after_ms: Some(1_000),
            ..IdlePolicy::default()
        };
        assert_eq!(escalation(&suspend, PowerState::Running, 5_000), None);
    }

    #[test]
    fn the_powered_down_clock_starts_at_zero_and_advances() {
        let mut clock = IdleClock::default();
        let start = Instant::now();
        assert_eq!(clock.powered_down_idle("a", start), 0);
        assert_eq!(
            clock.powered_down_idle("a", start + Duration::from_millis(1_500)),
            1_500
        );
        clock.0.insert(
            "b".to_owned(),
            PoweredDown {
                idle_for_ms: 4_000,
                since: start,
            },
        );
        assert_eq!(
            clock.powered_down_idle("b", start + Duration::from_secs(1)),
            5_000
        );
    }

    fn app_with(port: u16) -> (App<FakeBackend>, FakeBackend) {
        let backend = FakeBackend::default();
        (
            App::new(config_with(port, Some(CREDENTIAL)), backend.clone()),
            backend,
        )
    }

    async fn settle(app: &App<FakeBackend>, name: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while app.work.busy(name) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the idle job finishes");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_powered_down_target_escalates_on_the_provider_clock_and_closes() {
        let (app, backend) = app_with(19091);
        let owner = owner("u", "e");
        let name = policy::instance_name(&owner);
        backend.insert(&owner, "small", InstanceStatus::Frozen);
        backend.update(&owner, |target| target.idle = Some(policy()));
        let mut clock = IdleClock::default();
        let start = Instant::now();

        // The first look starts the clock; nothing is due at zero.
        check_once(&app, &mut clock, start).await.unwrap();
        assert!(backend.power_calls.lock().unwrap().is_empty());

        check_once(&app, &mut clock, start + Duration::from_secs(10))
            .await
            .unwrap();
        settle(&app, &name).await;
        assert_eq!(
            backend.power_calls.lock().unwrap().as_slice(),
            [PowerState::Stopped]
        );
        assert_eq!(backend.get(&owner).unwrap().status, InstanceStatus::Stopped);

        // The clock continues from the stop, not from zero.
        check_once(&app, &mut clock, start + Duration::from_secs(100))
            .await
            .unwrap();
        settle(&app, &name).await;
        assert!(backend.get(&owner).is_none());
        assert_eq!(backend.deletes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(clock.0.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn targets_without_a_policy_or_in_use_are_left_alone() {
        let (app, backend) = app_with(19091);
        let unpoliced = owner("u", "unpoliced");
        backend.insert(&unpoliced, "small", InstanceStatus::Frozen);
        let relayed = owner("u", "relayed");
        backend.insert(&relayed, "small", InstanceStatus::Frozen);
        backend.update(&relayed, |target| target.idle = Some(policy()));
        let _hold = app.relays.hold(&policy::instance_name(&relayed));
        let mut clock = IdleClock::default();
        let start = Instant::now();
        for offset in [0, 1_000] {
            check_once(&app, &mut clock, start + Duration::from_secs(offset))
                .await
                .unwrap();
        }
        assert!(backend.power_calls.lock().unwrap().is_empty());
        assert!(backend.get(&unpoliced).is_some());
        assert!(backend.get(&relayed).is_some());
    }

    /// A guest envd stand-in that answers `initialize` and reports
    /// `idle_for_ms` from `env/getIdle`. Returns its port.
    async fn idle_envd(idle_for_ms: u64) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(Message::Text(text))) = socket.next().await {
                        let Ok(jsonrpc::Message::Request(request)) = jsonrpc::parse(text.as_str())
                        else {
                            continue;
                        };
                        let result = match request.method.as_str() {
                            DataInitialize::NAME => {
                                serde_json::json!({"protocolVersion": ENVD_PROTOCOL_VERSION})
                            }
                            EnvGetIdle::NAME => serde_json::to_value(GetIdleResponse {
                                idle_for_ms,
                                ..GetIdleResponse::default()
                            })
                            .unwrap(),
                            _ => continue,
                        };
                        let response = Response::success(request.id, &result);
                        if socket
                            .send(Message::Text(response.to_text().into()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        port
    }

    fn running(backend: &FakeBackend, environment: &str) -> crate::incus::OwnedTarget {
        let owner = owner("u", environment);
        backend.insert(&owner, "small", InstanceStatus::Running);
        backend.update(&owner, |target| {
            target.idle = Some(policy());
            target.ipv4_address = Some("127.0.0.1".to_owned());
        });
        backend.get(&owner).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_running_target_is_paused_by_its_envd_idle_report() {
        let port = idle_envd(5_000).await;
        let (app, backend) = app_with(port);
        let target = running(&backend, "e");
        assert_eq!(
            read_idle(&app.config, &target).await.unwrap().idle_for_ms,
            5_000
        );
        let mut clock = IdleClock::default();
        let now = Instant::now();
        check_once(&app, &mut clock, now).await.unwrap();
        settle(&app, target.name()).await;
        assert_eq!(
            backend.power_calls.lock().unwrap().as_slice(),
            [PowerState::Paused]
        );
        // The provider clock continues from envd's report.
        assert_eq!(
            clock.powered_down_idle(target.name(), now + Duration::from_secs(5)),
            10_000
        );
    }

    #[test]
    fn the_wake_grace_covers_at_least_one_idle_check() {
        assert_eq!(wake_grace(1), MIN_WAKE_GRACE);
        assert_eq!(wake_grace(600), Duration::from_secs(600));
        let wakes = Wakes::new(Duration::from_secs(60));
        let start = Instant::now();
        wakes.record("a", start + Duration::from_secs(10));
        // An earlier record does not shorten the grace.
        wakes.record("a", start);
        assert!(wakes.within_grace("a", start + Duration::from_secs(69)));
        assert!(!wakes.within_grace("a", start + Duration::from_secs(70)));
        assert!(!wakes.within_grace("b", start));
        wakes.prune(start + Duration::from_secs(70));
        assert!(wakes.lock().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_freshly_woken_target_is_left_alone_for_the_wake_grace() {
        // Unpausing keeps envd's idle clock from before the pause.
        let port = idle_envd(5_000).await;
        let (app, backend) = app_with(port);
        let target = running(&backend, "e");
        let woke = Instant::now();
        app.wakes.record(target.name(), woke);
        let mut clock = IdleClock::default();
        check_once(&app, &mut clock, woke).await.unwrap();
        assert!(backend.power_calls.lock().unwrap().is_empty());

        check_once(
            &app,
            &mut clock,
            woke + wake_grace(app.config.idle_check_seconds),
        )
        .await
        .unwrap();
        settle(&app, target.name()).await;
        assert_eq!(
            backend.power_calls.lock().unwrap().as_slice(),
            [PowerState::Paused]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn running_work_reported_by_envd_keeps_a_target_awake() {
        // envd holds idleForMs at zero while processes, jobs or leftover
        // process groups run.
        let port = idle_envd(0).await;
        let (app, backend) = app_with(port);
        running(&backend, "e");
        let mut clock = IdleClock::default();
        check_once(&app, &mut clock, Instant::now()).await.unwrap();
        assert!(backend.power_calls.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_running_target_without_a_listening_envd_is_left_alone() {
        let port = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let (app, backend) = app_with(port);
        running(&backend, "e");
        let mut clock = IdleClock::default();
        check_once(&app, &mut clock, Instant::now()).await.unwrap();
        assert!(backend.power_calls.lock().unwrap().is_empty());
    }
}

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use environment_client::{
    ConnectOptions, ControllerClient, DataClient,
    conformance::controller::{ControllerConformance, run},
};
use environment_protocol::{
    control::target::{
        CloseTargetParams, CreateTargetParams, GetTargetParams, PowerState, SetTargetPowerParams,
        TargetObservation, TargetOwner, TargetPhase, TargetRef,
    },
    handshake::ImplementationInfo,
    idle::IdlePolicy,
    ids::{EnvironmentId, ProviderTargetId, TemplateId, UniverseId},
    paths::EnvironmentPath,
    routes::{DataRouteBase, provider_data_route},
};
use environment_provider_incus::{
    Config, IncusBackend as _, IncusClient, ProviderArgs, policy, server,
};
use tools::environment_protocol::conformance::{self as data_conformance, DataConformance};

async fn live_config() -> Config {
    let config_path = std::env::var("LIGHTSPEED_INCUS_PROVIDER_CONFIG")
        .map(PathBuf::from)
        .expect("LIGHTSPEED_INCUS_PROVIDER_CONFIG must name a live provider config");
    ProviderArgs {
        config: config_path,
    }
    .load()
    .await
    .expect("load live provider config")
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a reachable standalone Incus server or Incus cluster"]
async fn configured_topology_and_owned_inventory_are_live() {
    let config = live_config().await;
    let backend = IncusClient::new(config.clone())
        .await
        .expect("connect and validate live Incus topology");
    let topology = backend.topology().await.expect("read live topology");
    assert_eq!(topology.mode, config.incus_mode_name());
    assert!(!topology.members.is_empty());

    let targets = backend
        .list_all_owned()
        .await
        .expect("list live owned targets");
    for target in targets {
        if topology.mode == "cluster" {
            assert!(target.location.is_some());
        }
        // Every owned instance carries its owner's deterministic name.
        assert_eq!(target.name(), policy::instance_name(&target.owner));
    }
}

/// One owner in the fixed conformance universe, so repeated runs reuse
/// one Incus project and network.
fn conformance_owner() -> TargetOwner {
    TargetOwner {
        universe_id: UniverseId::new("lightspeed-conformance").expect("universe id"),
        environment_id: EnvironmentId::new(uuid::Uuid::new_v4().to_string())
            .expect("environment id"),
    }
}

fn first_template(config: &Config) -> TemplateId {
    config
        .templates
        .values()
        .find(|template| !template.deprecated)
        .map(|template| template.template_id.clone())
        .expect("the live config offers a template")
}

/// Connect options carrying the provider credential when one is
/// configured, as Lightspeed sends it.
fn connect_options(config: &Config) -> ConnectOptions {
    let options = ConnectOptions::new(
        ImplementationInfo::new("environment-provider-incus-live").expect("client info"),
    );
    match &config.credential {
        Some(credential) => options.with_bearer_token(credential.expose().to_owned()),
        None => options,
    }
}

/// A provider on loopback against live Incus, with its controller
/// connected. Returns the controller, the data route base Lightspeed
/// derives from its URL, and the server task.
async fn start_provider(
    config: &Config,
) -> (
    ControllerClient,
    DataRouteBase,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let backend = IncusClient::new(config.clone())
        .await
        .expect("connect and validate live Incus topology");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the controller listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(server::serve_on(listener, config.clone(), backend));
    let endpoint = format!("ws://{address}/control");
    let (controller, initialized) =
        ControllerClient::connect_initialized(&endpoint, connect_options(config))
            .await
            .expect("controller handshake");
    assert!(initialized.capabilities.power);
    let base = DataRouteBase::derived_from_controller(&endpoint).expect("derived data route base");
    (controller, base, server)
}

fn by_owner(owner: &TargetOwner, target_id: Option<&ProviderTargetId>) -> TargetRef {
    TargetRef {
        owner: owner.clone(),
        target_id: target_id.cloned(),
    }
}

/// Create a target for `owner` the way the runtime does: `target/create`
/// until the observation carries a `targetId`, then `target/get` until it
/// is active.
async fn create_until_active(
    controller: &ControllerClient,
    owner: &TargetOwner,
    template_id: &TemplateId,
    idle: Option<IdlePolicy>,
    timeout: Duration,
) -> anyhow::Result<ProviderTargetId> {
    let deadline = Instant::now() + timeout;
    let params = CreateTargetParams {
        owner: owner.clone(),
        template_id: template_id.clone(),
        display_name: None,
        idle,
    };
    let mut observation = controller.create_target(&params).await?.target;
    loop {
        if observation.phase == TargetPhase::Failed {
            anyhow::bail!("the target failed: {:?}", observation.message);
        }
        if observation.phase == TargetPhase::Active
            && let Some(target_id) = observation.target_id.clone()
        {
            // The provider names the instance after its owner.
            assert_eq!(target_id.as_str(), policy::instance_name(owner));
            return Ok(target_id);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("the target did not become active within {timeout:?}: {observation:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        observation = match &observation.target_id {
            Some(target_id) => {
                controller
                    .get_target(&GetTargetParams {
                        target: by_owner(owner, Some(target_id)),
                    })
                    .await?
                    .target
            }
            None => controller.create_target(&params).await?.target,
        };
    }
}

async fn get(
    controller: &ControllerClient,
    owner: &TargetOwner,
    target_id: &ProviderTargetId,
) -> anyhow::Result<TargetObservation> {
    Ok(controller
        .get_target(&GetTargetParams {
            target: by_owner(owner, Some(target_id)),
        })
        .await?
        .target)
}

/// Dial the target's data route until envd answers, honoring the
/// provider's `waking` and `booting` refusals and their `Retry-After`, as
/// the runtime's open handshake does.
async fn dial_until_ready(
    route: &str,
    options: ConnectOptions,
    timeout: Duration,
) -> anyhow::Result<(DataClient, Vec<String>)> {
    let deadline = Instant::now() + timeout;
    let mut refusals = Vec::new();
    loop {
        match DataClient::connect_initialized(route, options.clone()).await {
            Ok((client, _)) => return Ok((client, refusals)),
            Err(error) => {
                let Some(refusal) = error.data_route_refusal() else {
                    anyhow::bail!("the data route failed without a typed refusal: {error}");
                };
                if !refusal.is_retryable() {
                    anyhow::bail!("the data route refused for good: {refusal}");
                }
                refusals.push(refusal.state().to_owned());
                if Instant::now() >= deadline {
                    anyhow::bail!("envd did not answer within {timeout:?}: {refusal}");
                }
                let wait = error.retry_after_ms().unwrap_or(2_000);
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
        }
    }
}

/// Close by owner until the provider answers `closed`.
async fn close_until_closed(
    controller: &ControllerClient,
    owner: &TargetOwner,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let observation = controller
            .close_target(&CloseTargetParams {
                target: by_owner(owner, None),
            })
            .await?
            .target;
        if observation.phase == TargetPhase::Closed {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("the target did not close within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// The tools' data conformance suite against envd inside a live Incus VM,
/// reached through this provider's data route and relay, under the data
/// route base Lightspeed derives from the controller URL, exactly as the
/// runtime reaches it. Creates one VM and closes it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a reachable standalone Incus server or Incus cluster; creates and deletes one VM"]
async fn the_guest_envd_conforms_on_live_incus() {
    let config = live_config().await;
    let template_id = first_template(&config);
    let options = connect_options(&config);
    let (controller, base, server) = start_provider(&config).await;
    let owner = conformance_owner();
    let outcome = async {
        let target_id = create_until_active(
            &controller,
            &owner,
            &template_id,
            None,
            Duration::from_secs(900),
        )
        .await?;
        let route = provider_data_route(&base, &owner, &target_id);
        let (client, _) =
            dial_until_ready(&route, options.clone(), Duration::from_secs(600)).await?;
        let suite = DataConformance::new(
            client.clone(),
            EnvironmentPath::new("/workspace").expect("work directory"),
        );
        let report = data_conformance::run(&suite).await;
        let _ = client.close().await;
        report
            .into_result()
            .map(|_| ())
            .map_err(anyhow::Error::from)
    }
    .await;
    let closed = close_until_closed(&controller, &owner, Duration::from_secs(300)).await;
    let _ = controller.close().await;
    server.abort();
    outcome.expect("the guest envd conforms");
    closed.expect("the conformance target closes");
}

/// A paused target wakes when its data route is dialed: the dial holds or
/// refuses `waking`, and then relays to envd. Creates one VM and closes it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a reachable standalone Incus server or Incus cluster; creates and deletes one VM"]
async fn a_paused_target_wakes_on_dial_on_live_incus() {
    let config = live_config().await;
    let template_id = first_template(&config);
    let options = connect_options(&config);
    let (controller, base, server) = start_provider(&config).await;
    let owner = conformance_owner();
    let outcome = async {
        let target_id = create_until_active(
            &controller,
            &owner,
            &template_id,
            None,
            Duration::from_secs(900),
        )
        .await?;
        let route = provider_data_route(&base, &owner, &target_id);
        let (client, _) =
            dial_until_ready(&route, options.clone(), Duration::from_secs(600)).await?;
        let _ = client.close().await;

        let paused = controller
            .set_target_power(&SetTargetPowerParams {
                target: by_owner(&owner, Some(&target_id)),
                power: PowerState::Paused,
            })
            .await?
            .target;
        anyhow::ensure!(
            paused.power == Some(PowerState::Paused),
            "setPower did not pause the target: {paused:?}"
        );

        let (client, refusals) =
            dial_until_ready(&route, options.clone(), Duration::from_secs(120)).await?;
        let _ = client.close().await;
        anyhow::ensure!(
            refusals
                .iter()
                .all(|state| state == "waking" || state == "booting"),
            "unexpected refusals while waking: {refusals:?}"
        );
        let woken = get(&controller, &owner, &target_id).await?;
        anyhow::ensure!(
            woken.power == Some(PowerState::Running),
            "the dial did not wake the target: {woken:?}"
        );
        Ok(())
    }
    .await;
    let closed = close_until_closed(&controller, &owner, Duration::from_secs(300)).await;
    let _ = controller.close().await;
    server.abort();
    outcome.expect("the paused target wakes on dial");
    closed.expect("the target closes");
}

/// The idle loop pauses a target whose envd reports it idle past the
/// create-time `idle` override. Takes up to a few `idleCheckSeconds`.
/// Creates one VM and closes it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a reachable standalone Incus server or Incus cluster; creates and deletes one VM"]
async fn the_idle_loop_pauses_an_idle_target_on_live_incus() {
    let config = live_config().await;
    let template_id = first_template(&config);
    let options = connect_options(&config);
    let (controller, base, server) = start_provider(&config).await;
    let owner = conformance_owner();
    let idle = IdlePolicy {
        pause_after_ms: Some(1_000),
        ..IdlePolicy::default()
    };
    let wait = Duration::from_secs(config.idle_check_seconds * 3 + 60);
    let outcome = async {
        let target_id = create_until_active(
            &controller,
            &owner,
            &template_id,
            Some(idle),
            Duration::from_secs(900),
        )
        .await?;
        // Wait until envd answers, so the idle loop can read it.
        let route = provider_data_route(&base, &owner, &target_id);
        let (client, _) =
            dial_until_ready(&route, options.clone(), Duration::from_secs(600)).await?;
        let _ = client.close().await;
        let deadline = Instant::now() + wait;
        loop {
            let observed = get(&controller, &owner, &target_id).await?;
            if observed.power == Some(PowerState::Paused) {
                return Ok(());
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "the idle loop did not pause the target within {wait:?}: {observed:?}"
            );
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    .await;
    let closed = close_until_closed(&controller, &owner, Duration::from_secs(300)).await;
    let _ = controller.close().await;
    server.abort();
    outcome.expect("the idle loop pauses the target");
    closed.expect("the target closes");
}

/// The client's controller conformance suite against this provider on live
/// Incus. It creates one VM from the first offered template and closes it;
/// every owner lives in one fixed conformance universe, so repeated runs
/// reuse one Incus project and network.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a reachable standalone Incus server or Incus cluster; creates and deletes one VM"]
async fn the_controller_conforms_on_live_incus() {
    let config = live_config().await;
    let template_id = first_template(&config);
    let backend = IncusClient::new(config.clone())
        .await
        .expect("connect and validate live Incus topology");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the controller listener");
    let address = listener.local_addr().expect("listener address");
    let options = connect_options(&config);
    let server = tokio::spawn(server::serve_on(listener, config, backend));

    let owner = Arc::new(conformance_owner);
    let mut suite = ControllerConformance::new(
        format!("ws://{address}/control"),
        options,
        template_id,
        owner,
    );
    suite.settle_timeout = Duration::from_secs(900);
    let report = run(&suite).await;
    server.abort();
    report.into_result().expect("the Incus controller conforms");
}

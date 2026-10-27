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
        CloseTargetParams, CreateTargetParams, GetTargetParams, PowerState, TargetOwner,
        TargetPhase, TargetRef,
    },
    handshake::ImplementationInfo,
    ids::{BindingId, EnvironmentId, IncarnationId, ProviderTargetId, TemplateId, UniverseId},
    paths::EnvironmentPath,
    routes::provider_data_route,
};
use environment_provider_incus::{Config, IncusBackend as _, IncusClient, ProviderArgs, server};
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
    }
}

/// One owner in the fixed conformance universe and binding.
fn conformance_owner() -> TargetOwner {
    TargetOwner {
        universe_id: UniverseId::new("lightspeed-conformance").expect("universe id"),
        binding_id: BindingId::new("incus-live").expect("binding id"),
        environment_id: EnvironmentId::new(uuid::Uuid::new_v4().to_string())
            .expect("environment id"),
        incarnation_id: IncarnationId::new(uuid::Uuid::new_v4().to_string())
            .expect("incarnation id"),
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

/// Create a target for `owner` the way the runtime does: `target/create`
/// until the observation carries a `targetId`, then `target/get` until the
/// provider's own probe reaches envd.
async fn create_until_daemon_ready(
    controller: &ControllerClient,
    owner: &TargetOwner,
    template_id: &TemplateId,
    timeout: Duration,
) -> anyhow::Result<ProviderTargetId> {
    let deadline = Instant::now() + timeout;
    let params = CreateTargetParams {
        owner: owner.clone(),
        template_id: template_id.clone(),
    };
    let mut observation = controller.create_target(&params).await?.target;
    loop {
        if observation.state.phase == TargetPhase::Failed {
            anyhow::bail!("the target failed: {:?}", observation.state.message);
        }
        if observation.state.phase == TargetPhase::Active
            && observation.state.power == Some(PowerState::Running)
            && observation.state.daemon_ready
            && let Some(target_id) = observation.target_id.clone()
        {
            return Ok(target_id);
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "envd did not come up within {timeout:?}: {:?}",
                observation.state
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        observation = match &observation.target_id {
            Some(target_id) => {
                controller
                    .get_target(&GetTargetParams {
                        target: TargetRef {
                            owner: owner.clone(),
                            target_id: Some(target_id.clone()),
                        },
                    })
                    .await?
                    .target
            }
            None => controller.create_target(&params).await?.target,
        };
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
                target: TargetRef {
                    owner: owner.clone(),
                    target_id: None,
                },
            })
            .await?
            .target;
        if observation.state.phase == TargetPhase::Closed {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("the target did not close within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// The tools' data conformance suite against envd inside a live Incus VM,
/// reached through this provider's data route and relay with the provider
/// credential, exactly as the runtime reaches it. Creates one VM and
/// closes it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a reachable standalone Incus server or Incus cluster; creates and deletes one VM"]
async fn the_guest_envd_conforms_on_live_incus() {
    let config = live_config().await;
    let template_id = first_template(&config);
    let credential = config.credential.expose().to_owned();
    let backend = IncusClient::new(config.clone())
        .await
        .expect("connect and validate live Incus topology");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the controller listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(server::serve_on(listener, config, backend));

    let options = ConnectOptions::new(
        ImplementationInfo::new("environment-provider-incus-live").expect("client info"),
    )
    .with_bearer_token(credential);
    let (controller, initialized) =
        ControllerClient::connect_initialized(&format!("ws://{address}/control"), options.clone())
            .await
            .expect("controller handshake");
    let owner = conformance_owner();
    let outcome = async {
        let target_id =
            create_until_daemon_ready(&controller, &owner, &template_id, Duration::from_secs(900))
                .await?;
        let route = provider_data_route(&initialized.routes.data, &owner, &target_id);
        let (client, _) = DataClient::connect_initialized(&route, options.clone()).await?;
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

/// The client's controller conformance suite against this provider on live
/// Incus. It creates one VM from the first offered template and closes it;
/// every owner lives in one fixed conformance universe and binding, so
/// repeated runs reuse one Incus project and network.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a reachable standalone Incus server or Incus cluster; creates and deletes one VM"]
async fn the_controller_conforms_on_live_incus() {
    let config = live_config().await;
    let template_id = first_template(&config);
    let credential = config.credential.expose().to_owned();
    let backend = IncusClient::new(config.clone())
        .await
        .expect("connect and validate live Incus topology");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the controller listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(server::serve_on(listener, config, backend));

    let options = ConnectOptions::new(
        ImplementationInfo::new("environment-provider-incus-live").expect("client info"),
    )
    .with_bearer_token(credential);
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

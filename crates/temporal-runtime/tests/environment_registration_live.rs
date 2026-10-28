//! Key-based outbound registration, end to end against the real gateway:
//! an in-process `lightspeed-envd` dials the environment gateway with a
//! registration key and becomes an open, connected registered environment
//! that serves worker routes through reverse-dialed stream sockets. Its
//! connection is written on transitions only: a dropped control connection
//! marks the row disconnected, and the worker route then refuses with a
//! typed refusal (`away` at once for an ephemeral key, a bounded
//! `disconnected` wait for a persistent one). The daemon reconnects by
//! identity alone, loses to a newer process with the same identity, keeps
//! reconnecting after its key is revoked, closes its ephemeral environment
//! at once when it says goodbye (`departed`), and is told the environment
//! closed by an in-process close at once and by a close made elsewhere at
//! the next revocation sweep. The registered sweep closes ephemeral rows
//! after their grace and persistent rows after the key's offline bound; a
//! parked daemon waits for its stop signal instead of exiting. Rotation
//! keeps the key's group, startup repair marks connected rows disconnected,
//! and a configured registration URL is an accepted signature origin.

mod support;

use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use api::{
    AgentApiService, DeploymentApiService, DeploymentUniverseCreateParams, EnvironmentCloseParams,
    EnvironmentCloseReasonView, EnvironmentIdentityModeView, EnvironmentListParams,
    EnvironmentReadParams, EnvironmentRegistrationKeyCreateParams,
    EnvironmentRegistrationKeyReadParams, EnvironmentRegistrationKeyRevokeParams,
    EnvironmentRegistrationKeyRotateParams, EnvironmentRegistrationKeyUpdateParams,
    EnvironmentSourceView, EnvironmentStateView, EnvironmentStatusView, EnvironmentView,
};
use environment_client::{ClientError, DataClient};
use environment_daemon::{
    DaemonRuntime,
    config::{DaemonConfig, OnClosed, RegistrationConfig},
    server::DaemonExit,
};
use environment_protocol::{
    ENVD_PROTOCOL_VERSION,
    bytes::{ByteChunk, SecretString},
    data::{
        exec::{
            CommandSpec, ExecInput, ExecState, ExitStatus, OutputChunk, PtySize, ReturnOn,
            StoppedBy,
        },
        fs::{ReadDirectoryParams, WriteFileParams},
        idle::GetIdleParams,
        process::{
            ProcessSignal, ReadProcessParams, ReadProcessResponse, StartProcessParams,
            TerminateProcessParams, WriteProcessParams,
        },
    },
    error::ProtocolErrorKind,
    ids::ProcessId,
    metadata::{DisplayName, Metadata},
    paths::EnvironmentPath,
    routes::{DataRouteRefusal, canonical_origin},
};
use environments::{
    BeginCloseEnvironment, EnvironmentCloseReason, EnvironmentGoneReason, EnvironmentStore as _,
    EnvironmentUse, EnvironmentWaitReason, REGISTERED_LIVENESS_STAMP_MS,
    REGISTERED_RECONNECT_WAIT_MS, StampRegisteredSeen,
};
use support::live::{LIVE_TEST_LOCK, require_storage_live_env};
use temporal_runtime::{
    DeploymentStores, GatewayAuthMode, UniverseRuntime,
    environments::gateway::{EnvironmentGatewayClientConfig, RouteKey},
    gateway::{
        DEFAULT_MAX_REQUEST_BODY_BYTES, GatewayAgentApi, GatewayDeploymentApi, GatewayRoutes,
        GatewayState, gateway_router, registration,
        request_context::{RequestContext, with_request_context as calling},
    },
};
use temporal_workflow::{DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal};
use tools::{
    environment::process::{
        ContinueProcessRequest, ProcessExecutor, ProcessRequest, ProcessSignal as ExecutorSignal,
        ProcessStatus,
    },
    environment_protocol::RemoteEnvironmentConnection,
};
use uuid::Uuid;

const POLL: Duration = Duration::from_millis(200);
const SETTLE: Duration = Duration::from_secs(30);
/// An in-process close reaches the daemon without any polling interval.
const IN_PROCESS_NOTIFY: Duration = Duration::from_secs(3);

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn registered_envd_dials_out_serves_routes_reconnects_and_is_spent_on_close()
-> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;
    let _guard = LIVE_TEST_LOCK.lock().await;

    let temporal_target =
        std::env::var("TEMPORAL_ADDRESS").unwrap_or_else(|_| DEFAULT_TEMPORAL_TARGET.to_owned());
    let namespace = std::env::var("TEMPORAL_NAMESPACE")
        .unwrap_or_else(|_| DEFAULT_TEMPORAL_NAMESPACE.to_owned());
    let client = connect_temporal(&temporal_target, &namespace).await?;
    let stores = DeploymentStores::from_env().await?;
    let suffix = Uuid::new_v4().simple().to_string();
    let universe_id = Uuid::new_v4();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let base_url = format!("http://{address}");
    let connect_url = format!("ws://{address}/environment-gateway/connect");
    // A second name for the same listener: accepted only because it is a
    // configured registration URL.
    let alias_connect_url = format!(
        "ws://localhost:{}/environment-gateway/connect",
        address.port()
    );
    let runtime = Arc::new(UniverseRuntime::new(
        client,
        format!("environment-registration-live-{suffix}"),
        Some(base_url.clone()),
        stores,
    )?);
    let operator = GatewayDeploymentApi::new(runtime.clone());
    // Direct service calls carry an explicit local request context.
    calling(
        RequestContext::local(api::AccessScope::Deployment),
        operator.create_universe(DeploymentUniverseCreateParams {
            slug: None,
            universe_id: universe_id.to_string(),
        }),
    )
    .await?;
    let caller = RequestContext::local(api::AccessScope::Universe { universe_id });
    let origins = temporal_runtime::gateway::http::registration_origins(
        Some(&base_url),
        std::slice::from_ref(&alias_connect_url),
        address,
    )?;
    assert_eq!(
        origins,
        [
            canonical_origin(&base_url)?,
            canonical_origin(&alias_connect_url)?
        ]
    );
    let state = Arc::new(
        GatewayState::multi(
            GatewayAuthMode::Single { universe_id },
            runtime.clone(),
            base_url.clone(),
        )
        .with_registration_origins(origins),
    );
    let gateway = tokio::spawn(async move {
        axum::serve(
            listener,
            gateway_router(state, DEFAULT_MAX_REQUEST_BODY_BYTES, GatewayRoutes::ALL),
        )
        .await
    });
    // A process without the environment-gateway role has no registration
    // routes at all: a misrouted daemon fails loudly with a 404 instead of
    // registering somewhere no worker can reach.
    let api_only_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let api_only_url = format!("http://{}", api_only_listener.local_addr()?);
    let api_only_state = Arc::new(GatewayState::multi(
        GatewayAuthMode::Single { universe_id },
        runtime.clone(),
        api_only_url.clone(),
    ));
    let api_only = tokio::spawn(async move {
        axum::serve(
            api_only_listener,
            gateway_router(
                api_only_state,
                DEFAULT_MAX_REQUEST_BODY_BYTES,
                GatewayRoutes {
                    api: true,
                    environment: false,
                },
            ),
        )
        .await
    });
    let http = reqwest::Client::new();
    for path in ["/environment-gateway/connect", "/environment-gateway/data"] {
        let status = http
            .get(format!("{api_only_url}{path}"))
            .send()
            .await?
            .status();
        assert_eq!(
            status,
            reqwest::StatusCode::NOT_FOUND,
            "{path} on an api-only process"
        );
    }
    assert_eq!(
        http.get(format!("{api_only_url}/health"))
            .send()
            .await?
            .status(),
        reqwest::StatusCode::OK
    );
    assert_ne!(
        http.get(format!("{base_url}/environment-gateway/connect"))
            .send()
            .await?
            .status(),
        reqwest::StatusCode::NOT_FOUND,
        "the environment-gateway process serves the connect route"
    );
    api_only.abort();
    let api = runtime.state_for(universe_id, false).await?.api.clone();
    let sandbox = tempfile::tempdir()?;

    let urls = Urls {
        base: &base_url,
        connect: &connect_url,
        alias_connect: &alias_connect_url,
    };
    let result = calling(
        caller,
        scenario(&runtime, &api, universe_id, &urls, sandbox.path()),
    )
    .await;
    gateway.abort();
    result
}

struct Urls<'a> {
    base: &'a str,
    connect: &'a str,
    alias_connect: &'a str,
}

async fn scenario(
    runtime: &Arc<UniverseRuntime>,
    api: &Arc<GatewayAgentApi>,
    universe_id: Uuid,
    urls: &Urls<'_>,
    sandbox: &Path,
) -> anyhow::Result<()> {
    // Workers reach this test's gateway, not whatever the sourced dev
    // environment points LIGHTSPEED_ENVIRONMENT_GATEWAY_URL at; the route
    // bearer is the deployment token the gateway state checks.
    let gateway = EnvironmentGatewayClientConfig::new(
        urls.base,
        runtime.environment_gateway().deployment_token(),
    );
    ephemeral_scenario(runtime, api, universe_id, &gateway, urls, sandbox).await?;
    persistent_scenario(runtime, api, universe_id, &gateway, urls, sandbox).await?;
    ephemeral_grace_scenario(runtime, api, universe_id, urls, sandbox).await
}

/// An ephemeral key: admission, the data path, the coarse stamp, immediate
/// `away` while disconnected, reconnect, supersede, revocation, departure
/// and the in-process close notify.
async fn ephemeral_scenario(
    runtime: &Arc<UniverseRuntime>,
    api: &Arc<GatewayAgentApi>,
    universe_id: Uuid,
    gateway: &EnvironmentGatewayClientConfig,
    urls: &Urls<'_>,
    sandbox: &Path,
) -> anyhow::Result<()> {
    let store = runtime.state_for(universe_id, false).await?.store.clone();
    let minted = api
        .create_environment_registration_key(EnvironmentRegistrationKeyCreateParams {
            display_name: "registration live pool".to_owned(),
            identity_mode: EnvironmentIdentityModeView::Ephemeral,
            max_active_environments: Some(2),
            ephemeral_disconnect_grace_ms: None,
            offline_close_after_ms: None,
            expires_at_ms: None,
        })
        .await?
        .result;
    let key_id = minted.registration_key.registration_key_id.clone();
    let secret = minted.secret.0.clone();
    assert!(secret.starts_with("lsrk_"));
    assert_eq!(minted.registration_key.active_environment_count, 0);

    // First daemon: a fresh identity admitted by the key, open and
    // connected, with both connection stamps written at admission.
    let root_a = workspace(sandbox, "a")?;
    let receipt_a = sandbox.join("receipt-a.json");
    let daemon_a = spawn_daemon(daemon_config(
        &root_a,
        &sandbox.join("state-a"),
        urls.connect,
        Some(&secret),
        Some(&receipt_a),
        OnClosed::Exit,
    )?)?;
    let environment_a = wait_for_registered(api, &key_id, 1, is_connected).await?;
    let registered = registered_source(&environment_a)?;
    assert_eq!(registered.registration_key_id, key_id);
    assert_eq!(
        registered.identity_mode,
        EnvironmentIdentityModeView::Ephemeral
    );
    assert!(registered.daemon_id.starts_with("daemon_"));
    assert_eq!(
        registered.connection_changed_at_ms,
        registered.last_seen_at_ms
    );
    assert_eq!(environment_a.state, EnvironmentStateView::Open);
    assert_eq!(environment_a.status, EnvironmentStatusView::Ready);
    assert_eq!(environment_a.request_id, None);
    assert_eq!(
        environment_a
            .metadata
            .get("harbor.trialId")
            .map(String::as_str),
        Some("trial-1")
    );
    assert!(
        environment_a
            .metadata
            .keys()
            .all(|key| !key.starts_with("lightspeed.")),
        "build facts are not metadata: {:?}",
        environment_a.metadata
    );
    let daemon = environment_a.daemon.as_ref().expect("daemon build");
    assert_eq!(daemon.version.as_deref(), Some(release_info::VERSION));
    assert_eq!(daemon.git_sha.as_deref(), Some(release_info::GIT_SHA));
    assert_eq!(daemon.protocol_version, ENVD_PROTOCOL_VERSION);
    let receipt = read_receipt(&receipt_a)?;
    assert_eq!(receipt["environmentId"], environment_a.environment_id);
    assert_eq!(receipt["daemonId"], registered.daemon_id.as_str());
    assert_eq!(receipt["identityMode"], "ephemeral");
    assert_eq!(receipt["admission"], "created");
    assert!(receipt.get("incarnationId").is_none());
    assert!(
        receipt["controlConnectionId"]
            .as_str()
            .is_some_and(|id| id.starts_with("connection_"))
    );
    assert!(receipt.get("registrationKey").is_none());
    let key_view = read_key(api, &key_id).await?;
    assert_eq!(key_view.active_environment_count, 1);
    assert_eq!(key_view.registered_environment_count, 1);

    // The liveness stamp is coarse: a stamp within the interval of the
    // last one writes nothing, one a full interval later writes.
    let environment_id_a = environments::EnvironmentId::new(environment_a.environment_id.clone());
    assert!(
        !store
            .stamp_registered_seen(StampRegisteredSeen {
                environment_id: environment_id_a.clone(),
                at_ms: registered.last_seen_at_ms + 1_000,
            })
            .await?
    );
    assert!(
        store
            .stamp_registered_seen(StampRegisteredSeen {
                environment_id: environment_id_a.clone(),
                at_ms: registered.last_seen_at_ms + REGISTERED_LIVENESS_STAMP_MS,
            })
            .await?
    );

    // The worker route reaches the daemon through a reverse-dialed stream
    // socket; concurrent routes each get their own socket.
    let key = RouteKey {
        universe_id,
        environment_id: environment_a.environment_id.clone(),
    };
    assert_eq!(
        gateway.route_key(
            universe_id,
            &store.read_environment(&environment_id_a).await?
        ),
        key
    );
    let options = || gateway.connect_options("registration-live");
    let (first, initialized) = gateway.connect(&key, options()).await?;
    let (second, _) = gateway.connect(&key, options()).await?;
    let (third, _) = gateway.connect(&key, options()).await?;
    assert_eq!(initialized.protocol_version, ENVD_PROTOCOL_VERSION);
    assert_eq!(
        initialized.default_cwd.as_str(),
        root_a.to_str().expect("utf-8 root")
    );
    let written = path(&root_a, "registered.txt")?;
    first
        .write_file(&WriteFileParams {
            path: written.clone(),
            data: ByteChunk::new(b"through the reverse dial".to_vec()),
            create: Default::default(),
            if_match: None,
            executable: false,
        })
        .await?;
    let listing = second
        .read_directory(&ReadDirectoryParams {
            path: initialized.default_cwd.clone(),
            limit: 100,
            cursor: None,
        })
        .await?;
    assert!(
        listing
            .entries
            .iter()
            .any(|entry| entry.name.as_str() == "registered.txt"),
        "second data socket sees the file the first wrote: {listing:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root_a.join("registered.txt"))?,
        "through the reverse dial"
    );
    process_scenario(&first, &second, &root_a).await?;
    executor_scenario(third).await?;
    first.close().await?;
    second.close().await?;

    // Dropping the control connection marks the environment disconnected.
    // An ephemeral daemon is away at once: the gate refuses the route for
    // good instead of waiting.
    daemon_a.exit.abort();
    let away = wait_for_environment(api, &environment_a.environment_id, "disconnected", |view| {
        is_connected(view) == Some(false)
    })
    .await?;
    assert_eq!(away.state, EnvironmentStateView::Open);
    assert_eq!(away.status, EnvironmentStatusView::Away);
    assert!(
        registered_source(&away)?.connection_changed_at_ms >= registered.connection_changed_at_ms
    );
    let refused = expect_route_refusal(gateway, &key).await?;
    assert!(matches!(
        refused,
        ClientError::UpgradeRejected {
            status: 410,
            refusal: Some(DataRouteRefusal::Away {}),
            retry_after_ms: None,
        }
    ));

    // The same identity, now without any key, reconnects the same
    // environment.
    let daemon_a = spawn_daemon(daemon_config(
        &root_a,
        &sandbox.join("state-a"),
        urls.connect,
        None,
        Some(&receipt_a),
        OnClosed::Exit,
    )?)?;
    wait_for_environment(api, &environment_a.environment_id, "reconnected", |view| {
        is_connected(view) == Some(true)
    })
    .await?;
    wait_for_receipt_admission(&receipt_a, "reconnected").await?;
    assert_eq!(
        read_receipt(&receipt_a)?["environmentId"],
        environment_a.environment_id
    );
    assert_eq!(
        wait_for_registered(api, &key_id, 1, is_connected)
            .await?
            .environment_id,
        environment_a.environment_id,
        "reconnect must not create a second environment"
    );

    // A second process with the same identity (a cloned state directory)
    // wins; the older one is told it was superseded and exits instead of
    // fighting for the identity. The row stays connected throughout.
    let twin_state = sandbox.join("state-a-twin");
    std::fs::create_dir_all(&twin_state)?;
    std::fs::copy(
        sandbox
            .join("state-a")
            .join(environment_daemon::identity::DAEMON_KEY_FILE),
        twin_state.join(environment_daemon::identity::DAEMON_KEY_FILE),
    )?;
    let twin_a = spawn_daemon(daemon_config(
        &root_a,
        &twin_state,
        urls.connect,
        None,
        None,
        OnClosed::Exit,
    )?)?;
    let superseded = tokio::time::timeout(SETTLE, daemon_a.exit).await??;
    assert!(
        matches!(superseded, DaemonExit::Superseded { .. }),
        "unexpected exit: {superseded:?}"
    );
    assert_eq!(superseded.status(), 5);
    wait_for_environment(
        api,
        &environment_a.environment_id,
        "connected twin",
        |view| is_connected(view) == Some(true),
    )
    .await?;
    let (after_supersede, _) = gateway.connect(&key, options()).await?;
    after_supersede.get_idle(&GetIdleParams {}).await?;
    after_supersede.close().await?;

    // A second identity with the same key is a second environment.
    let root_b = workspace(sandbox, "b")?;
    let daemon_b = spawn_daemon(daemon_config(
        &root_b,
        &sandbox.join("state-b"),
        urls.connect,
        Some(&secret),
        None,
        OnClosed::Exit,
    )?)?;
    let environments = wait_for_registered_count(api, &key_id, 2).await?;
    let environment_b = environments
        .iter()
        .find(|environment| environment.environment_id != environment_a.environment_id)
        .cloned()
        .expect("second environment");
    assert_eq!(is_connected(&environment_b), Some(true));

    // Revocation without cascade stops new identities but not reconnects.
    api.revoke_environment_registration_key(EnvironmentRegistrationKeyRevokeParams {
        registration_key_id: key_id.clone(),
        close_environments: false,
    })
    .await?;
    let root_c = workspace(sandbox, "c")?;
    let daemon_c = spawn_daemon(daemon_config(
        &root_c,
        &sandbox.join("state-c"),
        urls.connect,
        Some(&secret),
        None,
        OnClosed::Exit,
    )?)?;
    let refused = tokio::time::timeout(SETTLE, daemon_c.exit).await??;
    assert!(
        matches!(refused, DaemonExit::Refused(_)),
        "a revoked key is a terminal refusal for a new daemon: {refused:?}"
    );
    assert_eq!(refused.status(), 3);

    // A deliberate stop says goodbye and the daemon exits cleanly. The
    // sandbox is finished, so its ephemeral environment closes at once
    // (`departed`) instead of waiting out the key's disconnect grace.
    daemon_b.runtime.shutdown().cancel();
    let stopped = tokio::time::timeout(SETTLE, daemon_b.exit).await??;
    assert!(
        matches!(stopped, DaemonExit::Shutdown),
        "unexpected exit: {stopped:?}"
    );
    assert_eq!(stopped.status(), 0);
    let departed = wait_for_state(
        api,
        &environment_b.environment_id,
        EnvironmentStateView::Closed,
    )
    .await?;
    assert_eq!(
        departed.close_reason,
        Some(EnvironmentCloseReasonView::Departed)
    );
    assert!(departed.closed_at_ms.is_some());
    // The departed identity is spent: a restart is refused at register.
    let restarted_b = spawn_daemon(daemon_config(
        &root_b,
        &sandbox.join("state-b"),
        urls.connect,
        None,
        None,
        OnClosed::Exit,
    )?)?;
    let refused_b = tokio::time::timeout(SETTLE, restarted_b.exit).await??;
    assert!(
        matches!(refused_b, DaemonExit::EnvironmentClosed),
        "a departed ephemeral identity cannot reconnect: {refused_b:?}"
    );
    assert_eq!(refused_b.status(), 4);

    // Closing the environment in this process notifies its control
    // connection at once: the daemon is told the environment closed and
    // exits for good, and its routes end.
    api.close_environment(EnvironmentCloseParams {
        environment_id: environment_a.environment_id.clone(),
    })
    .await?;
    let spent = tokio::time::timeout(IN_PROCESS_NOTIFY, twin_a.exit).await??;
    assert!(
        matches!(spent, DaemonExit::EnvironmentClosed),
        "a closed environment ends its daemon: {spent:?}"
    );
    assert_eq!(spent.status(), 4);
    let closed = wait_for_state(
        api,
        &environment_a.environment_id,
        EnvironmentStateView::Closed,
    )
    .await?;
    assert_eq!(
        closed.close_reason,
        Some(EnvironmentCloseReasonView::Requested)
    );
    let refused = expect_route_refusal(gateway, &key).await?;
    assert!(matches!(
        refused,
        ClientError::UpgradeRejected {
            status: 410,
            refusal: Some(DataRouteRefusal::Gone {}),
            ..
        }
    ));
    assert!(
        list_by_key(api, &key_id, false).await?.is_empty(),
        "closed environments are hidden by default"
    );
    assert_eq!(list_by_key(api, &key_id, true).await?.len(), 2);
    let key_view = read_key(api, &key_id).await?;
    assert_eq!(key_view.registered_environment_count, 2);
    assert_eq!(key_view.active_environment_count, 0);
    assert_eq!(
        key_view.status,
        api::EnvironmentRegistrationKeyStatusView::Revoked
    );
    Ok(())
}

/// A persistent key, dialed under the configured registration URL: the
/// bounded reconnect wait, startup repair, rotation, the cross-process
/// close at the revocation sweep, and the offline bound with a parked
/// daemon.
async fn persistent_scenario(
    runtime: &Arc<UniverseRuntime>,
    api: &Arc<GatewayAgentApi>,
    universe_id: Uuid,
    gateway: &EnvironmentGatewayClientConfig,
    urls: &Urls<'_>,
    sandbox: &Path,
) -> anyhow::Result<()> {
    let store = runtime.state_for(universe_id, false).await?.store.clone();
    let minted = api
        .create_environment_registration_key(EnvironmentRegistrationKeyCreateParams {
            display_name: "registration live fleet".to_owned(),
            identity_mode: EnvironmentIdentityModeView::Persistent,
            max_active_environments: Some(3),
            ephemeral_disconnect_grace_ms: None,
            offline_close_after_ms: None,
            expires_at_ms: None,
        })
        .await?
        .result;
    let key_id = minted.registration_key.registration_key_id.clone();
    let first_secret = minted.secret.0.clone();

    // The daemon signs the alias origin; the gateway accepts it because
    // the alias is a configured registration URL.
    let root_p = workspace(sandbox, "p")?;
    let state_p = sandbox.join("state-p");
    let receipt_p = sandbox.join("receipt-p.json");
    let spawn_p = |key: Option<&str>| {
        spawn_daemon(daemon_config(
            &root_p,
            &state_p,
            urls.alias_connect,
            key,
            Some(&receipt_p),
            OnClosed::Exit,
        )?)
    };
    let daemon_p = spawn_p(Some(&first_secret))?;
    let environment_p = wait_for_registered(api, &key_id, 1, is_connected).await?;
    assert_eq!(read_receipt(&receipt_p)?["identityMode"], "persistent");
    let key = RouteKey {
        universe_id,
        environment_id: environment_p.environment_id.clone(),
    };
    let environment_id_p = environments::EnvironmentId::new(environment_p.environment_id.clone());
    let (client, _) = gateway
        .connect(&key, gateway.connect_options("registration-live"))
        .await?;
    client.get_idle(&GetIdleParams {}).await?;
    client.close().await?;

    // A persistent daemon that drops is waited for, within a bound: the
    // route refuses `disconnected` with a retry hint, and the gate turns
    // into `away` once the bound passes.
    daemon_p.exit.abort();
    let away = wait_for_environment(api, &environment_p.environment_id, "disconnected", |view| {
        is_connected(view) == Some(false)
    })
    .await?;
    assert_eq!(away.status, EnvironmentStatusView::Away);
    let refused = expect_route_refusal(gateway, &key).await?;
    assert!(matches!(
        refused,
        ClientError::UpgradeRejected {
            status: 503,
            refusal: Some(DataRouteRefusal::Disconnected {}),
            retry_after_ms: Some(_),
        }
    ));
    let record = store.read_environment(&environment_id_p).await?;
    let since = record
        .registered()
        .expect("registered")
        .connection_changed_at_ms;
    assert_eq!(
        record.use_gate(since + 1_000),
        EnvironmentUse::Wait {
            reason: EnvironmentWaitReason::Disconnected,
            retry_after_ms: environments::WAIT_RETRY_AFTER_MS,
            until_ms: Some(since + REGISTERED_RECONNECT_WAIT_MS),
        }
    );
    assert!(matches!(
        record.use_gate(since + REGISTERED_RECONNECT_WAIT_MS),
        EnvironmentUse::Gone {
            reason: EnvironmentGoneReason::Away,
            ..
        }
    ));

    // Back by identity alone.
    let daemon_p = spawn_p(None)?;
    wait_for_environment(api, &environment_p.environment_id, "reconnected", |view| {
        is_connected(view) == Some(true)
    })
    .await?;
    wait_for_receipt_admission(&receipt_p, "reconnected").await?;

    // Startup repair (here for this universe only): a gateway that
    // restarted held none of the connections its rows still call
    // connected. The disconnect counts from now, so persistent rows get
    // their full reconnect wait.
    let before_repair = now_ms();
    assert!(store.repair_registered_connections(before_repair).await? >= 1);
    let repaired = store.read_environment(&environment_id_p).await?;
    let source = repaired.registered().expect("registered");
    assert!(!source.connected);
    assert_eq!(source.connection_changed_at_ms, before_repair);
    assert!(matches!(
        repaired.use_gate(before_repair + 1_000),
        EnvironmentUse::Wait {
            reason: EnvironmentWaitReason::Disconnected,
            ..
        }
    ));
    // The daemon's next connection records it again.
    daemon_p.exit.abort();
    let daemon_p = spawn_p(None)?;
    wait_for_environment(
        api,
        &environment_p.environment_id,
        "reconnected after repair",
        |view| is_connected(view) == Some(true),
    )
    .await?;

    // Rotation retires the live secret at once and keeps the key: its
    // group, identity mode and environments. Admitted daemons are
    // unaffected; a new daemon needs the new secret.
    let rotated = api
        .rotate_environment_registration_key(EnvironmentRegistrationKeyRotateParams {
            registration_key_id: key_id.clone(),
        })
        .await?
        .result;
    assert_eq!(rotated.registration_key.registration_key_id, key_id);
    assert_eq!(
        rotated.registration_key.identity_mode,
        EnvironmentIdentityModeView::Persistent
    );
    assert_ne!(
        rotated.registration_key.key_prefix,
        minted.registration_key.key_prefix
    );
    assert_ne!(rotated.secret.0, first_secret);
    let root_r = workspace(sandbox, "r")?;
    let stale = spawn_daemon(daemon_config(
        &root_r,
        &sandbox.join("state-r"),
        urls.connect,
        Some(&first_secret),
        None,
        OnClosed::Exit,
    )?)?;
    let stale_exit = tokio::time::timeout(SETTLE, stale.exit).await??;
    assert!(
        matches!(stale_exit, DaemonExit::Refused(_)),
        "a retired secret admits nobody: {stale_exit:?}"
    );
    let root_s = workspace(sandbox, "s")?;
    let state_s = sandbox.join("state-s");
    let daemon_s = spawn_daemon(daemon_config(
        &root_s,
        &state_s,
        urls.connect,
        Some(&rotated.secret.0),
        None,
        OnClosed::Exit,
    )?)?;
    let environments = wait_for_registered_count(api, &key_id, 2).await?;
    let environment_s = environments
        .iter()
        .find(|environment| environment.environment_id != environment_p.environment_id)
        .cloned()
        .expect("environment admitted by the rotated secret");
    assert_eq!(
        registered_source(&environment_s)?.registration_key_id,
        key_id
    );
    assert_eq!(
        read_key(api, &key_id).await?.registered_environment_count,
        2
    );

    // A close made outside this process (here written to the store
    // directly) reaches the daemon at the next revocation sweep.
    store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: environment_id_p.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: now_ms(),
        })
        .await?;
    assert!(registration::revoke_ended_routes(runtime).await? >= 1);
    let spent = tokio::time::timeout(SETTLE, daemon_p.exit).await??;
    assert!(
        matches!(spent, DaemonExit::EnvironmentClosed),
        "the revocation sweep ends the daemon of an environment closed elsewhere: {spent:?}"
    );

    // The key's offline bound closes a persistent environment whose daemon
    // stays away (`offline_timeout`). The daemon, configured to park,
    // then waits for its stop signal instead of exiting or crash-looping.
    let updated = api
        .update_environment_registration_key(EnvironmentRegistrationKeyUpdateParams {
            registration_key_id: key_id.clone(),
            display_name: "registration live fleet (offline bound)".to_owned(),
            max_active_environments: Some(3),
            ephemeral_disconnect_grace_ms: None,
            offline_close_after_ms: Some(1_000),
            expires_at_ms: None,
        })
        .await?
        .result;
    assert_eq!(updated.registration_key.offline_close_after_ms, Some(1_000));
    assert_eq!(
        updated.registration_key.display_name,
        "registration live fleet (offline bound)"
    );
    daemon_s.exit.abort();
    wait_for_environment(api, &environment_s.environment_id, "disconnected", |view| {
        is_connected(view) == Some(false)
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let sweep = registration::sweep_registered_universe(runtime, universe_id, now_ms()).await?;
    assert!(
        sweep
            .closed
            .iter()
            .any(|(id, reason)| id.as_str() == environment_s.environment_id
                && *reason == EnvironmentCloseReason::OfflineTimeout),
        "{sweep:?}"
    );
    let closed = wait_for_state(
        api,
        &environment_s.environment_id,
        EnvironmentStateView::Closed,
    )
    .await?;
    assert_eq!(
        closed.close_reason,
        Some(EnvironmentCloseReasonView::OfflineTimeout)
    );
    let parked = spawn_daemon(daemon_config(
        &root_s,
        &state_s,
        urls.connect,
        None,
        None,
        OnClosed::Park,
    )?)?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !parked.exit.is_finished(),
        "a parked daemon waits for its stop signal"
    );
    parked.runtime.shutdown().cancel();
    let stopped = tokio::time::timeout(SETTLE, parked.exit).await??;
    assert!(
        matches!(stopped, DaemonExit::Shutdown),
        "a parked daemon exits cleanly when stopped: {stopped:?}"
    );
    assert_eq!(stopped.status(), 0);
    Ok(())
}

/// Ephemeral grace: the registered sweep closes an environment whose
/// ephemeral daemon stays away longer than its key's grace.
async fn ephemeral_grace_scenario(
    runtime: &Arc<UniverseRuntime>,
    api: &Arc<GatewayAgentApi>,
    universe_id: Uuid,
    urls: &Urls<'_>,
    sandbox: &Path,
) -> anyhow::Result<()> {
    let short = api
        .create_environment_registration_key(EnvironmentRegistrationKeyCreateParams {
            display_name: "registration live short grace".to_owned(),
            identity_mode: EnvironmentIdentityModeView::Ephemeral,
            max_active_environments: None,
            ephemeral_disconnect_grace_ms: Some(1_500),
            offline_close_after_ms: None,
            expires_at_ms: None,
        })
        .await?
        .result;
    let short_key_id = short.registration_key.registration_key_id.clone();
    let root_d = workspace(sandbox, "d")?;
    let daemon_d = spawn_daemon(daemon_config(
        &root_d,
        &sandbox.join("state-d"),
        urls.connect,
        Some(&short.secret.0),
        None,
        OnClosed::Exit,
    )?)?;
    let environment_d = wait_for_registered(api, &short_key_id, 1, is_connected).await?;
    daemon_d.exit.abort();
    wait_for_environment(api, &environment_d.environment_id, "disconnected", |view| {
        is_connected(view) == Some(false)
    })
    .await?;
    let early = registration::sweep_registered_universe(runtime, universe_id, now_ms()).await?;
    assert!(
        early
            .closed
            .iter()
            .all(|(id, _)| id.as_str() != environment_d.environment_id),
        "the grace has not passed yet: {early:?}"
    );
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    let sweep = registration::sweep_registered_universe(runtime, universe_id, now_ms()).await?;
    assert!(
        sweep
            .closed
            .iter()
            .any(|(id, reason)| id.as_str() == environment_d.environment_id
                && *reason == EnvironmentCloseReason::EphemeralGrace),
        "{sweep:?}"
    );
    let closed = wait_for_state(
        api,
        &environment_d.environment_id,
        EnvironmentStateView::Closed,
    )
    .await?;
    assert_eq!(
        closed.close_reason,
        Some(EnvironmentCloseReasonView::EphemeralGrace)
    );
    let short_view = read_key(api, &short_key_id).await?;
    assert_eq!(short_view.active_environment_count, 0);
    assert_eq!(short_view.registered_environment_count, 1);
    Ok(())
}

/// The process path through the same reverse-dialed route, on two sockets
/// at once: a normal exit leaves its background child running, reported
/// and counted by the idle report and stoppable through the handle; the
/// read cursor is the daemon's, so the second socket continues where the
/// first stopped while positioned reads consume nothing; a PTY takes input;
/// an interrupt reaches the group; and terminate escalates gracefully.
async fn process_scenario(
    first: &DataClient,
    second: &DataClient,
    root: &Path,
) -> anyhow::Result<()> {
    // A service the command left behind survives its exit.
    first
        .start_process(&shell_process(
            "live-leftover",
            "nohup sleep 60 >/dev/null 2>&1 & echo $! > leftover.pid; echo started; exit 0",
            false,
        )?)
        .await?;
    let leftover_pid = wait_for_pid_file(&root.join("leftover.pid")).await?;
    let read = first
        .read_process(&read_until_exit("live-leftover")?)
        .await?;
    assert_eq!(read.state, ExecState::Exited);
    assert_eq!(read.exit, Some(ExitStatus::Code { code: 0 }));
    assert!(read.pid > 0, "the response names the root pid");
    assert_eq!(page_text(&read.output.chunks), "started\n");
    assert!(
        read.leftovers.as_ref().is_some_and(|leftovers| leftovers
            .processes
            .iter()
            .any(|member| i32::try_from(member.pid) == Ok(leftover_pid))),
        "the leftover is reported with its pid: {:?}",
        read.leftovers
    );
    assert!(
        process_alive(leftover_pid),
        "the leftover survives the call that started it"
    );
    let idle = second.get_idle(&GetIdleParams {}).await?;
    assert_eq!(idle.running_process_count, 0);
    assert_eq!(idle.leftover_process_group_count, 1);
    second
        .terminate_process(&TerminateProcessParams {
            process_id: process_id("live-leftover")?,
            signal: ProcessSignal::Kill,
        })
        .await?;
    wait_for_process_gone(leftover_pid).await?;
    assert_eq!(
        first
            .get_idle(&GetIdleParams {})
            .await?
            .leftover_process_group_count,
        0
    );

    // The daemon owns the cursor: the second socket reads only what the
    // first did not, a positioned read consumes nothing, and writing to a
    // process without input is refused.
    first
        .start_process(&shell_process(
            "live-cursor",
            "echo one; sleep 0.4; echo two; sleep 0.4; echo three",
            false,
        )?)
        .await?;
    let head = first
        .read_process(&ReadProcessParams {
            return_on: ReturnOn::AnyOutput,
            wait_ms: 10_000,
            ..read_until_exit("live-cursor")?
        })
        .await?;
    assert_eq!(head.state, ExecState::Running);
    assert_eq!(page_text(&head.output.chunks), "one\n");
    let rest = second
        .read_process(&read_until_exit("live-cursor")?)
        .await?;
    assert_eq!(rest.state, ExecState::Exited);
    assert_eq!(page_text(&rest.output.chunks), "two\nthree\n");
    assert!(
        rest.output
            .chunks
            .iter()
            .all(|chunk| chunk.offset >= head.output.next)
    );
    let whole = first
        .read_process(&ReadProcessParams {
            after: Some(0),
            ..read_until_exit("live-cursor")?
        })
        .await?;
    assert_eq!(page_text(&whole.output.chunks), "one\ntwo\nthree\n");
    assert_eq!(whole.output.next, whole.output.end);
    let tail = second
        .read_process(&ReadProcessParams {
            tail_bytes: Some(6),
            ..read_until_exit("live-cursor")?
        })
        .await?;
    assert_eq!(page_text(&tail.output.chunks), "three\n");
    let refused = first
        .write_process(&WriteProcessParams {
            process_id: process_id("live-cursor")?,
            data: ByteChunk::new(b"late".to_vec()),
            close_input: false,
        })
        .await
        .expect_err("an exited process takes no input");
    assert_eq!(
        refused.protocol_kind(),
        Some(ProtocolErrorKind::FailedPrecondition)
    );

    // A PTY takes input written from another socket.
    first
        .start_process(&shell_process(
            "live-pty",
            "read line; echo got:$line",
            true,
        )?)
        .await?;
    let write = second
        .write_process(&WriteProcessParams {
            process_id: process_id("live-pty")?,
            data: ByteChunk::new(b"hello\n".to_vec()),
            close_input: false,
        })
        .await?;
    assert_eq!(write.accepted_bytes, 6);
    let pty = first.read_process(&read_until_exit("live-pty")?).await?;
    assert_eq!(pty.state, ExecState::Exited);
    assert!(
        page_text(&pty.output.chunks).contains("got:hello"),
        "{:?}",
        page_text(&pty.output.chunks)
    );

    // An interrupt reaches the group; the next read observes the trap.
    second
        .start_process(&shell_process(
            "live-interrupt",
            "trap 'echo caught; exit 3' INT; sleep 100 & wait $!",
            false,
        )?)
        .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let signalled = first
        .terminate_process(&TerminateProcessParams {
            process_id: process_id("live-interrupt")?,
            signal: ProcessSignal::Interrupt,
        })
        .await?;
    assert!(signalled.running);
    let caught = second
        .read_process(&read_until_exit("live-interrupt")?)
        .await?;
    assert_exited_with(&caught, 3, None, "caught\n");

    // Terminate is graceful first: the process may clean up on SIGTERM.
    first
        .start_process(&shell_process(
            "live-terminate",
            "trap 'echo stopping; exit 7' TERM; sleep 100 & wait $!",
            false,
        )?)
        .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    second
        .terminate_process(&TerminateProcessParams {
            process_id: process_id("live-terminate")?,
            signal: ProcessSignal::Terminate,
        })
        .await?;
    let terminated = first
        .read_process(&read_until_exit("live-terminate")?)
        .await?;
    assert_exited_with(&terminated, 7, Some(StoppedBy::Terminate), "stopping\n");
    Ok(())
}

fn assert_exited_with(
    read: &ReadProcessResponse,
    code: i32,
    stopped_by: Option<StoppedBy>,
    output: &str,
) {
    assert_eq!(read.state, ExecState::Exited);
    assert_eq!(read.exit, Some(ExitStatus::Code { code }));
    assert_eq!(read.stopped_by, stopped_by);
    assert_eq!(page_text(&read.output.chunks), output);
}

/// The substrate executor over the real transport: a yielded run returns
/// a handle and pid, a continue reads only new output, and a kill through
/// the handle ends the group.
async fn executor_scenario(client: DataClient) -> anyhow::Result<()> {
    let connection = RemoteEnvironmentConnection::new(client);
    let executor = connection
        .process_executor()
        .expect("the daemon serves the process family");
    let mut request = ProcessRequest::argv(["/bin/sh", "-c", "echo hi; sleep 60"]);
    request.yield_ms = Some(300);
    request.max_output_bytes = Some(4096);
    let running = executor.run_process(request).await?;
    assert_eq!(running.status, ProcessStatus::Running);
    assert!(running.pid.is_some());
    assert_eq!(running.stdout.bytes, b"hi\n");
    let handle = running.handle.expect("a running process has a handle");

    let more = executor
        .continue_process(ContinueProcessRequest::wait(handle.clone(), Some(200)))
        .await?;
    assert_eq!(more.status, ProcessStatus::Running);
    assert!(
        more.stdout.bytes.is_empty(),
        "delivered output is never repeated: {:?}",
        more.stdout.text_lossy()
    );

    let root_pid = i32::try_from(running.pid.expect("pid"))?;
    let killed = executor
        .continue_process(ContinueProcessRequest {
            handle,
            input: None,
            close_stdin: false,
            signal: Some(ExecutorSignal::Kill),
            wait_ms: Some(2_000),
            max_output_bytes: None,
        })
        .await?;
    assert_eq!(killed.status, ProcessStatus::Killed);
    assert_eq!(killed.handle, None);
    wait_for_process_gone(root_pid).await?;
    connection.close().await?;
    Ok(())
}

fn process_id(id: &str) -> anyhow::Result<ProcessId> {
    Ok(ProcessId::new(id)?)
}

fn path(root: &Path, name: &str) -> anyhow::Result<EnvironmentPath> {
    let path = root.join(name);
    Ok(EnvironmentPath::new(path.to_str().ok_or_else(|| {
        anyhow::anyhow!("non-utf-8 path {}", path.display())
    })?)?)
}

fn shell_process(process_id: &str, script: &str, pty: bool) -> anyhow::Result<StartProcessParams> {
    Ok(StartProcessParams {
        process_id: self::process_id(process_id)?,
        command: CommandSpec {
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd: None,
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            input: ExecInput::None {},
            timeout_ms: Some(60_000),
        },
        pty: pty.then_some(PtySize { rows: 24, cols: 80 }),
    })
}

/// A cursor read that returns once the process exited and its output
/// settled.
fn read_until_exit(process_id: &str) -> anyhow::Result<ReadProcessParams> {
    Ok(ReadProcessParams {
        process_id: self::process_id(process_id)?,
        after: None,
        tail_bytes: None,
        ack: None,
        max_bytes: 65_536,
        wait_ms: 10_000,
        return_on: ReturnOn::Exit,
    })
}

fn page_text(chunks: &[OutputChunk]) -> String {
    let bytes = chunks
        .iter()
        .flat_map(|chunk| chunk.data.as_slice().to_vec())
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn process_alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn wait_for_pid_file(path: &Path) -> anyhow::Result<i32> {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        if let Ok(content) = std::fs::read_to_string(path)
            && let Ok(pid) = content.trim().parse::<i32>()
        {
            return Ok(pid);
        }
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!("pid file {} was not written", path.display());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_process_gone(pid: i32) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        if !process_alive(pid) {
            return Ok(());
        }
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!("process {pid} is still alive");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn read_receipt(path: &Path) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

async fn wait_for_receipt_admission(path: &Path, admission: &str) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        if read_receipt(path).is_ok_and(|receipt| receipt["admission"] == admission) {
            return Ok(());
        }
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!("receipt {} never said {admission}", path.display());
        }
        tokio::time::sleep(POLL).await;
    }
}

/// A canonical, empty working tree for one daemon.
fn workspace(sandbox: &Path, name: &str) -> anyhow::Result<std::path::PathBuf> {
    let root = sandbox.join(name);
    std::fs::create_dir_all(&root)?;
    Ok(root.canonicalize()?)
}

/// A daemon serving `root`, with its state outside the tree it serves.
fn daemon_config(
    root: &Path,
    state_dir: &Path,
    connect_url: &str,
    key: Option<&str>,
    receipt: Option<&Path>,
    on_closed: OnClosed,
) -> anyhow::Result<DaemonConfig> {
    let mut metadata = BTreeMap::new();
    metadata.insert("harbor.trialId".to_owned(), "trial-1".to_owned());
    let mut registration = RegistrationConfig::new(
        connect_url.to_owned(),
        environment_daemon::upgrade::resolve_discovery_url(Some(connect_url), None)?,
    );
    registration.registration_key = key.map(SecretString::new);
    registration.display_name = Some(DisplayName::new("registration live daemon")?);
    registration.metadata = Metadata::new(metadata)?;
    registration.receipt_path = receipt.map(Path::to_path_buf);
    registration.on_closed = on_closed;
    Ok(DaemonConfig {
        listen: None,
        cwd: root.to_path_buf(),
        fs_root: root.to_path_buf(),
        state_dir: state_dir.to_path_buf(),
        read_only_fs: false,
        registration: Some(registration),
        scrubbed_env: Vec::new(),
    })
}

/// One running daemon: how it ends, and the runtime whose shutdown token
/// stops it deliberately.
struct Daemon {
    exit: tokio::task::JoinHandle<DaemonExit>,
    runtime: DaemonRuntime,
}

fn spawn_daemon(config: DaemonConfig) -> anyhow::Result<Daemon> {
    let runtime = DaemonRuntime::new(config)?;
    let exit = tokio::spawn(environment_daemon::server::run(runtime.clone()));
    Ok(Daemon { exit, runtime })
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_millis(),
    )
    .expect("millisecond clock fits i64")
}

/// The registered facts of an environment view.
struct RegisteredFacts {
    registration_key_id: String,
    daemon_id: String,
    identity_mode: EnvironmentIdentityModeView,
    connection_changed_at_ms: i64,
    last_seen_at_ms: i64,
}

fn registered_source(view: &EnvironmentView) -> anyhow::Result<RegisteredFacts> {
    let EnvironmentSourceView::Registered {
        registration_key_id,
        daemon_id,
        identity_mode,
        connection_changed_at_ms,
        last_seen_at_ms,
        ..
    } = &view.source
    else {
        anyhow::bail!("expected a registered source, got {:?}", view.source)
    };
    Ok(RegisteredFacts {
        registration_key_id: registration_key_id.clone(),
        daemon_id: daemon_id.clone(),
        identity_mode: *identity_mode,
        connection_changed_at_ms: *connection_changed_at_ms,
        last_seen_at_ms: *last_seen_at_ms,
    })
}

/// Whether a registered environment's daemon is connected; `None` for
/// other kinds.
fn is_connected(view: &EnvironmentView) -> Option<bool> {
    match &view.source {
        EnvironmentSourceView::Registered { connected, .. } => Some(*connected),
        _ => None,
    }
}

/// Dial the worker route and expect the gateway to refuse the upgrade.
async fn expect_route_refusal(
    gateway: &EnvironmentGatewayClientConfig,
    key: &RouteKey,
) -> anyhow::Result<ClientError> {
    match gateway
        .connect(key, gateway.connect_options("registration-live"))
        .await
    {
        Ok((client, _)) => {
            client.close().await?;
            anyhow::bail!("the worker route upgraded for {}", key.environment_id)
        }
        Err(error) => Ok(error),
    }
}

async fn read_key(
    api: &GatewayAgentApi,
    key_id: &str,
) -> anyhow::Result<api::EnvironmentRegistrationKeyView> {
    Ok(api
        .read_environment_registration_key(EnvironmentRegistrationKeyReadParams {
            registration_key_id: key_id.to_owned(),
        })
        .await?
        .result
        .registration_key)
}

/// Wait until exactly `count` open environments are listed under the key
/// and one of them is connected.
async fn wait_for_registered(
    api: &GatewayAgentApi,
    key_id: &str,
    count: usize,
    connected: fn(&EnvironmentView) -> Option<bool>,
) -> anyhow::Result<EnvironmentView> {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        let environments = list_by_key(api, key_id, false).await?;
        if environments.len() == count
            && let Some(environment) = environments
                .iter()
                .find(|environment| connected(environment) == Some(true))
        {
            return Ok(environment.clone());
        }
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!(
                "timed out waiting for {count} environment(s) under {key_id} with one connected: {environments:?}"
            );
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Wait until exactly `count` open environments are listed under the key,
/// all connected.
async fn wait_for_registered_count(
    api: &GatewayAgentApi,
    key_id: &str,
    count: usize,
) -> anyhow::Result<Vec<EnvironmentView>> {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        let environments = list_by_key(api, key_id, false).await?;
        if environments.len() == count
            && environments
                .iter()
                .all(|environment| is_connected(environment) == Some(true))
        {
            return Ok(environments);
        }
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!("timed out waiting for {count} connected environments: {environments:?}");
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn list_by_key(
    api: &GatewayAgentApi,
    key_id: &str,
    include_closed: bool,
) -> anyhow::Result<Vec<EnvironmentView>> {
    Ok(api
        .list_environments(EnvironmentListParams {
            registration_key_id: Some(key_id.to_owned()),
            include_closed,
            ..EnvironmentListParams::default()
        })
        .await?
        .result
        .environments)
}

async fn wait_for_environment(
    api: &GatewayAgentApi,
    environment_id: &str,
    what: &str,
    ready: impl Fn(&EnvironmentView) -> bool,
) -> anyhow::Result<EnvironmentView> {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        let environment = api
            .read_environment(EnvironmentReadParams {
                environment_id: environment_id.to_owned(),
            })
            .await?
            .result
            .environment;
        if ready(&environment) {
            return Ok(environment);
        }
        if tokio::time::Instant::now() > deadline {
            anyhow::bail!(
                "timed out waiting for {environment_id} to be {what}; last {environment:?}"
            );
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn wait_for_state(
    api: &GatewayAgentApi,
    environment_id: &str,
    state: EnvironmentStateView,
) -> anyhow::Result<EnvironmentView> {
    wait_for_environment(api, environment_id, &format!("{state:?}"), |view| {
        view.state == state
    })
    .await
}

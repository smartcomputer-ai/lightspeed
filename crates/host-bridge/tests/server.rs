use std::{collections::BTreeMap, time::Duration};

use host_bridge::{BridgeRuntime, config::BridgeConfig, server};
use host_client::{HostControllerClient, HostDataClient, WebSocketConnectOptions};
use host_protocol::{
    control::{
        handshake::ControllerInitializeParams,
        targets::{AttachTargetParams, HostTargetAttachRequest, ListTargetsParams},
    },
    data::{
        handshake::{InitializeParams, InitializedParams},
        process::{ReadProcessParams, StartProcessParams},
    },
    shared::{CURRENT_PROTOCOL_VERSION, HostScope, ProcessId},
};
use tokio::net::TcpListener;

#[tokio::test(flavor = "current_thread")]
async fn bridge_serves_controller_attach_and_process_data_plane() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let local_addr = listener.local_addr().expect("local addr");
    let config = BridgeConfig {
        gateway_url: "http://127.0.0.1:18080/rpc".to_owned(),
        provider_id: "test-provider".to_owned(),
        provider_token: None,
        target_id: "local".to_owned(),
        listen: local_addr,
        advertise_url: None,
        cwd: root.clone(),
        fs_root: root,
        heartbeat_interval: Duration::from_millis(10_000),
        lease_ttl: Duration::from_millis(30_000),
        read_only_fs: false,
    };
    let runtime = BridgeRuntime::new(config, local_addr).expect("runtime");
    let controller_endpoint = runtime.controller_endpoint();
    let server_task = tokio::spawn(server::run_server(listener, runtime));

    let mut controller =
        HostControllerClient::connect(&controller_endpoint, WebSocketConnectOptions::default())
            .await
            .expect("connect controller");
    let initialized = controller
        .initialize(&ControllerInitializeParams {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            client_name: "host-bridge-test".to_owned(),
        })
        .await
        .expect("controller initialize");
    assert!(initialized.capabilities.attach_target);
    assert!(!initialized.capabilities.create_target);

    let targets = controller
        .list_targets(&ListTargetsParams { status: None })
        .await
        .expect("list targets");
    assert_eq!(targets.targets.len(), 1);
    assert_eq!(targets.targets[0].target_id.as_str(), "local");

    let attached = controller
        .attach_target(&AttachTargetParams {
            request: HostTargetAttachRequest::Target {
                target_id: "local".into(),
            },
        })
        .await
        .expect("attach target");
    assert!(attached.connection.capabilities.process_start);

    let mut data = HostDataClient::connect(
        &attached.connection.endpoint,
        WebSocketConnectOptions::default(),
    )
    .await
    .expect("connect data");
    data.initialize(&InitializeParams {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        client_name: "host-bridge-test".to_owned(),
        scope: HostScope::Default,
        resume_connection_id: None,
    })
    .await
    .expect("data initialize");
    data.initialized(&InitializedParams {})
        .await
        .expect("data initialized");

    let process_id = ProcessId::new("proc-1");
    data.start_process(&StartProcessParams {
        process_id: process_id.clone(),
        argv: vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "printf hello".to_owned(),
        ],
        cwd: attached.connection.default_cwd,
        env: BTreeMap::new(),
        stdin: None,
        timeout_ms: Some(5_000),
        tty: false,
        pipe_stdin: false,
    })
    .await
    .expect("start process");
    let output = data
        .read_process(&ReadProcessParams {
            process_id,
            after_seq: None,
            max_bytes: None,
            wait_ms: None,
        })
        .await
        .expect("read process");
    let stdout = output
        .chunks
        .iter()
        .flat_map(|chunk| chunk.chunk.as_slice().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(stdout, b"hello");
    assert_eq!(output.exit_code, Some(0));

    server_task.abort();
}

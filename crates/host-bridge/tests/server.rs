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
        jobs::{
            JobDependencyPolicy, JobStartSpec, ListJobsParams, ReadJobsParams, StartJobsParams,
        },
        process::{ReadProcessParams, StartProcessParams},
    },
    shared::{CURRENT_PROTOCOL_VERSION, HostScope, JobId, ProcessId},
};
use tokio::net::TcpListener;
use tools::{
    fs::FsPath,
    host_protocol::{HostDataConformanceOptions, assert_host_data_conformance},
};

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
        fs_root: root.clone(),
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
    assert!(attached.connection.capabilities.job_start);
    assert!(attached.connection.capabilities.job_read);
    assert!(attached.connection.capabilities.job_cancel);

    let conformance_client = HostDataClient::connect(
        &attached.connection.endpoint,
        WebSocketConnectOptions::default(),
    )
    .await
    .expect("connect conformance client");
    let default_cwd = attached
        .connection
        .default_cwd
        .as_ref()
        .expect("bridge default cwd");
    let forbidden_path = root
        .parent()
        .expect("root parent")
        .join("outside-host-route");
    assert_host_data_conformance(
        conformance_client,
        HostDataConformanceOptions {
            initialize: InitializeParams {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                client_name: "host-bridge-conformance".to_owned(),
                scope: HostScope::Default,
                resume_connection_id: None,
            },
            expected_capabilities: attached.connection.capabilities.clone(),
            expected_default_cwd: FsPath::new(default_cwd.as_str()).expect("default cwd"),
            test_directory: FsPath::new("host-data-conformance").expect("test directory"),
            forbidden_path: FsPath::new(forbidden_path.to_string_lossy()).expect("forbidden path"),
        },
    )
    .await
    .expect("host data conformance");

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
        cwd: attached.connection.default_cwd.clone(),
        env: BTreeMap::new(),
        secret_env: BTreeMap::new(),
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

    data.start_jobs(&StartJobsParams {
        namespace: "session_1".to_owned(),
        request_id: "server".to_owned(),
        jobs: vec![JobStartSpec {
            job_id: JobId::new("job-1"),
            name: Some("server-job".to_owned()),
            argv: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "printf job-ok".to_owned(),
            ],
            cwd: attached.connection.default_cwd.clone(),
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            stdin: None,
            timeout_ms: Some(5_000),
            depends_on: Vec::new(),
            dependency_policy: JobDependencyPolicy::AllSucceeded,
            queue_key: None,
        }],
    })
    .await
    .expect("start job");
    let listed = data
        .list_jobs(&ListJobsParams {
            namespace: "session_1".to_owned(),
            limit: Some(10),
        })
        .await
        .expect("list jobs");
    assert_eq!(listed.jobs[0].job_id.as_str(), "job-1");
    let jobs = data
        .read_jobs(&ReadJobsParams {
            namespace: "session_1".to_owned(),
            jobs: vec![JobId::new("job-1")],
            after_seq: None,
            max_bytes: None,
            include_artifacts: false,
            wait_ms: Some(5_000),
        })
        .await
        .expect("read job");
    let job = &jobs.jobs[0];
    assert_eq!(job.summary.exit_code, Some(0));
    let job_stdout = job
        .output_chunks
        .iter()
        .flat_map(|chunk| chunk.chunk.as_slice().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(job_stdout, b"job-ok");

    server_task.abort();
}

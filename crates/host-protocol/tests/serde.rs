use std::collections::BTreeMap;

use host_protocol::{
    control::{
        handshake::ControllerInitializeParams,
        methods::{
            ATTACH_TARGET_METHOD, CREATE_TARGET_METHOD,
            INITIALIZE_METHOD as CONTROL_INITIALIZE_METHOD, LIST_TARGETS_METHOD,
        },
        targets::{
            AttachTargetParams, CreateTargetParams, CreateTargetResponse, HostTargetAttachRequest,
            HostTargetCreateRequest, HostTargetStatus, HostTargetSummary, ListTargetsResponse,
        },
    },
    data::{
        fs::ReadFileResponse,
        handshake::InitializeParams,
        jobs::{
            JobArtifact, JobDependency, JobDependencyPolicy, JobOutputChunk, JobOutputStream,
            JobReadResult, JobStartSpec, JobStatus, JobSummary, ListJobsParams, ReadJobsResponse,
            StartJobsParams,
        },
        methods::{
            FS_READ_FILE_METHOD, INITIALIZE_METHOD, JOB_CANCEL_METHOD, JOB_LIST_METHOD,
            JOB_READ_METHOD, JOB_START_METHOD, PROCESS_OUTPUT_METHOD, PROCESS_START_METHOD,
        },
        process::{
            ProcessOutputChunk, ProcessOutputStream, ReadProcessResponse, StartProcessParams,
        },
    },
    shared::{
        ByteChunk, CURRENT_PROTOCOL_VERSION, HostCapabilities, HostConnectionId,
        HostConnectionSpec, HostScope, HostTargetId, HostTransport, JobId, ProcessId,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    serde_json::from_str(match name {
        "data_initialize_params" => include_str!("../fixtures/data_initialize_params.json"),
        "fs_read_file_response" => include_str!("../fixtures/fs_read_file_response.json"),
        "process_start_params" => include_str!("../fixtures/process_start_params.json"),
        "process_read_response" => include_str!("../fixtures/process_read_response.json"),
        "job_start_params" => include_str!("../fixtures/job_start_params.json"),
        "job_read_response" => include_str!("../fixtures/job_read_response.json"),
        "controller_initialize_params" => {
            include_str!("../fixtures/controller_initialize_params.json")
        }
        "controller_create_target_params" => {
            include_str!("../fixtures/controller_create_target_params.json")
        }
        "controller_create_target_response" => {
            include_str!("../fixtures/controller_create_target_response.json")
        }
        "controller_attach_target_params" => {
            include_str!("../fixtures/controller_attach_target_params.json")
        }
        "controller_list_targets_response" => {
            include_str!("../fixtures/controller_list_targets_response.json")
        }
        other => panic!("unknown fixture {other}"),
    })
    .expect("fixture JSON")
}

fn assert_round_trip<T>(value: T, expected: Value)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_value(&value).expect("serialize");
    assert_eq!(encoded, expected);
    let decoded: T = serde_json::from_value(encoded).expect("deserialize");
    assert_eq!(decoded, value);
}

#[test]
fn method_names_match_data_plane_contract() {
    assert_eq!(INITIALIZE_METHOD, "initialize");
    assert_eq!(FS_READ_FILE_METHOD, "fs/readFile");
    assert_eq!(PROCESS_START_METHOD, "process/start");
    assert_eq!(JOB_START_METHOD, "job/start");
    assert_eq!(JOB_LIST_METHOD, "job/list");
    assert_eq!(JOB_READ_METHOD, "job/read");
    assert_eq!(JOB_CANCEL_METHOD, "job/cancel");
    assert_eq!(PROCESS_OUTPUT_METHOD, "process/output");
}

#[test]
fn method_names_match_controller_plane_contract() {
    assert_eq!(CONTROL_INITIALIZE_METHOD, "controller/initialize");
    assert_eq!(LIST_TARGETS_METHOD, "controller/listTargets");
    assert_eq!(CREATE_TARGET_METHOD, "controller/createTarget");
    assert_eq!(ATTACH_TARGET_METHOD, "controller/attachTarget");
}

#[test]
fn initialize_params_match_fixture() {
    assert_round_trip(
        InitializeParams {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            client_name: "lightspeed-test".to_owned(),
            scope: HostScope::Session {
                session_id: "sandbox-123".to_owned(),
            },
            resume_connection_id: Some(HostConnectionId::new("conn-prev")),
        },
        fixture("data_initialize_params"),
    );
}

#[test]
fn read_file_response_uses_base64_byte_chunk() {
    assert_round_trip(
        ReadFileResponse {
            data: ByteChunk::from(b"hello\n".as_slice()),
        },
        fixture("fs_read_file_response"),
    );
}

#[test]
fn process_start_params_match_fixture() {
    assert_round_trip(
        StartProcessParams {
            process_id: ProcessId::new("proc-1"),
            argv: vec!["sh".to_owned(), "-lc".to_owned(), "cat".to_owned()],
            cwd: Some("/workspace".try_into().expect("cwd")),
            env: BTreeMap::from([("RUST_LOG".to_owned(), "debug".to_owned())]),
            secret_env: BTreeMap::new(),
            stdin: Some(ByteChunk::from(b"input\n".as_slice())),
            timeout_ms: Some(60_000),
            tty: false,
            pipe_stdin: true,
        },
        fixture("process_start_params"),
    );
}

#[test]
fn process_read_response_preserves_ordered_output_chunks() {
    assert_round_trip(
        ReadProcessResponse {
            chunks: vec![
                ProcessOutputChunk {
                    seq: 1,
                    stream: ProcessOutputStream::Stdout,
                    chunk: ByteChunk::from(b"ok\n".as_slice()),
                },
                ProcessOutputChunk {
                    seq: 2,
                    stream: ProcessOutputStream::Stderr,
                    chunk: ByteChunk::from(b"warn\n".as_slice()),
                },
            ],
            next_seq: 3,
            exited: true,
            exit_code: Some(0),
            closed: true,
            failure: None,
        },
        fixture("process_read_response"),
    );
}

#[test]
fn job_start_params_match_fixture() {
    assert_round_trip(
        StartJobsParams {
            namespace: "session_1".to_owned(),
            request_id: "request-1".to_owned(),
            jobs: vec![JobStartSpec {
                job_id: JobId::new("checkout"),
                name: Some("checkout".to_owned()),
                argv: vec!["git".to_owned(), "status".to_owned()],
                cwd: Some("/workspace".try_into().expect("cwd")),
                env: BTreeMap::from([("RUST_LOG".to_owned(), "debug".to_owned())]),
                secret_env: BTreeMap::new(),
                stdin: Some(ByteChunk::from(b"hello\n".as_slice())),
                timeout_ms: Some(60_000),
                depends_on: vec![JobDependency::name("setup")],
                dependency_policy: JobDependencyPolicy::AllSucceeded,
                queue_key: Some("repo".to_owned()),
            }],
        },
        fixture("job_start_params"),
    );
}

#[test]
fn job_read_response_matches_fixture() {
    assert_round_trip(
        ReadJobsResponse {
            jobs: vec![JobReadResult {
                summary: JobSummary {
                    namespace: "session_1".to_owned(),
                    job_id: JobId::new("tests"),
                    name: Some("tests".to_owned()),
                    status: JobStatus::Succeeded,
                    dependencies: vec![JobId::new("checkout")],
                    created_at_ms: 1,
                    queued_at_ms: Some(2),
                    started_at_ms: Some(3),
                    finished_at_ms: Some(4),
                    exit_code: Some(0),
                    failure: None,
                    queue_key: Some("repo".to_owned()),
                },
                output_chunks: vec![JobOutputChunk {
                    seq: 1,
                    stream: JobOutputStream::Stdout,
                    chunk: ByteChunk::from(b"ok\n".as_slice()),
                }],
                output_next_seq: 2,
                artifacts: vec![JobArtifact {
                    path: "/workspace/result.txt".try_into().expect("artifact path"),
                    kind: Some("file".to_owned()),
                    metadata: BTreeMap::from([("role".to_owned(), "report".to_owned())]),
                }],
            }],
        },
        fixture("job_read_response"),
    );
}

#[test]
fn job_list_params_use_session_namespace_and_limit() {
    assert_round_trip(
        ListJobsParams {
            namespace: "session_1".to_owned(),
            limit: Some(25),
        },
        json!({
            "namespace": "session_1",
            "limit": 25
        }),
    );
}

#[test]
fn byte_chunk_rejects_invalid_base64() {
    assert!(serde_json::from_value::<ByteChunk>(json!("not base64 !!!")).is_err());
}

#[test]
fn controller_initialize_params_match_fixture() {
    assert_round_trip(
        ControllerInitializeParams {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            client_name: "lightspeed-test".to_owned(),
        },
        fixture("controller_initialize_params"),
    );
}

#[test]
fn create_target_params_match_provider_fixture() {
    assert_round_trip(
        CreateTargetParams {
            request: HostTargetCreateRequest::Provider {
                provider_type: "smolvm".to_owned(),
                spec: json!({
                    "cpus": 2,
                    "image": "lightspeed-dev"
                }),
            },
        },
        fixture("controller_create_target_params"),
    );
}

#[test]
fn create_target_response_carries_data_plane_connection_spec() {
    assert_round_trip(
        CreateTargetResponse {
            target: ready_target_summary(),
            connection: data_plane_connection_spec(),
        },
        fixture("controller_create_target_response"),
    );
}

#[test]
fn attach_target_params_match_existing_target_fixture() {
    assert_round_trip(
        AttachTargetParams {
            request: HostTargetAttachRequest::Target {
                target_id: HostTargetId::new("sandbox-123"),
            },
        },
        fixture("controller_attach_target_params"),
    );
}

#[test]
fn list_targets_response_matches_fixture() {
    assert_round_trip(
        ListTargetsResponse {
            targets: vec![ready_target_summary()],
        },
        fixture("controller_list_targets_response"),
    );
}

fn ready_target_summary() -> HostTargetSummary {
    HostTargetSummary {
        target_id: HostTargetId::new("sandbox-123"),
        display_name: Some("lightspeed dev".to_owned()),
        status: HostTargetStatus::Ready,
        scope: HostScope::Session {
            session_id: "sandbox-123".to_owned(),
        },
        capabilities: remote_host_capabilities(),
        default_cwd: Some("/workspace".try_into().expect("cwd")),
        metadata: BTreeMap::from([("provider".to_owned(), "smolvm".to_owned())]),
    }
}

fn data_plane_connection_spec() -> HostConnectionSpec {
    HostConnectionSpec {
        target_id: HostTargetId::new("sandbox-123"),
        endpoint: "wss://host.example/data/sandbox-123".to_owned(),
        transport: HostTransport::WebSocket,
        scope: HostScope::Session {
            session_id: "sandbox-123".to_owned(),
        },
        default_cwd: Some("/workspace".try_into().expect("cwd")),
        capabilities: remote_host_capabilities(),
    }
}

fn remote_host_capabilities() -> HostCapabilities {
    HostCapabilities {
        filesystem_read: true,
        filesystem_write: true,
        process_start: true,
        process_stdin: true,
        process_terminate: true,
        process_output_polling: true,
        process_output_notifications: true,
        process_pty: false,
        job_start: true,
        job_list: true,
        job_read: true,
        job_cancel: true,
        job_wait_hint: false,
        job_dependencies: true,
        job_queue_keys: true,
    }
}

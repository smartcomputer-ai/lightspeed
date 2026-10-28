//! Wire-shape rules that the per-method fixtures do not spell out.

use environment_protocol::{
    CONTROLLER_PROTOCOL_VERSION, ENVD_PROTOCOL_VERSION,
    control::{
        handshake::{ControllerInitializeParams, ControllerInitializeResponse},
        target::{CreateTargetParams, PowerState, TargetObservation, TargetPhase, TargetResponse},
        template::ListTemplatesResponse,
    },
    data::{
        exec::{ExecInput, ExitStatus, GapReason, OutputPage},
        handshake::{DataCapabilities, InitializeParams, InitializeResponse},
        job::{JobDependency, JobReadEntry, JobStatus, ReadJobsResponse, StartJobsParams},
        process::{ReadProcessParams, ReadProcessResponse},
        scan::{ScanResponse, ScanRootResult},
    },
    error::{NotFoundReason, ProtocolError, ProtocolErrorKind},
    handshake::Plane,
    manifest::{FsScan, ProcessRead, RequestMethod},
    registration::{RegisterParams, RegisterResponse},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn fixture(path: &str) -> Value {
    let full = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(path);
    serde_json::from_str(&std::fs::read_to_string(&full).expect("fixture")).expect("JSON")
}

fn decode<T: DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).expect("decode")
}

#[test]
fn both_numbers_are_four() {
    assert_eq!(ENVD_PROTOCOL_VERSION, 4);
    assert_eq!(CONTROLLER_PROTOCOL_VERSION, 4);
}

#[test]
fn handshakes_require_a_stated_protocol_version() {
    let client_info = json!({"name": "x"});
    assert!(
        serde_json::from_value::<InitializeParams>(json!({"clientInfo": client_info})).is_err()
    );
    assert!(
        serde_json::from_value::<ControllerInitializeParams>(json!({"clientInfo": client_info}))
            .is_err()
    );
    let mut register = fixture("registration/registration.register.json")["params"].clone();
    register.as_object_mut().unwrap().remove("protocolVersion");
    assert!(serde_json::from_value::<RegisterParams>(register).is_err());
}

#[test]
fn initialize_names_the_plane_and_the_daemon_instance() {
    let response: InitializeResponse = decode(&fixture("data/initialize.json")["response"]);
    assert_eq!(response.plane, Plane::Data);
    assert_eq!(response.daemon_instance_id.as_str(), "envd_01jb8x7k2m");
    assert!(response.capabilities.filesystem.unwrap().write);
    assert!(response.limits.max_concurrent_requests >= 16);
    let absent: DataCapabilities = decode(&json!({}));
    assert!(absent.process.is_none(), "an absent family is unavailable");
}

#[test]
fn process_reads_require_bounded_waits_and_report_byte_gaps() {
    let mut params = fixture("data/process.read.json")["params"].clone();
    params.as_object_mut().unwrap().remove("waitMs");
    assert!(serde_json::from_value::<ReadProcessParams>(params).is_err());

    let response: ReadProcessResponse = decode(&fixture("data/process.read.json")["response"]);
    let OutputPage {
        chunks,
        gaps,
        next,
        end,
    } = response.output;
    let gap = gaps[0];
    assert_eq!(gap.reason, GapReason::Overflow);
    assert_eq!(chunks[0].offset + chunks[0].data.len() as u64, gap.offset);
    assert_eq!(gap.offset + gap.omitted_bytes, chunks[1].offset);
    assert_eq!(next, end);
    assert!(matches!(
        response.exit,
        Some(ExitStatus::Signal {
            core_dumped: true,
            ..
        })
    ));
}

#[test]
fn job_entries_report_unknown_ids_as_errors() {
    let response: ReadJobsResponse = decode(&fixture("data/job.read.json")["response"]);
    let JobReadEntry::Error { job_id, error } = &response.jobs[2] else {
        panic!("expected an error entry");
    };
    assert_eq!(job_id.as_str(), "gone");
    assert_eq!(error.kind(), ProtocolErrorKind::NotFound);
    let JobReadEntry::Job { summary, .. } = &response.jobs[0] else {
        panic!("expected a job entry");
    };
    assert_eq!(summary.status, JobStatus::Failed);
    assert!(summary.status.is_terminal());
    assert!(!JobStatus::Queued.is_terminal());
}

#[test]
fn job_dependencies_are_kind_tagged() {
    let params: StartJobsParams = decode(&fixture("data/job.start.json")["params"]);
    assert!(matches!(
        &params.jobs[1].depends_on[0],
        JobDependency::Name { name } if name.as_str() == "setup"
    ));
    assert!(
        serde_json::from_value::<JobDependency>(
            json!({"kind": "jobId", "jobId": "a", "name": "b"})
        )
        .is_err()
    );
    let mut job = fixture("data/job.start.json")["params"].clone();
    job["jobs"][0]["namespace"] = json!("shared");
    assert!(serde_json::from_value::<StartJobsParams>(job).is_err());
    let pipe = json!({"argv": ["cat"], "input": {"kind": "pipe"}});
    let spec: environment_protocol::data::exec::CommandSpec = decode(&pipe);
    assert_eq!(spec.input, ExecInput::Pipe {});
}

#[test]
fn scan_roots_are_reported_independently() {
    let response: ScanResponse = decode(&fixture("data/fs.scan.json")["response"]);
    assert_eq!(response.roots.len(), 3);
    assert!(matches!(
        response.roots[1],
        ScanRootResult::Unchanged { .. }
    ));
    let ScanRootResult::Complete { skipped, .. } = &response.roots[2] else {
        panic!("expected a complete root");
    };
    assert!(
        skipped[1].path.is_none(),
        "non-UTF-8 names are counted, not returned"
    );
    assert_eq!(FsScan::PLANE, Plane::Data);
}

#[test]
fn close_by_owner_answers_closed_without_a_target_id() {
    let response: TargetResponse = decode(&fixture("controller/target.close.json")["response"]);
    let TargetObservation {
        target_id,
        phase,
        power,
        ..
    } = response.target;
    assert!(target_id.is_none());
    assert_eq!(phase, TargetPhase::Closed);
    assert!(power.is_none());
}

#[test]
fn owners_name_only_the_universe_and_environment() {
    let mut params = fixture("controller/target.get.json")["params"].clone();
    params["target"]["owner"]["bindingId"] = json!("primary");
    assert!(
        serde_json::from_value::<environment_protocol::control::target::GetTargetParams>(params)
            .is_err()
    );
    let mut observation = fixture("controller/target.get.json")["response"].clone();
    observation["target"]["daemonReady"] = json!(true);
    assert!(serde_json::from_value::<TargetResponse>(observation).is_err());
}

#[test]
fn templates_carry_power_states_and_a_default_idle_policy() {
    let response: ListTemplatesResponse =
        decode(&fixture("controller/template.list.json")["response"]);
    let template = &response.templates[0];
    assert!(template.power_states.contains(&PowerState::Paused));
    let idle = template.idle.as_ref().expect("default idle policy");
    assert_eq!(idle.validate(), Ok(()));
    let create: CreateTargetParams = decode(&fixture("controller/target.create.json")["params"]);
    assert_eq!(create.idle.expect("override").pause_after_ms, Some(600_000));
}

#[test]
fn controller_capabilities_name_power_and_ingress_only() {
    let response: ControllerInitializeResponse =
        decode(&fixture("controller/initialize.json")["response"]);
    assert!(response.capabilities.power && response.capabilities.ingress);
    let mut routed = fixture("controller/initialize.json")["response"].clone();
    routed["routes"] = json!({"data": "wss://p.example/routes"});
    assert!(serde_json::from_value::<ControllerInitializeResponse>(routed).is_err());
}

#[test]
fn the_receipt_is_nested_and_typed() {
    let response: RegisterResponse =
        decode(&fixture("registration/registration.register.json")["response"]);
    assert_eq!(response.receipt.daemon_id.as_str(), "daemon_3b3b");
    let mut flattened = fixture("registration/registration.register.json")["response"].clone();
    flattened["environmentId"] = json!("env_1");
    assert!(serde_json::from_value::<RegisterResponse>(flattened).is_err());
}

#[test]
fn errors_carry_typed_data() {
    let error: ProtocolError = decode(&fixture("data/process.read.json")["error"]);
    assert_eq!(error.kind(), ProtocolErrorKind::NotFound);
    assert!(matches!(
        error.data,
        environment_protocol::error::ErrorData::NotFound {
            reason: Some(NotFoundReason::Expired)
        }
    ));
    assert!(!error.is_retryable());
    assert_eq!(ProcessRead::NAME, "process/read");
}

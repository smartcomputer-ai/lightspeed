use super::*;
use host_protocol::shared::HostTransport;

fn provider_registration(provider_id: &str) -> RegisterEnvironmentProvider {
    RegisterEnvironmentProvider {
        provider_id: EnvironmentProviderId::new(provider_id),
        provider_kind: EnvironmentProviderKind::Bridge,
        display_name: Some("Local bridge".to_owned()),
        controller_connection: HostControllerConnectionSpec::new(
            "ws://127.0.0.1:9000/controller",
            HostTransport::WebSocket,
        ),
        capabilities: EnvironmentProviderCapabilities {
            list_targets: true,
            attach_target: true,
            get_target: true,
            ..EnvironmentProviderCapabilities::default()
        },
        implementation: ImplementationInfo {
            name: "test-bridge".to_owned(),
            version: Some("1.0.0".to_owned()),
        },
        lease_ttl_ms: 30_000,
        metadata: BTreeMap::new(),
        observed_at_ms: 10,
    }
}

fn host_connection(target_id: &str) -> HostConnectionSpec {
    HostConnectionSpec {
        target_id: HostTargetId::new(target_id),
        endpoint: "ws://127.0.0.1:9001/data".to_owned(),
        transport: HostTransport::WebSocket,
        scope: HostScope::Session {
            session_id: "session_1".to_owned(),
        },
        default_cwd: Some(HostPath::new("/workspace").expect("cwd")),
        capabilities: HostCapabilities::filesystem(true, true).with_process(),
    }
}

fn target(provider_id: &str, target_id: &str) -> UpsertEnvironmentTargetRecord {
    UpsertEnvironmentTargetRecord {
        provider_id: EnvironmentProviderId::new(provider_id),
        target_id: HostTargetId::new(target_id),
        display_name: Some("Local host".to_owned()),
        status: HostTargetStatus::Ready,
        scope: HostScope::Default,
        capabilities: HostCapabilities::filesystem(true, true).with_process(),
        default_cwd: Some(HostPath::new("/workspace").expect("cwd")),
        metadata: BTreeMap::new(),
        observed_at_ms: 20,
    }
}

fn binding(session_id: &str, env_id: &str) -> CreateSessionEnvironmentBinding {
    CreateSessionEnvironmentBinding {
        session_id: SessionId::new(session_id),
        env_id: EnvironmentId::new(env_id),
        provider_id: EnvironmentProviderId::new("bridge-local"),
        target_id: HostTargetId::new("local-host"),
        kind: SessionEnvironmentKind::AttachedHost,
        status: SessionEnvironmentBindingStatus::Ready,
        capabilities: SessionEnvironmentCapabilities {
            fs_read: true,
            fs_write: true,
            process_exec: true,
            process_stdin: true,
            network: false,
            persistent: true,
        },
        connection: host_connection("local-host"),
        cwd: Some(HostPath::new("/workspace").expect("cwd")),
        fs_routes: vec![SessionEnvironmentFsRoute {
            path: HostPath::new("/workspace").expect("route"),
            source_path: None,
            access: SessionEnvironmentFsRouteAccess::ReadWrite,
            same_state_as_active_env: Some(EnvironmentId::new(env_id)),
        }],
        created_at_ms: 30,
    }
}

fn job_handle(session_id: &str, env_id: &str, job_id: &str) -> CreateJobHandle {
    CreateJobHandle {
        session_id: SessionId::new(session_id),
        env_id: EnvironmentId::new(env_id),
        provider_id: EnvironmentProviderId::new("bridge-local"),
        target_id: HostTargetId::new("local-host"),
        job_id: JobId::new(job_id),
        deck_id: Some("deck-1".to_owned()),
        name: Some(job_id.to_owned()),
        serial_lane: Some("repo".to_owned()),
        idempotency_key: Some("idem-1".to_owned()),
        created_by_run_id: Some(RunId::new(1)),
        created_by_turn_id: Some(TurnId::new(2)),
        created_by_tool_call_id: Some(ToolCallId::new("call_1")),
        created_at_ms: 70,
        start_request_hash: "hash-1".to_owned(),
        metadata: BTreeMap::from([("kind".to_owned(), "test".to_owned())]),
    }
}

#[test]
fn provider_records_validate_controller_shape() {
    let record = provider_registration("bridge-local")
        .into_record()
        .expect("record");

    record.validate().expect("valid provider");
}

#[test]
fn provider_records_reject_empty_capabilities() {
    let mut registration = provider_registration("bridge-local");
    registration.capabilities = EnvironmentProviderCapabilities::default();

    let error = registration
        .into_record()
        .expect_err("empty provider capabilities");

    assert!(matches!(
        error,
        EnvironmentRegistryError::InvalidInput { .. }
    ));
}

#[test]
fn binding_records_require_matching_env_execution_target() {
    let mut record = binding("session_1", "local").into_record();
    record.exec_target = ToolExecutionTarget::new("env", "other");

    let error = record
        .validate()
        .expect_err("mismatched env execution target");

    assert!(matches!(
        error,
        EnvironmentRegistryError::InvalidInput { .. }
    ));
}

#[tokio::test]
async fn in_memory_store_registers_heartbeats_lists_and_deletes_providers() {
    let store = InMemoryEnvironmentRegistryStore::new();

    let registered = store
        .register_provider(provider_registration("bridge-local"))
        .await
        .expect("register provider");
    assert_eq!(registered.provider_id.as_str(), "bridge-local");
    assert_eq!(registered.status, EnvironmentProviderStatus::Online);

    let heartbeat = store
        .update_provider_heartbeat(EnvironmentProviderHeartbeat {
            provider_id: EnvironmentProviderId::new("bridge-local"),
            observed_at_ms: 40,
            lease_ttl_ms: Some(10_000),
            observed_targets: Vec::new(),
        })
        .await
        .expect("heartbeat");
    assert_eq!(heartbeat.last_seen_ms, 40);
    assert_eq!(heartbeat.lease_expires_ms, 10_040);

    let online = store
        .list_providers(ListEnvironmentProviders {
            status: Some(EnvironmentProviderStatus::Online),
            provider_kind: Some(EnvironmentProviderKind::Bridge),
        })
        .await
        .expect("list providers");
    assert_eq!(online, vec![heartbeat.clone()]);

    let deleted = store
        .delete_provider(&EnvironmentProviderId::new("bridge-local"))
        .await
        .expect("delete provider");
    assert_eq!(deleted, heartbeat);
}

#[tokio::test]
async fn in_memory_store_upserts_targets() {
    let store = InMemoryEnvironmentRegistryStore::new();

    let created = store
        .upsert_target(target("bridge-local", "local-host"))
        .await
        .expect("upsert target");
    assert_eq!(created.target_id.as_str(), "local-host");

    let ready = store
        .list_targets(ListEnvironmentTargets {
            provider_id: Some(EnvironmentProviderId::new("bridge-local")),
            status: Some(HostTargetStatus::Ready),
        })
        .await
        .expect("list targets");
    assert_eq!(ready, vec![created.clone()]);

    let stopped = store
        .update_target_status(UpdateEnvironmentTargetStatus {
            provider_id: EnvironmentProviderId::new("bridge-local"),
            target_id: HostTargetId::new("local-host"),
            status: HostTargetStatus::Stopped,
            observed_at_ms: 50,
        })
        .await
        .expect("update target status");
    assert_eq!(stopped.status, HostTargetStatus::Stopped);
}

#[tokio::test]
async fn in_memory_store_creates_lists_updates_and_deletes_bindings() {
    let store = InMemoryEnvironmentRegistryStore::new();

    let created = store
        .create_binding(binding("session_1", "local"))
        .await
        .expect("create binding");
    assert_eq!(
        created.exec_target,
        ToolExecutionTarget::new("env", "local")
    );

    let listed = store
        .list_bindings_for_session(&SessionId::new("session_1"))
        .await
        .expect("list bindings");
    assert_eq!(listed, vec![created.clone()]);

    let degraded = store
        .update_binding_status(UpdateSessionEnvironmentBindingStatus {
            session_id: SessionId::new("session_1"),
            env_id: EnvironmentId::new("local"),
            status: SessionEnvironmentBindingStatus::Degraded,
            updated_at_ms: 60,
        })
        .await
        .expect("update binding");
    assert_eq!(degraded.status, SessionEnvironmentBindingStatus::Degraded);

    let deleted = store
        .delete_binding(&SessionId::new("session_1"), &EnvironmentId::new("local"))
        .await
        .expect("delete binding");
    assert_eq!(deleted, degraded);
}

#[tokio::test]
async fn in_memory_store_creates_reads_lists_and_deletes_job_handles() {
    let store = InMemoryEnvironmentRegistryStore::new();

    let created = store
        .create_job_handles(vec![
            job_handle("session_1", "local", "job-1"),
            CreateJobHandle {
                job_id: JobId::new("job-2"),
                deck_id: Some("deck-2".to_owned()),
                name: Some("job-2".to_owned()),
                ..job_handle("session_1", "local", "job-2")
            },
        ])
        .await
        .expect("create job handles");
    assert_eq!(created.len(), 2);

    let read = store
        .read_job_handle(
            &SessionId::new("session_1"),
            &EnvironmentId::new("local"),
            &JobId::new("job-1"),
        )
        .await
        .expect("read job handle");
    assert_eq!(read.job_id.as_str(), "job-1");
    assert_eq!(read.start_request_hash, "hash-1");

    let deck_one = store
        .list_job_handles(ListJobHandles {
            session_id: SessionId::new("session_1"),
            env_id: Some(EnvironmentId::new("local")),
            deck_id: Some("deck-1".to_owned()),
            limit: Some(10),
        })
        .await
        .expect("list job handles");
    assert_eq!(deck_one.len(), 1);
    assert_eq!(deck_one[0].job_id.as_str(), "job-1");

    let limited = store
        .list_job_handles(ListJobHandles {
            session_id: SessionId::new("session_1"),
            env_id: None,
            deck_id: None,
            limit: Some(1),
        })
        .await
        .expect("list limited");
    assert_eq!(limited.len(), 1);

    let deleted = store
        .delete_job_handle(
            &SessionId::new("session_1"),
            &EnvironmentId::new("local"),
            &JobId::new("job-1"),
        )
        .await
        .expect("delete job handle");
    assert_eq!(deleted.job_id.as_str(), "job-1");
}

#[tokio::test]
async fn in_memory_job_handle_create_is_idempotent_for_same_hash() {
    let store = InMemoryEnvironmentRegistryStore::new();
    let first = store
        .create_job_handles(vec![job_handle("session_1", "local", "job-1")])
        .await
        .expect("first create");
    let second = store
        .create_job_handles(vec![job_handle("session_1", "local", "job-1")])
        .await
        .expect("second create");

    assert_eq!(second, first);
}

#[tokio::test]
async fn in_memory_job_handle_create_rejects_same_handle_with_different_hash() {
    let store = InMemoryEnvironmentRegistryStore::new();
    store
        .create_job_handles(vec![job_handle("session_1", "local", "job-1")])
        .await
        .expect("first create");

    let error = store
        .create_job_handles(vec![CreateJobHandle {
            start_request_hash: "different-hash".to_owned(),
            ..job_handle("session_1", "local", "job-1")
        }])
        .await
        .expect_err("conflicting create");

    assert!(matches!(
        error,
        EnvironmentRegistryError::AlreadyExists {
            kind: "job_handle",
            ..
        }
    ));
}

#[test]
fn job_handle_records_reject_execution_state_fields_by_absence_and_validate_ids() {
    let mut record = job_handle("session_1", "local", "job-1").into_record();
    record.job_id = JobId::new("not valid");

    let error = record.validate().expect_err("invalid job id");

    assert!(matches!(
        error,
        EnvironmentRegistryError::InvalidInput { .. }
    ));
}

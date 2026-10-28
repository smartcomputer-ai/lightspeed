use std::collections::BTreeMap;

use environment_protocol::{
    ids::ProviderTargetId,
    metadata::{DescriptiveText, MAX_METADATA_ENTRIES},
};
use uuid::Uuid;

use super::*;

const PROVIDER: &str = "incus-local";

fn endpoint(value: &str) -> EnvironmentEndpoint {
    EnvironmentEndpoint::parse(value).expect("endpoint")
}

fn target(value: &str) -> ProviderTargetId {
    ProviderTargetId::new(value).expect("target id")
}

fn provider() -> PutEnvironmentProvider {
    PutEnvironmentProvider {
        provider_id: EnvironmentProviderId::new(PROVIDER),
        display_name: Some("Local Incus".to_owned()),
        controller_endpoint: endpoint("ws://127.0.0.1:19090/control"),
        data_route_base: None,
        credential: ProviderCredentialChange::Set(SecretValue::new("provider-secret")),
        updated_at_ms: 1_000,
    }
}

fn binding(universe_id: Uuid, enabled: bool, at: i64) -> PutEnvironmentProviderBinding {
    PutEnvironmentProviderBinding {
        universe_id,
        provider_id: EnvironmentProviderId::new(PROVIDER),
        enabled,
        updated_at_ms: at,
    }
}

fn create(request: &str, environment: &str, at: i64) -> CreateEnvironment {
    CreateEnvironment {
        request_id: EnvironmentProvisionRequestId::new(request),
        environment_id: EnvironmentId::new(environment),
        provider_id: EnvironmentProviderId::new(PROVIDER),
        template_id: EnvironmentTemplateId::new("rust-v1"),
        idle: None,
        display_name: None,
        metadata: BTreeMap::new(),
        created_at_ms: at,
    }
}

fn external(request: &str, environment: &str, at: i64) -> CreateExternalEnvironment {
    CreateExternalEnvironment {
        request_id: EnvironmentProvisionRequestId::new(request),
        environment_id: EnvironmentId::new(environment),
        endpoint: endpoint("ws://envd.example:9000"),
        display_name: None,
        metadata: BTreeMap::new(),
        created_at_ms: at,
    }
}

fn build(version: &str) -> DaemonBuild {
    DaemonBuild {
        version: Some(version.to_owned()),
        git_sha: Some("aaaa".to_owned()),
        protocol_version: 4,
    }
}

fn observed(phase: TargetPhase, power: Option<PowerState>, at: i64) -> ObservedTarget {
    ObservedTarget {
        phase,
        power,
        message: None,
        observed_at_ms: at,
    }
}

fn observation(
    environment_id: &EnvironmentId,
    target_id: Option<&str>,
    target: ObservedTarget,
) -> RecordTargetObservation {
    RecordTargetObservation {
        environment_id: environment_id.clone(),
        provider_target_id: target_id.map(self::target),
        observed_at_ms: target.observed_at_ms,
        observed: target,
        public_endpoint: None,
    }
}

async fn store() -> (Uuid, InMemoryEnvironmentRegistryStore) {
    let universe_id = Uuid::new_v4();
    let store = InMemoryEnvironmentRegistryStore::for_universe(universe_id);
    store.put_provider(provider()).await.expect("provider");
    store
        .put_provider_binding(binding(universe_id, true, 1_000))
        .await
        .expect("binding");
    (universe_id, store)
}

async fn open_provisioned(
    store: &InMemoryEnvironmentRegistryStore,
    environment: &str,
) -> EnvironmentRecord {
    let created = store
        .create_environment(create(&format!("req-{environment}"), environment, 2_000))
        .await
        .expect("create");
    store
        .record_target_observation(observation(
            &created.environment_id,
            Some(&format!("target-{environment}")),
            observed(TargetPhase::Active, Some(PowerState::Running), 2_100),
        ))
        .await
        .expect("observe");
    store
        .open_environment(OpenEnvironment {
            environment_id: created.environment_id,
            daemon_build: build("0.5.0"),
            at_ms: 2_200,
        })
        .await
        .expect("open")
}

// --- providers and bindings --------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn provider_credentials_are_optional_and_follow_the_change() {
    let store = InMemoryEnvironmentRegistryStore::new();
    let provider_id = EnvironmentProviderId::new(PROVIDER);
    let mut bare = provider();
    bare.credential = ProviderCredentialChange::Keep;
    let created = store
        .put_provider(bare.clone())
        .await
        .expect("no credential");
    assert!(!created.has_credential);
    assert_eq!(
        store
            .read_provider_credential(&provider_id)
            .await
            .expect("read"),
        None
    );

    let mut unprintable = provider();
    unprintable.credential = ProviderCredentialChange::Set(SecretValue::new("two words"));
    assert!(matches!(
        store.put_provider(unprintable).await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));

    let set = store.put_provider(provider()).await.expect("set");
    assert!(set.has_credential);
    assert_eq!(set.created_at_ms, 1_000);
    bare.updated_at_ms = 2_000;
    let kept = store.put_provider(bare).await.expect("keep");
    assert!(kept.has_credential);
    assert_eq!(
        store
            .read_provider_credential(&provider_id)
            .await
            .expect("read")
            .expect("stored")
            .expose(),
        "provider-secret"
    );

    let mut clear = provider();
    clear.credential = ProviderCredentialChange::Clear;
    clear.updated_at_ms = 3_000;
    let cleared = store.put_provider(clear).await.expect("clear");
    assert!(!cleared.has_credential);
    assert_eq!(
        store
            .read_provider_credential(&provider_id)
            .await
            .expect("read"),
        None
    );
    assert!(matches!(
        store
            .read_provider_credential(&EnvironmentProviderId::new("missing"))
            .await,
        Err(EnvironmentRegistryError::NotFound { .. })
    ));
    let debug = format!("{:?}", provider());
    assert!(!debug.contains("provider-secret"));
}

#[test]
fn data_route_base_is_configured_or_derived_and_never_downgrades() {
    let derived = provider().into_record(None).expect("derived");
    assert_eq!(
        derived.effective_data_route_base().expect("base").as_str(),
        "ws://127.0.0.1:19090/routes"
    );

    let mut configured = provider();
    configured.data_route_base = Some(DataRouteBase::new("ws://relay.example:7000/data").unwrap());
    let configured = configured.into_record(None).expect("configured");
    assert_eq!(
        configured
            .effective_data_route_base()
            .expect("base")
            .as_str(),
        "ws://relay.example:7000/data"
    );

    let mut downgrade = provider();
    downgrade.controller_endpoint = endpoint("wss://incus.example.com/control");
    downgrade.data_route_base = Some(DataRouteBase::new("ws://incus.example.com/routes").unwrap());
    assert!(matches!(
        downgrade.into_record(None),
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));

    let mut unix = provider();
    unix.controller_endpoint = endpoint("unix:///run/incus-provider.sock");
    assert!(matches!(
        unix.clone().into_record(None),
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    unix.data_route_base = Some(DataRouteBase::new("ws://127.0.0.1:19091/routes").unwrap());
    unix.into_record(None).expect("unix with a configured base");
}

#[tokio::test(flavor = "current_thread")]
async fn bindings_are_one_per_provider_and_guard_deletes() {
    let (universe_id, store) = store().await;
    let provider_id = EnvironmentProviderId::new(PROVIDER);
    let replaced = store
        .put_provider_binding(binding(universe_id, false, 1_500))
        .await
        .expect("replace");
    assert!(!replaced.enabled);
    assert_eq!(replaced.created_at_ms, 1_000);
    assert_eq!(
        store
            .list_provider_bindings(universe_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        store
            .put_provider_binding(PutEnvironmentProviderBinding {
                provider_id: EnvironmentProviderId::new("missing"),
                ..binding(universe_id, true, 1_600)
            })
            .await,
        Err(EnvironmentRegistryError::NotFound { .. })
    ));

    // A disabled binding refuses creates; a missing one is not found.
    assert!(matches!(
        store
            .create_environment(create("r-1", "env-1", 2_000))
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    store
        .put_provider_binding(binding(universe_id, true, 1_700))
        .await
        .expect("enable");
    let environment = store
        .create_environment(create("r-1", "env-1", 2_000))
        .await
        .expect("create");

    // Live environments guard the binding and the provider.
    assert!(matches!(
        store
            .delete_provider_binding(universe_id, &provider_id)
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    assert!(matches!(
        store.delete_provider(&provider_id).await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: environment.environment_id.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 3_000,
        })
        .await
        .expect("begin close");
    store
        .finish_close_environment(FinishCloseEnvironment {
            environment_id: environment.environment_id,
            at_ms: 3_100,
        })
        .await
        .expect("close");
    store
        .delete_provider_binding(universe_id, &provider_id)
        .await
        .expect("delete binding");
    assert!(matches!(
        store
            .create_environment(create("r-2", "env-2", 4_000))
            .await,
        Err(EnvironmentRegistryError::NotFound { .. })
    ));
    store
        .delete_provider(&provider_id)
        .await
        .expect("delete provider");
}

// --- creates -----------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn request_ids_are_idempotent_only_for_identical_inputs() {
    let (_, store) = store().await;
    let first = store
        .create_environment(create("request-1", "environment-1", 2_000))
        .await
        .expect("first");
    assert_eq!(first.state, EnvironmentState::Provisioning);
    let source = first.provisioned().expect("provisioned");
    assert_eq!(source.next_attempt_at_ms, Some(2_000));
    assert_eq!(source.attempts, 0);

    // A retry with a fresh environment id and time converges.
    let retry = store
        .create_environment(create("request-1", "other-environment", 3_000))
        .await
        .expect("retry");
    assert_eq!(retry, first);

    let mut changed = create("request-1", "other-environment", 3_000);
    changed.idle = Some(IdlePolicy {
        pause_after_ms: Some(60_000),
        ..IdlePolicy::default()
    });
    assert!(matches!(
        store.create_environment(changed).await,
        Err(EnvironmentRegistryError::AlreadyExists { .. })
    ));
    // Another kind cannot claim the same request id.
    assert!(matches!(
        store
            .create_external_environment(external("request-1", "ext", 3_000))
            .await,
        Err(EnvironmentRegistryError::AlreadyExists { .. })
    ));
    assert!(matches!(
        store
            .create_environment(create("request-2", "environment-1", 3_000))
            .await,
        Err(EnvironmentRegistryError::AlreadyExists { .. })
    ));

    let external_row = store
        .create_external_environment(external("ext-1", "ext-env", 4_000))
        .await
        .expect("external");
    assert_eq!(external_row.state, EnvironmentState::Open);
    assert_eq!(
        store
            .create_external_environment(external("ext-1", "ext-env-2", 5_000))
            .await
            .expect("external retry"),
        external_row
    );
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_idle_overrides_are_refused() {
    let (_, store) = store().await;
    let mut request = create("request-1", "environment-1", 2_000);
    request.idle = Some(IdlePolicy {
        pause_after_ms: Some(30),
        stop_after_ms: Some(10),
        ..IdlePolicy::default()
    });
    assert!(matches!(
        store.create_environment(request).await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn external_environment_persists_its_endpoint() {
    let store = InMemoryEnvironmentRegistryStore::new();
    let environment = store
        .create_external_environment(CreateExternalEnvironment {
            endpoint: endpoint("unix:///run/lightspeed/envd.sock"),
            ..external("external-request", "external-environment", 1_000)
        })
        .await
        .expect("create external environment");
    let EnvironmentSource::External { endpoint } = &environment.source else {
        panic!("external source")
    };
    assert_eq!(endpoint.as_str(), "unix:///run/lightspeed/envd.sock");
    assert_eq!(
        environment.display_status(),
        EnvironmentDisplayStatus::Ready
    );
    assert_eq!(environment.use_gate(1_000), EnvironmentUse::Use);
}

// --- provisioned lifecycle ---------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn provisioning_opens_after_the_handshake_and_records_the_build() {
    let (_, store) = store().await;
    let created = store
        .create_environment(create("request-1", "environment-1", 2_000))
        .await
        .expect("create");
    let scheduled = store
        .schedule_environment_attempt(ScheduleEnvironmentAttempt {
            environment_id: created.environment_id.clone(),
            expected_state: EnvironmentState::Provisioning,
            next_attempt_at_ms: 4_000,
            attempts: 1,
            state_message: Some(format!("connect failed\u{7}{}", "x".repeat(2_000))),
            at_ms: 2_000,
        })
        .await
        .expect("schedule");
    let message = scheduled.state_message.as_deref().expect("message");
    assert!(message.len() <= STATE_MESSAGE_MAX_BYTES);
    assert!(!message.contains('\u{7}'));
    assert_eq!(
        scheduled.provisioned().unwrap().next_attempt_at_ms,
        Some(4_000)
    );

    let opened = open_provisioned(&store, "environment-2").await;
    assert_eq!(opened.state, EnvironmentState::Open);
    assert_eq!(opened.state_message, None);
    assert_eq!(opened.state_changed_at_ms, 2_200);
    assert_eq!(opened.daemon_build, Some(build("0.5.0")));
    let source = opened.provisioned().unwrap();
    assert_eq!(source.next_attempt_at_ms, None);
    assert_eq!(
        source.provider_target_id,
        Some(target("target-environment-2"))
    );

    // Opening twice, or scheduling an open row, is skipped.
    let again = store
        .open_environment(OpenEnvironment {
            environment_id: opened.environment_id.clone(),
            daemon_build: build("9.9.9"),
            at_ms: 3_000,
        })
        .await
        .expect("skipped");
    assert_eq!(again, opened);
    let unscheduled = store
        .schedule_environment_attempt(ScheduleEnvironmentAttempt {
            environment_id: opened.environment_id.clone(),
            expected_state: EnvironmentState::Provisioning,
            next_attempt_at_ms: 5_000,
            attempts: 2,
            state_message: None,
            at_ms: 3_000,
        })
        .await
        .expect("skipped");
    assert_eq!(unscheduled, opened);
}

#[tokio::test(flavor = "current_thread")]
async fn work_lists_due_rows_earliest_first() {
    let (_, store) = store().await;
    for (request, environment, at) in [("r-a", "env-a", 3_000), ("r-b", "env-b", 1_500)] {
        store
            .create_environment(create(request, environment, at))
            .await
            .expect("create");
    }
    store
        .schedule_environment_attempt(ScheduleEnvironmentAttempt {
            environment_id: EnvironmentId::new("env-a"),
            expected_state: EnvironmentState::Provisioning,
            next_attempt_at_ms: 2_500,
            attempts: 1,
            state_message: None,
            at_ms: 3_000,
        })
        .await
        .expect("schedule");
    let open = open_provisioned(&store, "env-open").await;

    let due = store.list_environment_work(2_600, 10).await.expect("work");
    let ids: Vec<&str> = due
        .iter()
        .map(|record| record.environment_id.as_str())
        .collect();
    assert_eq!(ids, ["env-b", "env-a"]);
    assert_eq!(
        store.list_environment_work(2_600, 1).await.unwrap().len(),
        1
    );
    assert!(
        store
            .list_environment_work(1_000, 10)
            .await
            .unwrap()
            .is_empty()
    );

    let open_rows = store.list_open_provisioned_environments().await.unwrap();
    assert_eq!(open_rows.len(), 1);
    assert_eq!(open_rows[0].environment_id, open.environment_id);
}

#[tokio::test(flavor = "current_thread")]
async fn failing_applies_to_provisioning_and_open_rows_only() {
    let (_, store) = store().await;
    let created = store
        .create_environment(create("request-1", "environment-1", 2_000))
        .await
        .expect("create");
    let failed = store
        .fail_environment(FailEnvironment {
            environment_id: created.environment_id.clone(),
            message: "protocol mismatch: local 4, remote 3".to_owned(),
            daemon_build: Some(DaemonBuild {
                version: None,
                git_sha: None,
                protocol_version: 3,
            }),
            at_ms: 3_000,
        })
        .await
        .expect("fail");
    assert_eq!(failed.state, EnvironmentState::Failed);
    assert_eq!(failed.provisioned().unwrap().next_attempt_at_ms, None);
    assert_eq!(failed.daemon_build.as_ref().unwrap().protocol_version, 3);
    assert_eq!(
        failed.use_gate(3_000),
        EnvironmentUse::Gone {
            reason: EnvironmentGoneReason::Failed,
            message: Some("protocol mismatch: local 4, remote 3".to_owned()),
        }
    );
    // A failed row cannot fail again; it can close.
    let again = store
        .fail_environment(FailEnvironment {
            environment_id: created.environment_id.clone(),
            message: "other".to_owned(),
            daemon_build: None,
            at_ms: 3_500,
        })
        .await
        .expect("skipped");
    assert_eq!(again, failed);
    let closing = store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: created.environment_id.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 4_000,
        })
        .await
        .expect("close failed row");
    assert_eq!(closing.state, EnvironmentState::Closing);
    assert_eq!(closing.state_message, None);

    let external_row = store
        .create_external_environment(external("ext", "ext-env", 2_000))
        .await
        .expect("external");
    assert!(matches!(
        store
            .fail_environment(FailEnvironment {
                environment_id: external_row.environment_id,
                message: "x".to_owned(),
                daemon_build: None,
                at_ms: 3_000,
            })
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn a_close_wins_over_work_that_was_in_flight() {
    let (_, store) = store().await;
    let created = store
        .create_environment(create("request-1", "environment-1", 2_000))
        .await
        .expect("create");
    let closing = store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: created.environment_id.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 2_500,
        })
        .await
        .expect("begin close");
    assert_eq!(closing.state, EnvironmentState::Closing);
    assert_eq!(
        closing.close_reason,
        Some(EnvironmentCloseReason::Requested)
    );
    assert_eq!(
        closing.provisioned().unwrap().next_attempt_at_ms,
        Some(2_500)
    );

    // The create that was in flight answers: the target id is recorded so
    // the close can address it, but the row stays closing.
    let observed = store
        .record_target_observation(observation(
            &created.environment_id,
            Some("ls-3f2a"),
            observed(TargetPhase::Active, Some(PowerState::Running), 3_000),
        ))
        .await
        .expect("observe");
    assert_eq!(observed.state, EnvironmentState::Closing);
    assert_eq!(
        observed.provisioned().unwrap().provider_target_id,
        Some(target("ls-3f2a"))
    );
    for skipped in [
        store
            .open_environment(OpenEnvironment {
                environment_id: created.environment_id.clone(),
                daemon_build: build("0.5.0"),
                at_ms: 3_100,
            })
            .await
            .expect("open skipped"),
        store
            .fail_environment(FailEnvironment {
                environment_id: created.environment_id.clone(),
                message: "late".to_owned(),
                daemon_build: None,
                at_ms: 3_200,
            })
            .await
            .expect("fail skipped"),
        store
            .begin_close_environment(BeginCloseEnvironment {
                environment_id: created.environment_id.clone(),
                reason: EnvironmentCloseReason::KeyRevoked,
                at_ms: 3_300,
            })
            .await
            .expect("close skipped"),
    ] {
        assert_eq!(skipped.state, EnvironmentState::Closing);
        assert_eq!(
            skipped.close_reason,
            Some(EnvironmentCloseReason::Requested)
        );
    }

    // The provision step that read the row while provisioning reschedules
    // it: skipped, so the close keeps its own schedule and message.
    let rescheduled = store
        .schedule_environment_attempt(ScheduleEnvironmentAttempt {
            environment_id: created.environment_id.clone(),
            expected_state: EnvironmentState::Provisioning,
            next_attempt_at_ms: 300_000,
            attempts: 3,
            state_message: Some("waiting for the provider".to_owned()),
            at_ms: 3_400,
        })
        .await
        .expect("schedule skipped");
    assert_eq!(rescheduled.state, EnvironmentState::Closing);
    assert_eq!(rescheduled.state_message, None);
    let source = rescheduled.provisioned().unwrap();
    assert_eq!(source.next_attempt_at_ms, Some(2_500));
    assert_eq!(source.attempts, 0);
    // The close step's own reschedule still applies.
    let close_retry = store
        .schedule_environment_attempt(ScheduleEnvironmentAttempt {
            environment_id: created.environment_id.clone(),
            expected_state: EnvironmentState::Closing,
            next_attempt_at_ms: 3_600,
            attempts: 1,
            state_message: None,
            at_ms: 3_500,
        })
        .await
        .expect("schedule close retry");
    assert_eq!(
        close_retry.provisioned().unwrap().next_attempt_at_ms,
        Some(3_600)
    );

    let closed = store
        .finish_close_environment(FinishCloseEnvironment {
            environment_id: created.environment_id.clone(),
            at_ms: 4_000,
        })
        .await
        .expect("finish");
    assert_eq!(closed.state, EnvironmentState::Closed);
    assert_eq!(closed.closed_at_ms, Some(4_000));
    assert_eq!(closed.close_reason, Some(EnvironmentCloseReason::Requested));
    assert_eq!(closed.provisioned().unwrap().next_attempt_at_ms, None);
    let finished_again = store
        .finish_close_environment(FinishCloseEnvironment {
            environment_id: created.environment_id,
            at_ms: 5_000,
        })
        .await
        .expect("skipped");
    assert_eq!(finished_again, closed);
}

#[tokio::test(flavor = "current_thread")]
async fn observations_keep_the_target_id_and_never_go_back_in_time() {
    let (_, store) = store().await;
    let created = store
        .create_environment(create("request-1", "environment-1", 2_000))
        .await
        .expect("create");
    let pending = store
        .record_target_observation(observation(
            &created.environment_id,
            None,
            observed(TargetPhase::Provisioning, None, 2_500),
        ))
        .await
        .expect("no id yet");
    assert_eq!(pending.provisioned().unwrap().provider_target_id, None);

    // Natural provider ids are opaque: slashes and colons are fine.
    let natural = "arn:aws:ec2:eu-west-1:123456789012:instance/i-0abc";
    store
        .record_target_observation(observation(
            &created.environment_id,
            Some(natural),
            observed(TargetPhase::Active, Some(PowerState::Running), 3_000),
        ))
        .await
        .expect("id assigned");
    let stale = store
        .record_target_observation(RecordTargetObservation {
            public_endpoint: Some("https://stale.example".to_owned()),
            ..observation(
                &created.environment_id,
                None,
                observed(TargetPhase::Provisioning, None, 2_800),
            )
        })
        .await
        .expect("stale");
    let source = stale.provisioned().unwrap();
    assert_eq!(source.provider_target_id, Some(target(natural)));
    assert_eq!(source.observed.as_ref().unwrap().phase, TargetPhase::Active);
    assert_eq!(source.public_endpoint, None);
    assert_eq!(stale.state, EnvironmentState::Provisioning);

    assert!(matches!(
        store
            .record_target_observation(observation(
                &created.environment_id,
                Some("another-target"),
                observed(TargetPhase::Active, None, 4_000),
            ))
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    // Power without the active phase is not a valid observation.
    assert!(matches!(
        store
            .record_target_observation(observation(
                &created.environment_id,
                None,
                observed(TargetPhase::Unknown, Some(PowerState::Paused), 4_000),
            ))
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    // Another live row of the provider cannot claim the same target.
    let other = store
        .create_environment(create("request-2", "environment-2", 2_000))
        .await
        .expect("other");
    assert!(matches!(
        store
            .record_target_observation(observation(
                &other.environment_id,
                Some(natural),
                observed(TargetPhase::Active, None, 4_000),
            ))
            .await,
        Err(EnvironmentRegistryError::AlreadyExists { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn the_provider_ending_a_target_closes_open_and_failed_rows() {
    let (_, store) = store().await;
    let open = open_provisioned(&store, "env-open").await;
    let ended = store
        .end_provisioned_environment(EndProvisionedEnvironment {
            environment_id: open.environment_id.clone(),
            at_ms: 5_000,
        })
        .await
        .expect("end");
    assert_eq!(ended.state, EnvironmentState::Closed);
    assert_eq!(
        ended.close_reason,
        Some(EnvironmentCloseReason::ProviderEnded)
    );
    assert_eq!(ended.closed_at_ms, Some(5_000));

    let provisioning = store
        .create_environment(create("r-p", "env-p", 2_000))
        .await
        .expect("create");
    let skipped = store
        .end_provisioned_environment(EndProvisionedEnvironment {
            environment_id: provisioning.environment_id.clone(),
            at_ms: 5_000,
        })
        .await
        .expect("skipped");
    assert_eq!(skipped, provisioning);
}

#[tokio::test(flavor = "current_thread")]
async fn a_closed_environment_keeps_no_public_endpoint() {
    let (_, store) = store().await;
    let with_endpoint =
        |environment_id: &EnvironmentId, target: ObservedTarget| RecordTargetObservation {
            public_endpoint: Some("https://env.example.test".to_owned()),
            ..observation(environment_id, None, target)
        };

    // The provider ending the target clears the endpoint.
    let open = open_provisioned(&store, "env-ended").await;
    let exposed = store
        .record_target_observation(with_endpoint(
            &open.environment_id,
            observed(TargetPhase::Active, Some(PowerState::Running), 4_000),
        ))
        .await
        .expect("observe");
    assert_eq!(
        exposed.provisioned().unwrap().public_endpoint.as_deref(),
        Some("https://env.example.test")
    );
    let ended = store
        .end_provisioned_environment(EndProvisionedEnvironment {
            environment_id: open.environment_id.clone(),
            at_ms: 5_000,
        })
        .await
        .expect("end");
    assert_eq!(ended.provisioned().unwrap().public_endpoint, None);
    // A late observation does not bring it back.
    let late = store
        .record_target_observation(with_endpoint(
            &open.environment_id,
            observed(TargetPhase::Closed, None, 6_000),
        ))
        .await
        .expect("late observation");
    assert_eq!(late.state, EnvironmentState::Closed);
    assert_eq!(late.provisioned().unwrap().public_endpoint, None);

    // So does finishing a requested close; closing keeps it until then.
    let created = store
        .create_environment(create("r-close", "env-close", 2_000))
        .await
        .expect("create");
    store
        .record_target_observation(RecordTargetObservation {
            provider_target_id: Some(target("ls-close")),
            ..with_endpoint(
                &created.environment_id,
                observed(TargetPhase::Active, Some(PowerState::Running), 3_000),
            )
        })
        .await
        .expect("observe");
    let closing = store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: created.environment_id.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 3_500,
        })
        .await
        .expect("begin close");
    assert!(closing.provisioned().unwrap().public_endpoint.is_some());
    let closed = store
        .finish_close_environment(FinishCloseEnvironment {
            environment_id: created.environment_id,
            at_ms: 4_000,
        })
        .await
        .expect("finish");
    assert_eq!(closed.provisioned().unwrap().public_endpoint, None);
}

// --- display status and the use gate ----------------------------------------

#[test]
fn display_status_refines_open_rows_by_observation() {
    let mut record = create("r", "e", 1_000).record().expect("record");
    assert_eq!(
        record.display_status(),
        EnvironmentDisplayStatus::Provisioning
    );
    record.state = EnvironmentState::Open;
    if let EnvironmentSource::Provisioned(source) = &mut record.source {
        source.next_attempt_at_ms = None;
    }
    assert_eq!(record.display_status(), EnvironmentDisplayStatus::Ready);
    for (phase, power, status) in [
        (TargetPhase::Active, None, EnvironmentDisplayStatus::Ready),
        (
            TargetPhase::Active,
            Some(PowerState::Running),
            EnvironmentDisplayStatus::Ready,
        ),
        (
            TargetPhase::Active,
            Some(PowerState::Paused),
            EnvironmentDisplayStatus::Paused,
        ),
        (
            TargetPhase::Active,
            Some(PowerState::Suspended),
            EnvironmentDisplayStatus::Suspended,
        ),
        (
            TargetPhase::Active,
            Some(PowerState::Stopped),
            EnvironmentDisplayStatus::Stopped,
        ),
        (
            TargetPhase::Provisioning,
            None,
            EnvironmentDisplayStatus::Provisioning,
        ),
        (
            TargetPhase::Closing,
            None,
            EnvironmentDisplayStatus::Closing,
        ),
        (
            TargetPhase::Unknown,
            None,
            EnvironmentDisplayStatus::Unknown,
        ),
        (TargetPhase::Failed, None, EnvironmentDisplayStatus::Unknown),
        (TargetPhase::Closed, None, EnvironmentDisplayStatus::Unknown),
    ] {
        if let EnvironmentSource::Provisioned(source) = &mut record.source {
            source.observed = Some(observed(phase, power, 1_500));
        }
        record.validate().expect("valid");
        assert_eq!(record.display_status(), status, "{phase:?} {power:?}");
        // An observation never blocks use: the dial decides.
        assert_eq!(record.use_gate(2_000), EnvironmentUse::Use);
    }
    for (state, status) in [
        (EnvironmentState::Closing, EnvironmentDisplayStatus::Closing),
        (EnvironmentState::Closed, EnvironmentDisplayStatus::Closed),
        (EnvironmentState::Failed, EnvironmentDisplayStatus::Failed),
    ] {
        record.state = state;
        assert_eq!(record.display_status(), status);
    }
}

#[test]
fn the_use_gate_covers_every_state() {
    let provisioning = create("r", "e", 1_000).record().expect("record");
    assert_eq!(
        provisioning.use_gate(1_000),
        EnvironmentUse::Wait {
            reason: EnvironmentWaitReason::Provisioning,
            retry_after_ms: WAIT_RETRY_AFTER_MS,
            until_ms: None,
        }
    );
    for (state, reason, message) in [
        (
            EnvironmentState::Closing,
            EnvironmentGoneReason::Closing,
            None,
        ),
        (
            EnvironmentState::Closed,
            EnvironmentGoneReason::Closed,
            None,
        ),
        (
            EnvironmentState::Failed,
            EnvironmentGoneReason::Failed,
            Some("broken".to_owned()),
        ),
    ] {
        let record = EnvironmentRecord {
            state,
            state_message: message.clone(),
            ..provisioning.clone()
        };
        assert_eq!(
            record.use_gate(1_000),
            EnvironmentUse::Gone { reason, message }
        );
    }
}

fn registered_record(mode: RegisteredIdentityMode, connected: bool) -> EnvironmentRecord {
    let key = mint_registration_key(
        EnvironmentRegistrationKeyId::new("rk"),
        registration_policy(mode),
        1_000,
    )
    .expect("mint")
    .record;
    let mut record = register("rk", "env", &daemon_key(0x51), 1_000)
        .record(&key)
        .expect("record");
    if let EnvironmentSource::Registered(source) = &mut record.source {
        source.connected = connected;
        source.connection_changed_at_ms = 10_000;
    }
    record
}

#[test]
fn a_disconnected_registered_daemon_is_waited_for_by_identity_mode() {
    let connected = registered_record(RegisteredIdentityMode::Persistent, true);
    assert_eq!(connected.use_gate(10_000), EnvironmentUse::Use);
    assert_eq!(connected.display_status(), EnvironmentDisplayStatus::Ready);

    let persistent = registered_record(RegisteredIdentityMode::Persistent, false);
    assert_eq!(persistent.display_status(), EnvironmentDisplayStatus::Away);
    let until = 10_000 + REGISTERED_RECONNECT_WAIT_MS;
    assert_eq!(
        persistent.use_gate(until - 1),
        EnvironmentUse::Wait {
            reason: EnvironmentWaitReason::Disconnected,
            retry_after_ms: WAIT_RETRY_AFTER_MS,
            until_ms: Some(until),
        }
    );
    assert_eq!(
        persistent.use_gate(until + 12_500),
        EnvironmentUse::Gone {
            reason: EnvironmentGoneReason::Away,
            message: Some("daemon away for 2m12s".to_owned()),
        }
    );

    let ephemeral = registered_record(RegisteredIdentityMode::Ephemeral, false);
    assert!(matches!(
        ephemeral.use_gate(10_001),
        EnvironmentUse::Gone {
            reason: EnvironmentGoneReason::Away,
            ..
        }
    ));
}

#[test]
fn away_spans_read_as_compact_durations() {
    for (ms, expected) in [
        (-5, "0s"),
        (999, "0s"),
        (45_000, "45s"),
        (120_000, "2m"),
        (192_000, "3m12s"),
        (3_600_000, "1h"),
        (3_900_000, "1h5m"),
    ] {
        assert_eq!(compact_duration(ms), expected);
    }
}

#[test]
fn route_refusals_map_to_wait_or_gone() {
    let failed = DataRouteRefusal::Failed {
        message: DescriptiveText::new("disk full").unwrap(),
    };
    for (refusal, expected) in [
        (
            DataRouteRefusal::Waking {},
            EnvironmentUse::Wait {
                reason: EnvironmentWaitReason::Waking,
                retry_after_ms: 5_000,
                until_ms: None,
            },
        ),
        (
            DataRouteRefusal::Booting {},
            EnvironmentUse::Wait {
                reason: EnvironmentWaitReason::Booting,
                retry_after_ms: 5_000,
                until_ms: None,
            },
        ),
        (
            DataRouteRefusal::Provisioning {},
            EnvironmentUse::Wait {
                reason: EnvironmentWaitReason::Provisioning,
                retry_after_ms: 5_000,
                until_ms: None,
            },
        ),
        (
            DataRouteRefusal::Disconnected {},
            EnvironmentUse::Wait {
                reason: EnvironmentWaitReason::Disconnected,
                retry_after_ms: 5_000,
                until_ms: None,
            },
        ),
        (
            DataRouteRefusal::Away {},
            EnvironmentUse::Gone {
                reason: EnvironmentGoneReason::Away,
                message: None,
            },
        ),
        (
            DataRouteRefusal::Gone {},
            EnvironmentUse::Gone {
                reason: EnvironmentGoneReason::Closed,
                message: None,
            },
        ),
        (
            failed,
            EnvironmentUse::Gone {
                reason: EnvironmentGoneReason::Failed,
                message: Some("disk full".to_owned()),
            },
        ),
    ] {
        assert_eq!(
            EnvironmentUse::from_route_refusal(&refusal, Some(5_000)),
            expected
        );
    }
    assert_eq!(
        EnvironmentUse::from_route_refusal(&DataRouteRefusal::Waking {}, None),
        EnvironmentUse::Wait {
            reason: EnvironmentWaitReason::Waking,
            retry_after_ms: WAIT_RETRY_AFTER_MS,
            until_ms: None,
        }
    );

    // A registered row that reads connected but has no control handle.
    let ephemeral = registered_record(RegisteredIdentityMode::Ephemeral, true);
    assert_eq!(
        ephemeral.bound_route_refusal(&DataRouteRefusal::Disconnected {}, None),
        EnvironmentUse::Gone {
            reason: EnvironmentGoneReason::Away,
            message: Some("daemon is not connected".to_owned()),
        }
    );
    let persistent = registered_record(RegisteredIdentityMode::Persistent, true);
    assert_eq!(
        persistent.bound_route_refusal(&DataRouteRefusal::Disconnected {}, Some(1_000)),
        EnvironmentUse::Wait {
            reason: EnvironmentWaitReason::Disconnected,
            retry_after_ms: 1_000,
            until_ms: None,
        }
    );
    assert_eq!(
        ephemeral.bound_route_refusal(&DataRouteRefusal::Booting {}, None),
        EnvironmentUse::from_route_refusal(&DataRouteRefusal::Booting {}, None)
    );
}

// --- registration keys -------------------------------------------------------

fn registration_policy(mode: RegisteredIdentityMode) -> RegistrationKeyPolicy {
    RegistrationKeyPolicy {
        display_name: "fleet".to_owned(),
        identity_mode: mode,
        max_active_environments: None,
        ephemeral_disconnect_grace_ms: None,
        offline_close_after_ms: None,
        expires_at_ms: None,
    }
}

/// The secret hash [`minted_key`] stores for key `id`, so [`register`] can
/// present it.
fn key_secret_hash(id: &str) -> String {
    registration_key_hash(&format!("{REGISTRATION_KEY_SECRET_PREFIX}test-{id}"))
}

async fn minted_key(
    store: &InMemoryEnvironmentRegistryStore,
    id: &str,
    policy: RegistrationKeyPolicy,
) -> MintedRegistrationKey {
    let mut minted =
        mint_registration_key(EnvironmentRegistrationKeyId::new(id), policy, 1_000).expect("mint");
    minted.secret.secret_hash = key_secret_hash(id);
    store
        .create_registration_key(CreateEnvironmentRegistrationKey {
            secret_hash: minted.secret.secret_hash.clone(),
            record: minted.record.clone(),
        })
        .await
        .expect("create key");
    minted
}

fn daemon_key(seed: u8) -> String {
    (0..32).map(|_| format!("{seed:02x}")).collect()
}

fn register(
    key: &str,
    environment: &str,
    public_key: &str,
    at: i64,
) -> CreateRegisteredEnvironment {
    CreateRegisteredEnvironment {
        registration_key_id: EnvironmentRegistrationKeyId::new(key),
        secret_hash: key_secret_hash(key),
        environment_id: EnvironmentId::new(environment),
        daemon_public_key: public_key.to_owned(),
        display_name: Some("worker".to_owned()),
        metadata: BTreeMap::new(),
        daemon_build: build("0.1.0"),
        created_at_ms: at,
    }
}

#[test]
fn daemon_ids_derive_from_the_public_key() {
    let public_key = [7u8; 32];
    let daemon_id = EnvironmentDaemonId::from_public_key(&public_key);
    assert!(daemon_id.as_str().starts_with(DAEMON_ID_PREFIX));
    assert_eq!(daemon_id.as_str().len(), DAEMON_ID_PREFIX.len() + 64);
    assert_eq!(daemon_id, EnvironmentDaemonId::from_public_key(&public_key));
    assert_ne!(daemon_id, EnvironmentDaemonId::from_public_key(&[8u8; 32]));
    assert!(validate_daemon_public_key(&daemon_key(0xab)).is_ok());
    assert!(validate_daemon_public_key("ABCD").is_err());

    let record = registered_record(RegisteredIdentityMode::Persistent, true);
    assert_eq!(
        record.registered().unwrap().daemon_id(),
        EnvironmentDaemonId::from_public_key(&[0x51u8; 32])
    );
}

#[test]
fn minted_registration_keys_return_the_secret_once_and_redact_debug() {
    let minted = mint_registration_key(
        EnvironmentRegistrationKeyId::new("rk-1"),
        registration_policy(RegisteredIdentityMode::Ephemeral),
        5,
    )
    .expect("mint");
    let secret = minted.secret.secret.expose().to_owned();
    assert!(secret.starts_with(REGISTRATION_KEY_SECRET_PREFIX));
    assert_eq!(minted.secret.secret_hash, registration_key_hash(&secret));
    assert_eq!(minted.record.key_prefix, minted.secret.key_prefix);
    assert_eq!(
        minted.record.key_prefix.len(),
        REGISTRATION_KEY_DISPLAY_PREFIX_LEN
    );
    assert!(secret.starts_with(&minted.record.key_prefix));
    assert_eq!(minted.record.updated_at_ms, 5);
    let debug = format!("{minted:?}");
    assert!(!debug.contains(&secret[REGISTRATION_KEY_DISPLAY_PREFIX_LEN..]));
    assert!(
        mint_registration_key(
            EnvironmentRegistrationKeyId::new("rk-2"),
            RegistrationKeyPolicy {
                display_name: " padded ".to_owned(),
                ..registration_policy(RegisteredIdentityMode::Persistent)
            },
            5,
        )
        .is_err()
    );
}

#[test]
fn offline_close_applies_only_to_persistent_keys() {
    let policy = |mode, offline| RegistrationKeyPolicy {
        offline_close_after_ms: offline,
        ..registration_policy(mode)
    };
    let id = || EnvironmentRegistrationKeyId::new("rk");
    mint_registration_key(
        id(),
        policy(RegisteredIdentityMode::Persistent, Some(60_000)),
        1,
    )
    .expect("persistent");
    for refused in [
        policy(RegisteredIdentityMode::Ephemeral, Some(60_000)),
        policy(RegisteredIdentityMode::Persistent, Some(0)),
    ] {
        assert!(matches!(
            mint_registration_key(id(), refused, 1),
            Err(EnvironmentRegistryError::InvalidInput { .. })
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn key_settings_replace_the_editable_policy() {
    let (_, store) = store().await;
    minted_key(
        &store,
        "rk",
        RegistrationKeyPolicy {
            max_active_environments: Some(3),
            ..registration_policy(RegisteredIdentityMode::Persistent)
        },
    )
    .await;
    let settings = RegistrationKeySettings {
        display_name: "renamed fleet".to_owned(),
        max_active_environments: None,
        ephemeral_disconnect_grace_ms: None,
        offline_close_after_ms: Some(3_600_000),
        expires_at_ms: Some(99_000),
    };
    let updated = store
        .update_registration_key(UpdateEnvironmentRegistrationKey {
            registration_key_id: EnvironmentRegistrationKeyId::new("rk"),
            settings: settings.clone(),
            updated_at_ms: 2_000,
        })
        .await
        .expect("update");
    assert_eq!(updated.display_name, "renamed fleet");
    assert_eq!(updated.max_active_environments, None);
    assert_eq!(updated.offline_close_after_ms, Some(3_600_000));
    assert_eq!(updated.identity_mode, RegisteredIdentityMode::Persistent);
    assert_eq!(updated.updated_at_ms, 2_000);

    let refused = store
        .update_registration_key(UpdateEnvironmentRegistrationKey {
            registration_key_id: EnvironmentRegistrationKeyId::new("rk"),
            settings: RegistrationKeySettings {
                display_name: String::new(),
                ..settings
            },
            updated_at_ms: 3_000,
        })
        .await;
    assert!(matches!(
        refused,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    assert_eq!(
        store
            .read_registration_key(&EnvironmentRegistrationKeyId::new("rk"))
            .await
            .unwrap(),
        updated
    );
}

#[tokio::test(flavor = "current_thread")]
async fn rotation_retires_the_live_secret_and_keeps_the_group() {
    let (_, store) = store().await;
    let minted = minted_key(
        &store,
        "rk",
        registration_policy(RegisteredIdentityMode::Persistent),
    )
    .await;
    let admitted = store
        .create_registered_environment(register("rk", "env-1", &daemon_key(0x61), 1_500))
        .await
        .expect("admit");

    let fresh = mint_registration_secret();
    let rotated = store
        .rotate_registration_key(RotateEnvironmentRegistrationKey {
            registration_key_id: EnvironmentRegistrationKeyId::new("rk"),
            secret_hash: fresh.secret_hash.clone(),
            key_prefix: fresh.key_prefix.clone(),
            rotated_at_ms: 2_000,
        })
        .await
        .expect("rotate");
    assert_eq!(rotated.key_prefix, fresh.key_prefix);
    assert_eq!(rotated.updated_at_ms, 2_000);
    assert!(
        store
            .resolve_registration_key(&minted.secret.secret_hash)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .resolve_registration_key(&fresh.secret_hash)
            .await
            .unwrap()
            .map(|key| key.registration_key_id),
        Some(EnvironmentRegistrationKeyId::new("rk"))
    );
    // The admitted daemon still belongs to the key's group.
    let usage = store
        .registration_key_usage(&EnvironmentRegistrationKeyId::new("rk"))
        .await
        .unwrap();
    assert_eq!(usage.active, 1);
    assert_eq!(
        store
            .read_environment(&admitted.environment_id)
            .await
            .unwrap()
            .registration_key_id(),
        Some(&EnvironmentRegistrationKeyId::new("rk"))
    );
    // An admission that resolved the key before the rotation committed
    // presents the retired secret and is refused; the live one admits.
    assert!(matches!(
        store
            .create_registered_environment(register("rk", "env-2", &daemon_key(0x62), 2_100))
            .await,
        Err(EnvironmentRegistryError::RegistrationKeyUnavailable {
            reason: "rotated",
            ..
        })
    ));
    let admitted_fresh = store
        .create_registered_environment(CreateRegisteredEnvironment {
            secret_hash: fresh.secret_hash.clone(),
            ..register("rk", "env-2", &daemon_key(0x62), 2_100)
        })
        .await
        .expect("admit with the live secret");
    assert_eq!(admitted_fresh.state, EnvironmentState::Open);

    // A retired secret cannot come back.
    assert!(matches!(
        store
            .rotate_registration_key(RotateEnvironmentRegistrationKey {
                registration_key_id: EnvironmentRegistrationKeyId::new("rk"),
                secret_hash: minted.secret.secret_hash.clone(),
                key_prefix: minted.secret.key_prefix.clone(),
                rotated_at_ms: 3_000,
            })
            .await,
        Err(EnvironmentRegistryError::AlreadyExists { .. })
    ));

    store
        .revoke_registration_key(RevokeEnvironmentRegistrationKey {
            registration_key_id: EnvironmentRegistrationKeyId::new("rk"),
            revoked_at_ms: 4_000,
        })
        .await
        .expect("revoke");
    let another = mint_registration_secret();
    assert!(matches!(
        store
            .rotate_registration_key(RotateEnvironmentRegistrationKey {
                registration_key_id: EnvironmentRegistrationKeyId::new("rk"),
                secret_hash: another.secret_hash,
                key_prefix: another.key_prefix,
                rotated_at_ms: 5_000,
            })
            .await,
        Err(EnvironmentRegistryError::RegistrationKeyUnavailable {
            reason: "revoked",
            ..
        })
    ));
}

// --- registered environments -------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn registered_admission_is_keyed_by_daemon_identity() {
    let (_, store) = store().await;
    let minted = minted_key(
        &store,
        "rk-eph",
        registration_policy(RegisteredIdentityMode::Ephemeral),
    )
    .await;
    assert_eq!(
        store
            .resolve_registration_key(&minted.secret.secret_hash)
            .await
            .expect("resolve")
            .map(|key| key.registration_key_id),
        Some(EnvironmentRegistrationKeyId::new("rk-eph"))
    );
    assert!(
        store
            .resolve_registration_key(&"0".repeat(64))
            .await
            .expect("resolve")
            .is_none()
    );

    let public_key = daemon_key(0x11);
    let created = store
        .create_registered_environment(register("rk-eph", "env-a", &public_key, 2_000))
        .await
        .expect("register");
    assert_eq!(created.state, EnvironmentState::Open);
    assert_eq!(created.request_id, None);
    assert_eq!(created.daemon_build, Some(build("0.1.0")));
    let source = created.registered().expect("registered");
    assert!(source.connected);
    assert_eq!(source.identity_mode, RegisteredIdentityMode::Ephemeral);
    assert_eq!(source.connection_changed_at_ms, 2_000);
    assert_eq!(source.last_seen_at_ms, 2_000);

    // A retried first registration converges on the same environment.
    let retried = store
        .create_registered_environment(register("rk-eph", "env-b", &public_key, 2_500))
        .await
        .expect("retry");
    assert_eq!(retried, created);
    assert_eq!(
        store
            .read_environment_by_daemon_public_key(&public_key)
            .await
            .expect("read")
            .map(|record| record.environment_id),
        Some(created.environment_id.clone())
    );

    let closed = store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: created.environment_id.clone(),
            reason: EnvironmentCloseReason::Departed,
            at_ms: 3_000,
        })
        .await
        .expect("close");
    assert_eq!(closed.state, EnvironmentState::Closed);
    assert_eq!(closed.close_reason, Some(EnvironmentCloseReason::Departed));
    assert_eq!(closed.closed_at_ms, Some(3_000));

    // The identity is spent: a reconnect is refused, a fresh admission
    // returns the closed row.
    assert!(matches!(
        store
            .mark_registered_connected(MarkRegisteredConnected {
                environment_id: created.environment_id.clone(),
                daemon_build: build("0.2.0"),
                at_ms: 4_000,
            })
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    let spent = store
        .create_registered_environment(register("rk-eph", "env-c", &public_key, 5_000))
        .await
        .expect("spent");
    assert_eq!(spent.environment_id, created.environment_id);
    assert_eq!(spent.state, EnvironmentState::Closed);
}

#[tokio::test(flavor = "current_thread")]
async fn connection_writes_happen_on_transitions_and_coarse_stamps() {
    let (_, store) = store().await;
    minted_key(
        &store,
        "rk",
        registration_policy(RegisteredIdentityMode::Persistent),
    )
    .await;
    let created = store
        .create_registered_environment(register("rk", "env", &daemon_key(0x71), 10_000))
        .await
        .expect("register");
    let id = created.environment_id.clone();
    let stamp = |at_ms| StampRegisteredSeen {
        environment_id: id.clone(),
        at_ms,
    };
    assert!(
        !store
            .stamp_registered_seen(stamp(10_000 + REGISTERED_LIVENESS_STAMP_MS - 1))
            .await
            .unwrap()
    );
    assert!(
        store
            .stamp_registered_seen(stamp(10_000 + REGISTERED_LIVENESS_STAMP_MS))
            .await
            .unwrap()
    );

    let away = store
        .mark_registered_disconnected(MarkRegisteredDisconnected {
            environment_id: id.clone(),
            at_ms: 400_000,
        })
        .await
        .expect("disconnect");
    let source = away.registered().unwrap();
    assert!(!source.connected);
    assert_eq!(source.connection_changed_at_ms, 400_000);
    assert_eq!(
        source.last_seen_at_ms,
        10_000 + REGISTERED_LIVENESS_STAMP_MS
    );
    // A superseded connection's late disconnect writes nothing, and a
    // disconnected row takes no stamps.
    let again = store
        .mark_registered_disconnected(MarkRegisteredDisconnected {
            environment_id: id.clone(),
            at_ms: 500_000,
        })
        .await
        .expect("skipped");
    assert_eq!(again, away);
    assert!(
        !store
            .stamp_registered_seen(stamp(10_000_000))
            .await
            .unwrap()
    );

    let back = store
        .mark_registered_connected(MarkRegisteredConnected {
            environment_id: id.clone(),
            daemon_build: build("0.2.0"),
            at_ms: 600_000,
        })
        .await
        .expect("reconnect");
    let source = back.registered().unwrap();
    assert!(source.connected);
    assert_eq!(source.connection_changed_at_ms, 600_000);
    assert_eq!(source.last_seen_at_ms, 600_000);
    assert_eq!(back.daemon_build, Some(build("0.2.0")));

    assert_eq!(
        store.repair_registered_connections(700_000).await.unwrap(),
        1
    );
    let repaired = store.read_environment(&id).await.unwrap();
    let source = repaired.registered().unwrap();
    assert!(!source.connected);
    assert_eq!(source.connection_changed_at_ms, 700_000);
    assert_eq!(source.last_seen_at_ms, 600_000);
    assert_eq!(
        store.repair_registered_connections(800_000).await.unwrap(),
        0
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_registered_sweep_disconnects_stale_rows_and_closes_by_key_policy() {
    let (_, store) = store().await;
    minted_key(
        &store,
        "rk-eph",
        RegistrationKeyPolicy {
            ephemeral_disconnect_grace_ms: Some(60_000),
            ..registration_policy(RegisteredIdentityMode::Ephemeral)
        },
    )
    .await;
    minted_key(
        &store,
        "rk-off",
        RegistrationKeyPolicy {
            offline_close_after_ms: Some(600_000),
            ..registration_policy(RegisteredIdentityMode::Persistent)
        },
    )
    .await;
    minted_key(
        &store,
        "rk-keep",
        registration_policy(RegisteredIdentityMode::Persistent),
    )
    .await;
    let start = 1_000_000;
    for (key, environment, seed) in [
        ("rk-eph", "eph-stale", 0x81),
        ("rk-eph", "eph-away", 0x82),
        ("rk-off", "off-away", 0x83),
        ("rk-keep", "keep-away", 0x84),
        ("rk-keep", "keep-live", 0x85),
    ] {
        store
            .create_registered_environment(register(key, environment, &daemon_key(seed), start))
            .await
            .expect("register");
    }
    for environment in ["eph-away", "off-away", "keep-away"] {
        store
            .mark_registered_disconnected(MarkRegisteredDisconnected {
                environment_id: EnvironmentId::new(environment),
                at_ms: start,
            })
            .await
            .expect("disconnect");
    }
    // keep-live keeps stamping; eph-stale's gateway died silently.
    let now = start + REGISTERED_STALE_AFTER_MS + 1;
    store
        .stamp_registered_seen(StampRegisteredSeen {
            environment_id: EnvironmentId::new("keep-live"),
            at_ms: now - 1_000,
        })
        .await
        .expect("stamp");

    let sweep = store
        .sweep_registered_environments(now)
        .await
        .expect("sweep");
    assert_eq!(sweep.disconnected, [EnvironmentId::new("eph-stale")]);
    assert_eq!(
        sweep.closed,
        [
            (
                EnvironmentId::new("eph-away"),
                EnvironmentCloseReason::EphemeralGrace
            ),
            (
                EnvironmentId::new("eph-stale"),
                EnvironmentCloseReason::EphemeralGrace
            ),
            (
                EnvironmentId::new("off-away"),
                EnvironmentCloseReason::OfflineTimeout
            ),
        ]
    );
    let stale = store
        .read_environment(&EnvironmentId::new("eph-stale"))
        .await
        .unwrap();
    assert_eq!(stale.state, EnvironmentState::Closed);
    assert_eq!(stale.registered().unwrap().connection_changed_at_ms, start);
    for open in ["keep-away", "keep-live"] {
        assert_eq!(
            store
                .read_environment(&EnvironmentId::new(open))
                .await
                .unwrap()
                .state,
            EnvironmentState::Open
        );
    }
    let quiet = store
        .sweep_registered_environments(now)
        .await
        .expect("sweep");
    assert_eq!(quiet, RegisteredSweep::default());
}

#[tokio::test(flavor = "current_thread")]
async fn registration_key_policy_gates_admission_without_touching_reconnects() {
    let (_, store) = store().await;
    minted_key(
        &store,
        "rk-cap",
        RegistrationKeyPolicy {
            max_active_environments: Some(1),
            ..registration_policy(RegisteredIdentityMode::Persistent)
        },
    )
    .await;
    let first = store
        .create_registered_environment(register("rk-cap", "env-1", &daemon_key(0x21), 2_000))
        .await
        .expect("first");
    let refused = store
        .create_registered_environment(register("rk-cap", "env-2", &daemon_key(0x22), 2_100))
        .await
        .expect_err("capacity");
    assert!(matches!(
        refused,
        EnvironmentRegistryError::RegistrationCapacityExhausted { limit: 1, .. }
    ));
    assert!(
        store
            .read_environment(&EnvironmentId::new("env-2"))
            .await
            .is_err()
    );

    store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: first.environment_id.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 3_000,
        })
        .await
        .expect("close");
    store
        .create_registered_environment(register("rk-cap", "env-2", &daemon_key(0x22), 3_200))
        .await
        .expect("capacity freed by close");

    let usage = store
        .registration_key_usage(&EnvironmentRegistrationKeyId::new("rk-cap"))
        .await
        .expect("usage");
    assert_eq!(usage.registered, 2);
    assert_eq!(usage.active, 1);
    assert_eq!(usage.last_registered_at_ms, Some(3_200));

    let revoked = store
        .revoke_registration_key(RevokeEnvironmentRegistrationKey {
            registration_key_id: EnvironmentRegistrationKeyId::new("rk-cap"),
            revoked_at_ms: 4_000,
        })
        .await
        .expect("revoke");
    assert_eq!(revoked.revoked_at_ms, Some(4_000));
    assert_eq!(
        revoked.status(4_500),
        EnvironmentRegistrationKeyStatus::Revoked
    );
    let again = store
        .revoke_registration_key(RevokeEnvironmentRegistrationKey {
            registration_key_id: EnvironmentRegistrationKeyId::new("rk-cap"),
            revoked_at_ms: 9_000,
        })
        .await
        .expect("revoke again");
    assert_eq!(again.revoked_at_ms, Some(4_000));
    assert!(matches!(
        store
            .create_registered_environment(register("rk-cap", "env-3", &daemon_key(0x23), 5_000))
            .await,
        Err(EnvironmentRegistryError::RegistrationKeyUnavailable {
            reason: "revoked",
            ..
        })
    ));
    // Known daemons never consult the key.
    store
        .mark_registered_disconnected(MarkRegisteredDisconnected {
            environment_id: EnvironmentId::new("env-2"),
            at_ms: 5_400,
        })
        .await
        .expect("disconnect");
    let reconnect = store
        .mark_registered_connected(MarkRegisteredConnected {
            environment_id: EnvironmentId::new("env-2"),
            daemon_build: build("0.1.1"),
            at_ms: 5_500,
        })
        .await
        .expect("reconnect after revoke");
    assert!(reconnect.registered().unwrap().connected);

    minted_key(
        &store,
        "rk-exp",
        RegistrationKeyPolicy {
            expires_at_ms: Some(1_500),
            ..registration_policy(RegisteredIdentityMode::Ephemeral)
        },
    )
    .await;
    assert!(matches!(
        store
            .create_registered_environment(register("rk-exp", "env-4", &daemon_key(0x24), 2_000))
            .await,
        Err(EnvironmentRegistryError::RegistrationKeyUnavailable {
            reason: "expired",
            ..
        })
    ));
    assert_eq!(store.list_registration_keys().await.unwrap().len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn metadata_is_caller_only_and_bounded() {
    let (_, store) = store().await;
    minted_key(
        &store,
        "rk",
        registration_policy(RegisteredIdentityMode::Persistent),
    )
    .await;
    let mut full = register("rk", "env-full", &daemon_key(0x31), 1_000);
    full.metadata = (0..MAX_METADATA_ENTRIES as usize)
        .map(|index| (format!("caller.key{index}"), "v".to_owned()))
        .collect();
    store
        .create_registered_environment(full)
        .await
        .expect("full caller allowance");

    let mut over = register("rk", "env-over", &daemon_key(0x32), 1_000);
    over.metadata = (0..=MAX_METADATA_ENTRIES as usize)
        .map(|index| (format!("caller.key{index}"), "v".to_owned()))
        .collect();
    assert!(matches!(
        store.create_registered_environment(over).await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    let mut reserved = register("rk", "env-reserved", &daemon_key(0x33), 1_000);
    reserved.metadata = BTreeMap::from([("lightspeed.envd.version".to_owned(), "1".to_owned())]);
    assert!(matches!(
        store.create_registered_environment(reserved).await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
}

// --- updates and listing -----------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn updates_replace_name_and_metadata_until_closed() {
    let store = InMemoryEnvironmentRegistryStore::new();
    let created = store
        .create_external_environment(external("ext", "ext-env", 1_000))
        .await
        .expect("external");
    let update = |name: Option<&str>, at| UpdateEnvironment {
        environment_id: created.environment_id.clone(),
        display_name: name.map(str::to_owned),
        metadata: BTreeMap::from([("team".to_owned(), "infra".to_owned())]),
        updated_at_ms: at,
    };
    let updated = store
        .update_environment(update(Some("builder"), 2_000))
        .await
        .expect("update");
    assert_eq!(updated.display_name.as_deref(), Some("builder"));
    assert_eq!(
        updated.metadata.get("team").map(String::as_str),
        Some("infra")
    );
    assert_eq!(updated.updated_at_ms, 2_000);

    // A refused change leaves the stored record untouched.
    assert!(matches!(
        store.update_environment(update(Some(""), 2_500)).await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    assert_eq!(
        store
            .read_environment(&created.environment_id)
            .await
            .unwrap(),
        updated
    );

    store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: created.environment_id.clone(),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 3_000,
        })
        .await
        .expect("close");
    assert!(matches!(
        store.update_environment(update(None, 4_000)).await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn listing_pages_by_id_and_hides_closed_rows_by_default() {
    let (_, store) = store().await;
    for index in 0..5 {
        let mut request = create(&format!("r-{index}"), &format!("env-{index}"), 2_000);
        if index % 2 == 0 {
            request.metadata = BTreeMap::from([("job".to_owned(), "nightly".to_owned())]);
        }
        store.create_environment(request).await.expect("create");
    }
    store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: EnvironmentId::new("env-1"),
            reason: EnvironmentCloseReason::Requested,
            at_ms: 3_000,
        })
        .await
        .expect("close");
    store
        .finish_close_environment(FinishCloseEnvironment {
            environment_id: EnvironmentId::new("env-1"),
            at_ms: 3_100,
        })
        .await
        .expect("closed");

    let ids = |page: &EnvironmentPage| -> Vec<String> {
        page.environments
            .iter()
            .map(|record| record.environment_id.to_string())
            .collect()
    };
    let first = store
        .list_environments(ListEnvironments {
            limit: 2,
            ..ListEnvironments::default()
        })
        .await
        .expect("page 1");
    assert_eq!(ids(&first), ["env-0", "env-2"]);
    assert_eq!(first.next_after, Some(EnvironmentId::new("env-2")));
    let second = store
        .list_environments(ListEnvironments {
            limit: 2,
            after: first.next_after.clone(),
            ..ListEnvironments::default()
        })
        .await
        .expect("page 2");
    assert_eq!(ids(&second), ["env-3", "env-4"]);
    assert_eq!(second.next_after, None);

    let with_closed = store
        .list_environments(ListEnvironments {
            include_closed: true,
            ..ListEnvironments::default()
        })
        .await
        .expect("all");
    assert_eq!(with_closed.environments.len(), 5);
    let only_closed = store
        .list_environments(ListEnvironments {
            state: Some(EnvironmentState::Closed),
            ..ListEnvironments::default()
        })
        .await
        .expect("closed");
    assert_eq!(ids(&only_closed), ["env-1"]);

    let tagged = store
        .list_environments(ListEnvironments {
            metadata: BTreeMap::from([("job".to_owned(), "nightly".to_owned())]),
            ..ListEnvironments::default()
        })
        .await
        .expect("metadata");
    assert_eq!(ids(&tagged), ["env-0", "env-2", "env-4"]);
    let mismatched = store
        .list_environments(ListEnvironments {
            metadata: BTreeMap::from([
                ("job".to_owned(), "nightly".to_owned()),
                ("trial".to_owned(), "1".to_owned()),
            ]),
            ..ListEnvironments::default()
        })
        .await
        .expect("mismatched");
    assert!(mismatched.environments.is_empty());

    for limit in [0, LIST_ENVIRONMENTS_MAX_LIMIT + 1] {
        assert!(matches!(
            store
                .list_environments(ListEnvironments {
                    limit,
                    ..ListEnvironments::default()
                })
                .await,
            Err(EnvironmentRegistryError::InvalidInput { .. })
        ));
    }
    assert_eq!(
        ListEnvironments::default().limit,
        LIST_ENVIRONMENTS_DEFAULT_LIMIT
    );
}

#[tokio::test(flavor = "current_thread")]
async fn registered_environments_list_by_key() {
    let (_, store) = store().await;
    for key in ["rk-a", "rk-b"] {
        minted_key(
            &store,
            key,
            registration_policy(RegisteredIdentityMode::Persistent),
        )
        .await;
    }
    let a = store
        .create_registered_environment(register("rk-a", "env-a", &daemon_key(0x31), 2_000))
        .await
        .expect("a");
    store
        .create_registered_environment(register("rk-b", "env-b", &daemon_key(0x32), 2_000))
        .await
        .expect("b");
    let by_key = store
        .list_environments(ListEnvironments {
            registration_key_id: Some(EnvironmentRegistrationKeyId::new("rk-a")),
            ..ListEnvironments::default()
        })
        .await
        .expect("list");
    assert_eq!(by_key.environments.len(), 1);
    assert_eq!(by_key.environments[0].environment_id, a.environment_id);
}

#[test]
fn states_and_close_reasons_round_trip_their_stored_names() {
    for state in [
        EnvironmentState::Provisioning,
        EnvironmentState::Open,
        EnvironmentState::Closing,
        EnvironmentState::Closed,
        EnvironmentState::Failed,
    ] {
        assert_eq!(EnvironmentState::parse(state.as_str()), Some(state));
        assert_eq!(
            serde_json::to_value(state).unwrap(),
            serde_json::json!(state.as_str())
        );
    }
    for reason in [
        EnvironmentCloseReason::Requested,
        EnvironmentCloseReason::KeyRevoked,
        EnvironmentCloseReason::EphemeralGrace,
        EnvironmentCloseReason::Departed,
        EnvironmentCloseReason::OfflineTimeout,
        EnvironmentCloseReason::ProviderEnded,
    ] {
        assert_eq!(EnvironmentCloseReason::parse(reason.as_str()), Some(reason));
        assert_eq!(
            serde_json::to_value(reason).unwrap(),
            serde_json::json!(reason.as_str())
        );
    }
    assert_eq!(EnvironmentState::parse("ready"), None);
}

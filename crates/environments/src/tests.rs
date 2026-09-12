use std::collections::BTreeMap;

use environment_protocol::shared::{EnvironmentTransport, ProviderTargetId};
use uuid::Uuid;

use super::*;

fn provider() -> PutEnvironmentProvider {
    PutEnvironmentProvider {
        provider_id: EnvironmentProviderId::new("incus-local"),
        display_name: Some("Local Incus".to_owned()),
        controller_connection: EnvironmentConnectionSpec::new(
            "ws://127.0.0.1:19090/control",
            EnvironmentTransport::WebSocket,
        ),
        metadata: BTreeMap::new(),
        updated_at_ms: 1_000,
    }
}

fn binding(universe_id: Uuid, expected_revision: Option<u64>) -> PutEnvironmentProviderBinding {
    PutEnvironmentProviderBinding {
        universe_id,
        binding_id: EnvironmentProviderBindingId::new("primary"),
        provider_id: EnvironmentProviderId::new("incus-local"),
        status: EnvironmentProviderBindingStatus::Enabled,
        metadata: BTreeMap::new(),
        expected_revision,
        updated_at_ms: 1_000 + expected_revision.unwrap_or(0) as i64,
    }
}

fn create(request: &str, environment: &str, incarnation: &str, at: i64) -> CreateEnvironment {
    CreateEnvironment {
        request_id: EnvironmentProvisionRequestId::new(request),
        environment_id: EnvironmentId::new(environment),
        incarnation_id: EnvironmentIncarnationId::new(incarnation),
        binding_id: EnvironmentProviderBindingId::new("primary"),
        template_id: EnvironmentTemplateId::new("rust-v1"),
        display_name: None,
        metadata: BTreeMap::new(),

        idle_policy: None,
        created_at_ms: at,
    }
}

async fn store() -> (Uuid, InMemoryEnvironmentRegistryStore) {
    let universe_id = Uuid::new_v4();
    let store = InMemoryEnvironmentRegistryStore::for_universe(universe_id);
    store.put_provider(provider()).await.expect("provider");
    store
        .put_provider_binding(binding(universe_id, None))
        .await
        .expect("binding");
    (universe_id, store)
}

#[tokio::test(flavor = "current_thread")]
async fn binding_put_is_revisioned_and_unique_per_provider() {
    let (universe_id, store) = store().await;
    let first = store
        .read_provider_binding(universe_id, &EnvironmentProviderBindingId::new("primary"))
        .await
        .expect("read");
    assert_eq!(first.revision, 1);

    let conflict = store
        .put_provider_binding(binding(universe_id, None))
        .await
        .expect_err("stale write");
    assert!(matches!(
        conflict,
        EnvironmentRegistryError::RevisionConflict {
            actual: Some(1),
            ..
        }
    ));

    let second = store
        .put_provider_binding(binding(universe_id, Some(1)))
        .await
        .expect("replace");
    assert_eq!(second.revision, 2);

    let mut duplicate = binding(universe_id, None);
    duplicate.binding_id = EnvironmentProviderBindingId::new("secondary");
    assert!(matches!(
        store.put_provider_binding(duplicate).await,
        Err(EnvironmentRegistryError::AlreadyExists { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn provider_delete_requires_all_bindings_to_be_removed() {
    let (universe_id, store) = store().await;
    let provider_id = EnvironmentProviderId::new("incus-local");
    assert!(matches!(
        store.delete_provider(&provider_id).await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    store
        .delete_provider_binding(universe_id, &EnvironmentProviderBindingId::new("primary"))
        .await
        .expect("delete binding");
    let deleted = store
        .delete_provider(&provider_id)
        .await
        .expect("delete provider");
    assert_eq!(deleted.provider_id, provider_id);
    assert!(matches!(
        store.read_provider(&provider_id).await,
        Err(EnvironmentRegistryError::NotFound { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn stable_request_id_returns_the_original_environment() {
    let (_, store) = store().await;
    let first = store
        .create_environment(create("request-1", "environment-1", "incarnation-1", 2_000))
        .await
        .expect("first");
    let retry = store
        .create_environment(create(
            "request-1",
            "different-environment",
            "different-incarnation",
            3_000,
        ))
        .await
        .expect("retry");
    assert_eq!(retry.environment_id, first.environment_id);
    assert_eq!(
        retry.incarnation.incarnation_id,
        first.incarnation.incarnation_id
    );
    assert_eq!(
        store
            .list_environments(ListEnvironments::default())
            .await
            .expect("list")
            .len(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn adoption_creates_a_provisioned_environment_without_a_template() {
    let (_, store) = store().await;
    let adopted = store
        .adopt_environment(AdoptEnvironment {
            request_id: EnvironmentProvisionRequestId::new("adopt-request-1"),
            environment_id: EnvironmentId::new("environment-adopted"),
            incarnation_id: EnvironmentIncarnationId::new("incarnation-adopted"),
            binding_id: EnvironmentProviderBindingId::new("primary"),
            source_target: "legacy/hand-built-vm".to_owned(),
            display_name: Some("Hand-built VM".to_owned()),
            metadata: BTreeMap::new(),
            created_at_ms: 2_000,
        })
        .await
        .expect("adopt");

    assert_eq!(adopted.status, EnvironmentStatus::Provisioning);
    assert!(matches!(
        adopted.source,
        EnvironmentSource::Provisioned { .. }
    ));
    assert_eq!(adopted.incarnation.template_id, None);
    assert_eq!(
        adopted.incarnation.adoption_source_target.as_deref(),
        Some("legacy/hand-built-vm")
    );

    let retry = store
        .adopt_environment(AdoptEnvironment {
            request_id: EnvironmentProvisionRequestId::new("adopt-request-1"),
            environment_id: EnvironmentId::new("ignored-on-retry"),
            incarnation_id: EnvironmentIncarnationId::new("ignored-on-retry"),
            binding_id: EnvironmentProviderBindingId::new("primary"),
            source_target: "legacy/another-vm".to_owned(),
            display_name: None,
            metadata: BTreeMap::new(),
            created_at_ms: 3_000,
        })
        .await
        .expect("idempotent retry");
    assert_eq!(retry.environment_id, adopted.environment_id);
}

#[tokio::test(flavor = "current_thread")]
async fn lightspeed_does_not_enforce_provider_quota() {
    let (_, store) = store().await;
    let first = store
        .create_environment(create("request-1", "environment-1", "incarnation-1", 2_000))
        .await
        .expect("first");
    store
        .fail_environment_lifecycle(FailEnvironmentLifecycle {
            environment_id: first.environment_id.clone(),
            message: "provider failed".to_owned(),
            observed_at_ms: 3_000,
        })
        .await
        .expect("fail");
    store
        .create_environment(create("request-2", "environment-2", "incarnation-2", 4_000))
        .await
        .expect("second intent");
}

#[tokio::test(flavor = "current_thread")]
async fn provider_observation_populates_only_the_current_incarnation() {
    let (_, store) = store().await;
    let environment = store
        .create_environment(create("request-1", "environment-1", "incarnation-1", 2_000))
        .await
        .expect("create");
    let target_id = ProviderTargetId::new("target-1");
    let observed = store
        .observe_provisioned_environment(ObserveProvisionedEnvironment {
            environment_id: environment.environment_id.clone(),
            provider_target_id: target_id.clone(),
            status: EnvironmentStatus::Ready,
            power_states: vec![PowerState::Running, PowerState::Paused],
            observed_at_ms: 3_000,
        })
        .await
        .expect("observe");
    assert_eq!(observed.status, EnvironmentStatus::Ready);
    assert_eq!(observed.incarnation.provider_target_id, Some(target_id));
    assert_eq!(
        observed.incarnation.power_states,
        vec![PowerState::Running, PowerState::Paused]
    );
    assert_eq!(
        observed.incarnation.template_id,
        Some(EnvironmentTemplateId::new("rust-v1"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn public_ingress_is_a_realized_environment_facet() {
    let (_, store) = store().await;
    let environment = store
        .create_environment(create("request-1", "environment-1", "incarnation-1", 2_000))
        .await
        .expect("create");
    let enabled = store
        .set_environment_ingress(SetEnvironmentIngress {
            environment_id: environment.environment_id.clone(),
            enabled: true,
            public_endpoint: Some("https://opaque.env.example".to_owned()),
            updated_at_ms: 3_000,
        })
        .await
        .expect("enable ingress");
    assert!(enabled.public_ingress_enabled);
    assert_eq!(
        enabled.public_endpoint.as_deref(),
        Some("https://opaque.env.example")
    );
    let disabled = store
        .set_environment_ingress(SetEnvironmentIngress {
            environment_id: environment.environment_id,
            enabled: false,
            public_endpoint: None,
            updated_at_ms: 4_000,
        })
        .await
        .expect("disable ingress");
    assert!(!disabled.public_ingress_enabled);
    assert_eq!(disabled.public_endpoint, None);
}

#[tokio::test(flavor = "current_thread")]
async fn external_environment_cannot_enable_provider_managed_ingress() {
    let store = InMemoryEnvironmentRegistryStore::new();
    let environment = store
        .create_external_environment(CreateExternalEnvironment {
            request_id: EnvironmentProvisionRequestId::new("external-request"),
            environment_id: EnvironmentId::new("external-environment"),
            incarnation_id: EnvironmentIncarnationId::new("external-incarnation"),
            connection: EnvironmentConnectionSpec::new(
                "ws://envd.example",
                EnvironmentTransport::WebSocket,
            ),
            display_name: None,
            metadata: BTreeMap::new(),
            created_at_ms: 1_000,
        })
        .await
        .expect("external");
    assert!(matches!(
        store
            .set_environment_ingress(SetEnvironmentIngress {
                environment_id: environment.environment_id,
                enabled: true,
                public_endpoint: Some("https://invalid.example".to_owned()),
                updated_at_ms: 2_000,
            })
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn external_environment_persists_a_typed_connection() {
    let store = InMemoryEnvironmentRegistryStore::new();
    let environment = store
        .create_external_environment(CreateExternalEnvironment {
            request_id: EnvironmentProvisionRequestId::new("external-request"),
            environment_id: EnvironmentId::new("external-environment"),
            incarnation_id: EnvironmentIncarnationId::new("external-incarnation"),
            connection: EnvironmentConnectionSpec::new(
                "ws://envd.example:19091",
                EnvironmentTransport::WebSocket,
            ),
            display_name: None,
            metadata: BTreeMap::new(),
            created_at_ms: 1_000,
        })
        .await
        .expect("create external environment");
    let EnvironmentSource::External { connection } = environment.source else {
        panic!("external source")
    };
    assert_eq!(connection.endpoint, "ws://envd.example:19091");
}

#[tokio::test(flavor = "current_thread")]
async fn disabled_binding_blocks_create_and_live_references_block_delete() {
    let (universe_id, store) = store().await;
    let environment = store
        .create_environment(create("request-1", "environment-1", "incarnation-1", 2_000))
        .await
        .expect("create");
    let mut disabled = binding(universe_id, Some(1));
    disabled.status = EnvironmentProviderBindingStatus::Disabled;
    store.put_provider_binding(disabled).await.expect("disable");
    assert!(matches!(
        store
            .create_environment(create("request-2", "environment-2", "incarnation-2", 3_000))
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    assert!(matches!(
        store
            .delete_provider_binding(universe_id, &EnvironmentProviderBindingId::new("primary"))
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
    store
        .finish_close_environment(FinishCloseEnvironment {
            environment_id: environment.environment_id,
            observed_at_ms: 4_000,
        })
        .await
        .expect("close");
    store
        .delete_provider_binding(universe_id, &EnvironmentProviderBindingId::new("primary"))
        .await
        .expect("delete");
}

#[tokio::test(flavor = "current_thread")]
async fn environment_metadata_filters_require_every_pair() {
    let (_universe, store) = store().await;
    // Metadata filters by containment: every listed pair must match.
    let mut tagged = create("tagged", "env-tagged", "inc-tagged", 3);
    tagged.metadata = BTreeMap::from([
        ("source".to_owned(), "harbor".to_owned()),
        ("job".to_owned(), "nightly".to_owned()),
    ]);
    store
        .create_environment(tagged)
        .await
        .expect("create tagged");
    let by_metadata = store
        .list_environments(ListEnvironments {
            metadata: BTreeMap::from([("job".to_owned(), "nightly".to_owned())]),
            ..ListEnvironments::default()
        })
        .await
        .expect("list by metadata");
    assert_eq!(by_metadata.len(), 1);
    assert_eq!(by_metadata[0].environment_id.as_str(), "env-tagged");
    let mismatched = store
        .list_environments(ListEnvironments {
            metadata: BTreeMap::from([
                ("job".to_owned(), "nightly".to_owned()),
                ("trial".to_owned(), "1".to_owned()),
            ]),
            ..ListEnvironments::default()
        })
        .await
        .expect("list by mismatched metadata");
    assert!(mismatched.is_empty());
}

#[test]
fn idle_policy_validates_monotone_stages_and_picks_the_supported_due_action() {
    let policy = EnvironmentIdlePolicy {
        pause_after_ms: Some(10),
        suspend_after_ms: Some(20),
        stop_after_ms: Some(30),
        close_after_ms: Some(40),
    };
    policy.validate().expect("valid");
    assert!(EnvironmentIdlePolicy::default().validate().is_err());
    assert!(
        EnvironmentIdlePolicy {
            pause_after_ms: Some(0),
            ..EnvironmentIdlePolicy::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        EnvironmentIdlePolicy {
            pause_after_ms: Some(30),
            stop_after_ms: Some(10),
            ..EnvironmentIdlePolicy::default()
        }
        .validate()
        .is_err()
    );
    let incus = [PowerState::Running, PowerState::Paused, PowerState::Stopped];
    assert_eq!(policy.due_action(5, &incus), None);
    assert_eq!(policy.due_action(10, &incus), Some(IdleAction::Pause));
    // Suspend is due but unsupported by this provider: pause stays the
    // most escalated applicable stage.
    assert_eq!(policy.due_action(25, &incus), Some(IdleAction::Pause));
    assert_eq!(policy.due_action(30, &incus), Some(IdleAction::Stop));
    assert_eq!(policy.due_action(40, &incus), Some(IdleAction::Close));
    // Close never needs provider support.
    assert_eq!(policy.due_action(40, &[]), Some(IdleAction::Close));
    assert_eq!(policy.due_action(30, &[]), None);
}

#[tokio::test(flavor = "current_thread")]
async fn power_intent_and_idle_policy_are_provisioned_only_and_drive_reconcile_lists() {
    let (_, store) = store().await;
    let environment = store
        .create_environment(create("request-1", "environment-1", "incarnation-1", 2_000))
        .await
        .expect("create");
    assert_eq!(environment.desired_power, PowerState::Running);
    assert!(environment.idle_policy.is_none());

    store
        .observe_provisioned_environment(ObserveProvisionedEnvironment {
            environment_id: environment.environment_id.clone(),
            provider_target_id: ProviderTargetId::new("target-1"),
            status: EnvironmentStatus::Ready,
            power_states: vec![PowerState::Running, PowerState::Paused],
            observed_at_ms: 3_000,
        })
        .await
        .expect("observe");
    // Ready and converged: nothing to reconcile.
    assert!(
        store
            .list_environments_needing_reconcile()
            .await
            .unwrap()
            .is_empty()
    );
    let paused = store
        .set_environment_power(SetEnvironmentPower {
            environment_id: environment.environment_id.clone(),
            desired_power: PowerState::Paused,
            updated_at_ms: 4_000,
        })
        .await
        .expect("set power");
    assert_eq!(paused.desired_power, PowerState::Paused);
    assert!(paused.power_diverges());
    let pending = store.list_environments_needing_reconcile().await.unwrap();
    assert_eq!(pending.len(), 1);

    // Observed paused: converged again.
    store
        .observe_provisioned_environment(ObserveProvisionedEnvironment {
            environment_id: environment.environment_id.clone(),
            provider_target_id: ProviderTargetId::new("target-1"),
            status: EnvironmentStatus::Paused,
            power_states: vec![PowerState::Running, PowerState::Paused],
            observed_at_ms: 5_000,
        })
        .await
        .expect("observe paused");
    assert!(
        store
            .list_environments_needing_reconcile()
            .await
            .unwrap()
            .is_empty()
    );

    // Idle policy only lists ready environments.
    let policy = EnvironmentIdlePolicy {
        pause_after_ms: Some(60_000),
        ..EnvironmentIdlePolicy::default()
    };
    store
        .set_environment_idle_policy(SetEnvironmentIdlePolicy {
            environment_id: environment.environment_id.clone(),
            idle_policy: Some(policy.clone()),
            updated_at_ms: 6_000,
        })
        .await
        .expect("policy");
    assert!(
        store
            .list_environments_with_idle_policy()
            .await
            .unwrap()
            .is_empty()
    );
    store
        .observe_provisioned_environment(ObserveProvisionedEnvironment {
            environment_id: environment.environment_id.clone(),
            provider_target_id: ProviderTargetId::new("target-1"),
            status: EnvironmentStatus::Ready,
            power_states: vec![PowerState::Running, PowerState::Paused],
            observed_at_ms: 7_000,
        })
        .await
        .expect("observe ready");
    let candidates = store.list_environments_with_idle_policy().await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].idle_policy, Some(policy));

    // External environments have no power control.
    let external = store
        .create_external_environment(CreateExternalEnvironment {
            request_id: EnvironmentProvisionRequestId::new("external-1"),
            environment_id: EnvironmentId::new("external-env"),
            incarnation_id: EnvironmentIncarnationId::new("external-inc"),
            connection: EnvironmentConnectionSpec::new(
                "ws://envd.test:9000",
                environment_protocol::shared::EnvironmentTransport::WebSocket,
            ),
            display_name: None,
            metadata: BTreeMap::new(),
            created_at_ms: 8_000,
        })
        .await
        .expect("external");
    assert!(matches!(
        store
            .set_environment_power(SetEnvironmentPower {
                environment_id: external.environment_id.clone(),
                desired_power: PowerState::Paused,
                updated_at_ms: 9_000,
            })
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));

    // Closing environments cannot change power.
    store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: environment.environment_id.clone(),
            updated_at_ms: 10_000,
        })
        .await
        .expect("close");
    assert!(matches!(
        store
            .set_environment_power(SetEnvironmentPower {
                environment_id: environment.environment_id.clone(),
                desired_power: PowerState::Running,
                updated_at_ms: 11_000,
            })
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
}

fn registration_policy(mode: RegisteredIdentityMode) -> RegistrationKeyPolicy {
    RegistrationKeyPolicy {
        display_name: "harbor campaign".to_owned(),
        identity_mode: mode,
        max_active_environments: None,
        ephemeral_disconnect_grace_ms: None,
        expires_at_ms: None,
    }
}

async fn minted_key(
    store: &InMemoryEnvironmentRegistryStore,
    id: &str,
    policy: RegistrationKeyPolicy,
) -> MintedRegistrationKey {
    let minted =
        mint_registration_key(EnvironmentRegistrationKeyId::new(id), policy, 1_000).expect("mint");
    store
        .create_registration_key(CreateEnvironmentRegistrationKey {
            secret_hash: minted.secret_hash.clone(),
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
        environment_id: EnvironmentId::new(environment),
        incarnation_id: EnvironmentIncarnationId::new(format!("{environment}-inc")),
        daemon_public_key: public_key.to_owned(),
        display_name: Some("worker".to_owned()),
        metadata: BTreeMap::new(),
        created_at_ms: at,
    }
}

#[test]
fn daemon_id_and_request_id_derive_from_the_public_key() {
    let public_key = [7u8; 32];
    let daemon_id = EnvironmentDaemonId::from_public_key(&public_key);
    assert!(daemon_id.as_str().starts_with(DAEMON_ID_PREFIX));
    assert_eq!(daemon_id.as_str().len(), DAEMON_ID_PREFIX.len() + 64);
    assert_eq!(daemon_id, EnvironmentDaemonId::from_public_key(&public_key));
    assert_ne!(daemon_id, EnvironmentDaemonId::from_public_key(&[8u8; 32]));
    let request_id = EnvironmentProvisionRequestId::for_daemon(&daemon_id);
    assert_eq!(
        request_id.as_str(),
        format!("{DAEMON_PROVISION_REQUEST_PREFIX}{}", daemon_id.as_str())
    );
    assert!(validate_daemon_public_key(&daemon_key(0xab)).is_ok());
    assert!(validate_daemon_public_key("ABCD").is_err());
}

#[test]
fn minted_registration_keys_return_the_secret_once_and_redact_debug() {
    let minted = mint_registration_key(
        EnvironmentRegistrationKeyId::new("rk-1"),
        registration_policy(RegisteredIdentityMode::Ephemeral),
        5,
    )
    .expect("mint");
    let secret = minted.secret.expose().to_owned();
    assert!(secret.starts_with(REGISTRATION_KEY_SECRET_PREFIX));
    assert_eq!(minted.secret_hash, registration_key_hash(&secret));
    assert_eq!(
        minted.record.key_prefix.len(),
        REGISTRATION_KEY_DISPLAY_PREFIX_LEN
    );
    assert!(secret.starts_with(&minted.record.key_prefix));
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

#[tokio::test(flavor = "current_thread")]
async fn registered_admission_is_keyed_by_daemon_identity() {
    let (_universe_id, store) = store().await;
    let minted = minted_key(
        &store,
        "rk-eph",
        registration_policy(RegisteredIdentityMode::Ephemeral),
    )
    .await;
    let resolved = store
        .resolve_registration_key(&minted.secret_hash)
        .await
        .expect("resolve")
        .expect("known");
    assert_eq!(resolved.registration_key_id.as_str(), "rk-eph");
    assert!(
        store
            .resolve_registration_key("0000")
            .await
            .expect("resolve")
            .is_none()
    );

    let public_key = daemon_key(0x11);
    let created = store
        .create_registered_environment(register("rk-eph", "env-a", &public_key, 2_000))
        .await
        .expect("register");
    assert_eq!(created.status, EnvironmentStatus::Ready);
    assert_eq!(created.last_seen_at_ms, Some(2_000));
    assert_eq!(
        created.source.identity_mode(),
        Some(RegisteredIdentityMode::Ephemeral)
    );
    assert_eq!(
        created.request_id,
        EnvironmentProvisionRequestId::for_daemon(&EnvironmentDaemonId::from_public_key(
            &[0x11u8; 32]
        ))
    );

    // A retried first registration converges on the same environment.
    let retried = store
        .create_registered_environment(register("rk-eph", "env-b", &public_key, 2_500))
        .await
        .expect("retry");
    assert_eq!(retried.environment_id, created.environment_id);
    assert_eq!(
        store
            .read_environment_by_daemon_public_key(&public_key)
            .await
            .expect("read")
            .map(|record| record.environment_id),
        Some(created.environment_id.clone())
    );

    let offline = store
        .observe_registered_environment(ObserveRegisteredEnvironment {
            environment_id: created.environment_id.clone(),
            observation: RegisteredConnectionObservation::Disconnected,
            observed_at_ms: 3_000,
            metadata: BTreeMap::new(),
        })
        .await
        .expect("disconnect");
    assert_eq!(offline.status, EnvironmentStatus::Offline);
    assert_eq!(offline.last_seen_at_ms, Some(2_000));
    assert!(offline.registered_daemon_absent(3_000, 60_000));

    // A reconnecting daemon may be a newer build: the admission re-records
    // its build facts on the row.
    let back = store
        .observe_registered_environment(ObserveRegisteredEnvironment {
            environment_id: created.environment_id.clone(),
            observation: RegisteredConnectionObservation::Connected,
            observed_at_ms: 4_000,
            metadata: RegisteredDaemonBuild {
                version: Some("0.2.0".to_owned()),
                git_sha: Some("bbbb".to_owned()),
                protocol_version: 3,
            }
            .metadata(),
        })
        .await
        .expect("reconnect");
    assert_eq!(back.status, EnvironmentStatus::Ready);
    assert_eq!(back.last_seen_at_ms, Some(4_000));
    assert_eq!(
        back.metadata
            .get(ENVD_VERSION_METADATA_KEY)
            .map(String::as_str),
        Some("0.2.0")
    );
    assert_eq!(
        back.metadata
            .get(ENVD_GIT_SHA_METADATA_KEY)
            .map(String::as_str),
        Some("bbbb")
    );
    assert_eq!(
        back.metadata
            .get(ENVD_PROTOCOL_VERSION_METADATA_KEY)
            .map(String::as_str),
        Some("3")
    );
    assert!(!back.registered_daemon_absent(4_500, 60_000));
    assert!(back.registered_daemon_absent(100_000, 60_000));

    store
        .begin_close_environment(BeginCloseEnvironment {
            environment_id: created.environment_id.clone(),
            updated_at_ms: 5_000,
        })
        .await
        .expect("begin close");
    let closed = store
        .finish_close_environment(FinishCloseEnvironment {
            environment_id: created.environment_id.clone(),
            observed_at_ms: 5_100,
        })
        .await
        .expect("close");
    assert_eq!(closed.status, EnvironmentStatus::Closed);
    // A late heartbeat cannot resurrect a closed environment.
    let still_closed = store
        .observe_registered_environment(ObserveRegisteredEnvironment {
            environment_id: created.environment_id.clone(),
            observation: RegisteredConnectionObservation::Connected,
            observed_at_ms: 5_200,
            metadata: BTreeMap::from([(ENVD_GIT_SHA_METADATA_KEY.to_owned(), "cccc".to_owned())]),
        })
        .await
        .expect("late heartbeat");
    assert_eq!(still_closed.status, EnvironmentStatus::Closed);
    assert_eq!(
        still_closed
            .metadata
            .get(ENVD_GIT_SHA_METADATA_KEY)
            .map(String::as_str),
        Some("bbbb")
    );

    // The identity is spent: the same key with a fresh environment id is
    // refused, and a different registration key cannot move it either.
    let spent = store
        .create_registered_environment(register("rk-eph", "env-c", &public_key, 6_000))
        .await
        .expect("dedup");
    assert_eq!(spent.environment_id, created.environment_id);
    assert_eq!(spent.status, EnvironmentStatus::Closed);
}

#[tokio::test(flavor = "current_thread")]
async fn registration_key_policy_gates_admission_without_touching_reconnects() {
    let (_universe_id, store) = store().await;
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
            updated_at_ms: 3_000,
        })
        .await
        .expect("begin close");
    store
        .finish_close_environment(FinishCloseEnvironment {
            environment_id: first.environment_id.clone(),
            observed_at_ms: 3_100,
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
    let refused = store
        .create_registered_environment(register("rk-cap", "env-3", &daemon_key(0x23), 5_000))
        .await
        .expect_err("revoked");
    assert!(matches!(
        refused,
        EnvironmentRegistryError::RegistrationKeyUnavailable {
            reason: "revoked",
            ..
        }
    ));
    // Known daemons never consult the key.
    let reconnect = store
        .observe_registered_environment(ObserveRegisteredEnvironment {
            environment_id: EnvironmentId::new("env-2"),
            observation: RegisteredConnectionObservation::Connected,
            observed_at_ms: 5_500,
            metadata: BTreeMap::new(),
        })
        .await
        .expect("reconnect after revoke");
    assert_eq!(reconnect.status, EnvironmentStatus::Ready);

    minted_key(
        &store,
        "rk-exp",
        RegistrationKeyPolicy {
            expires_at_ms: Some(1_500),
            ..registration_policy(RegisteredIdentityMode::Ephemeral)
        },
    )
    .await;
    let expired = store
        .create_registered_environment(register("rk-exp", "env-4", &daemon_key(0x24), 2_000))
        .await
        .expect_err("expired");
    assert!(matches!(
        expired,
        EnvironmentRegistryError::RegistrationKeyUnavailable {
            reason: "expired",
            ..
        }
    ));
    let keys = store.list_registration_keys().await.expect("list");
    assert_eq!(keys.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn registered_environments_group_by_key_and_access_policy_scopes_them() {
    let (_universe_id, store) = store().await;
    minted_key(
        &store,
        "rk-a",
        registration_policy(RegisteredIdentityMode::Persistent),
    )
    .await;
    minted_key(
        &store,
        "rk-b",
        registration_policy(RegisteredIdentityMode::Persistent),
    )
    .await;
    let a = store
        .create_registered_environment(register("rk-a", "env-a", &daemon_key(0x31), 2_000))
        .await
        .expect("a");
    let b = store
        .create_registered_environment(register("rk-b", "env-b", &daemon_key(0x32), 2_000))
        .await
        .expect("b");
    let provisioned = store
        .create_environment(create("p", "env-p", "inc-p", 2_000))
        .await
        .expect("provisioned");
    let external = store
        .create_external_environment(CreateExternalEnvironment {
            request_id: EnvironmentProvisionRequestId::new("ext"),
            environment_id: EnvironmentId::new("env-x"),
            incarnation_id: EnvironmentIncarnationId::new("inc-x"),
            connection: EnvironmentConnectionSpec::new(
                "ws://envd.internal:19091",
                EnvironmentTransport::WebSocket,
            ),
            display_name: None,
            metadata: BTreeMap::new(),
            created_at_ms: 2_000,
        })
        .await
        .expect("external");

    let by_key = store
        .list_environments(ListEnvironments {
            metadata: Default::default(),
            registration_key_id: Some(EnvironmentRegistrationKeyId::new("rk-a")),
            ..ListEnvironments::default()
        })
        .await
        .expect("list");
    assert_eq!(by_key.len(), 1);
    assert_eq!(by_key[0].environment_id, a.environment_id);

    let open = EnvironmentAccessPolicy::ALLOW_ALL;
    assert!(
        open.allows(&a) && open.allows(&b) && open.allows(&provisioned) && open.allows(&external)
    );

    let keys_only =
        EnvironmentAccessPolicy::new(None::<Vec<String>>, Some(vec!["rk-a".to_owned()]));
    assert!(keys_only.allows(&a));
    assert!(!keys_only.allows(&b));
    assert!(keys_only.allows(&provisioned));
    assert!(!keys_only.allows(&external));
    assert!(keys_only.refusal(&b).contains("rk-b"));

    let providers_only =
        EnvironmentAccessPolicy::new(Some(vec!["incus-local".to_owned()]), None::<Vec<String>>);
    assert!(providers_only.allows(&provisioned));
    assert!(providers_only.allows(&a));
    assert!(!providers_only.allows(&external));

    let neither =
        EnvironmentAccessPolicy::new(Some(vec!["other".to_owned()]), Some(Vec::<String>::new()));
    assert!(!neither.allows(&provisioned));
    assert!(!neither.allows(&a));
    assert!(!neither.allows(&external));
}

/// The gateway stamps reserved entries on top of whatever a daemon sent, so
/// a daemon using its whole allowance must still be admitted; the allowance
/// itself stays enforced on the caller's keys.
#[tokio::test(flavor = "current_thread")]
async fn reserved_metadata_sits_on_top_of_the_caller_allowance() {
    use environment_protocol::registration::MAX_METADATA_ENTRIES;

    let (_universe_id, store) = store().await;
    minted_key(
        &store,
        "rk-meta",
        registration_policy(RegisteredIdentityMode::Persistent),
    )
    .await;
    let reserved = RegisteredDaemonBuild {
        version: Some("0.1.0".to_owned()),
        git_sha: Some("a".repeat(40)),
        protocol_version: 2,
    }
    .metadata();

    let mut full = register("rk-meta", "env-meta", &daemon_key(0x31), 1_000);
    full.metadata = (0..MAX_METADATA_ENTRIES)
        .map(|index| (format!("caller.key{index}"), "v".to_owned()))
        .collect();
    full.metadata.extend(reserved.clone());
    let created = store
        .create_registered_environment(full)
        .await
        .expect("full caller allowance plus reserved entries");
    assert_eq!(
        created.metadata.len(),
        MAX_METADATA_ENTRIES + reserved.len()
    );

    let mut over = register("rk-meta", "env-over", &daemon_key(0x32), 1_000);
    over.metadata = (0..=MAX_METADATA_ENTRIES)
        .map(|index| (format!("caller.key{index}"), "v".to_owned()))
        .collect();
    assert!(matches!(
        store.create_registered_environment(over).await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));
}

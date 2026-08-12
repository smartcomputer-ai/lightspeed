use std::collections::BTreeMap;

use host_protocol::shared::{HostTargetId, HostTransport};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::*;

fn provider() -> PutEnvironmentProvider {
    PutEnvironmentProvider {
        provider_id: EnvironmentProviderId::new("incus-local"),
        display_name: Some("Local Incus".to_owned()),
        controller_connection: HostControllerConnectionSpec::new(
            "ws://127.0.0.1:19090/control",
            HostTransport::WebSocket,
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
    let target_id = HostTargetId::new("target-1");
    let observed = store
        .observe_provisioned_environment(ObserveProvisionedEnvironment {
            environment_id: environment.environment_id.clone(),
            provider_target_id: target_id.clone(),
            status: EnvironmentStatus::WaitingForDaemon,
            observed_at_ms: 3_000,
        })
        .await
        .expect("observe");
    assert_eq!(observed.status, EnvironmentStatus::WaitingForDaemon);
    assert_eq!(observed.incarnation.provider_target_id, Some(target_id));
    assert_eq!(
        observed.incarnation.template_id,
        Some(EnvironmentTemplateId::new("rust-v1"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provisioned_environment_has_no_direct_daemon_enrollment() {
    let (_, store) = store().await;
    let environment = store
        .create_environment(create("request-1", "environment-1", "incarnation-1", 2_000))
        .await
        .expect("create");

    assert!(matches!(
        store.read_enrollment(&environment.environment_id).await,
        Err(EnvironmentRegistryError::NotFound {
            kind: "environment_enrollment",
            ..
        })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn direct_enrollment_token_is_one_time_and_revocation_is_final() {
    let store = InMemoryEnvironmentRegistryStore::new();
    let token_hash = Sha256::digest(b"one-time-token").to_vec();
    let (environment, pending, created) = store
        .create_enrollment(CreateEnvironmentEnrollment {
            request_id: EnvironmentProvisionRequestId::new("enroll-request-1"),
            environment_id: EnvironmentId::new("environment-direct"),
            incarnation_id: EnvironmentIncarnationId::new("incarnation-direct"),
            display_name: None,
            metadata: BTreeMap::new(),
            token_hash: token_hash.clone(),
            token_expires_at_ms: 10_000,
            created_at_ms: 1_000,
        })
        .await
        .expect("create enrollment");
    assert!(created);
    assert!(pending.daemon_id.is_none());

    let enrolled = store
        .enroll_daemon(EnrollEnvironmentDaemon {
            environment_id: environment.environment_id.clone(),
            incarnation_id: environment.incarnation.incarnation_id.clone(),
            token_hash: token_hash.clone(),
            daemon_id: EnvironmentDaemonId::new("daemon-a"),
            daemon_public_key: vec![7; 32],
            enrolled_at_ms: 2_000,
        })
        .await
        .expect("redeem token");
    assert_eq!(enrolled.token_redeemed_at_ms, Some(2_000));
    assert!(matches!(
        store
            .enroll_daemon(EnrollEnvironmentDaemon {
                environment_id: environment.environment_id.clone(),
                incarnation_id: environment.incarnation.incarnation_id.clone(),
                token_hash,
                daemon_id: EnvironmentDaemonId::new("daemon-b"),
                daemon_public_key: vec![8; 32],
                enrolled_at_ms: 3_000,
            })
            .await,
        Err(EnvironmentRegistryError::InvalidInput { .. })
    ));

    let revoked = store
        .revoke_enrollment(&environment.environment_id, 4_000)
        .await
        .expect("revoke");
    assert_eq!(revoked.revoked_at_ms, Some(4_000));
    let revoked_again = store
        .revoke_enrollment(&environment.environment_id, 5_000)
        .await
        .expect("idempotent revoke");
    assert_eq!(revoked_again.revoked_at_ms, Some(4_000));
}

#[tokio::test(flavor = "current_thread")]
async fn direct_enrollment_rejects_wrong_expired_and_stale_tokens() {
    for (environment, incarnation, hash, enrolled_at_ms) in [
        ("wrong-token", "incarnation-wrong", vec![9; 32], 2_000),
        (
            "expired-token",
            "incarnation-expired",
            Sha256::digest(b"token").to_vec(),
            20_000,
        ),
    ] {
        let store = InMemoryEnvironmentRegistryStore::new();
        store
            .create_enrollment(CreateEnvironmentEnrollment {
                request_id: EnvironmentProvisionRequestId::new(format!("request-{environment}")),
                environment_id: EnvironmentId::new(environment),
                incarnation_id: EnvironmentIncarnationId::new(incarnation),
                display_name: None,
                metadata: BTreeMap::new(),
                token_hash: Sha256::digest(b"token").to_vec(),
                token_expires_at_ms: 10_000,
                created_at_ms: 1_000,
            })
            .await
            .expect("create enrollment");
        assert!(matches!(
            store
                .enroll_daemon(EnrollEnvironmentDaemon {
                    environment_id: EnvironmentId::new(environment),
                    incarnation_id: EnvironmentIncarnationId::new(incarnation),
                    token_hash: hash,
                    daemon_id: EnvironmentDaemonId::new("daemon-a"),
                    daemon_public_key: vec![7; 32],
                    enrolled_at_ms,
                })
                .await,
            Err(EnvironmentRegistryError::InvalidInput { .. })
        ));
    }
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

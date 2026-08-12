use std::collections::BTreeMap;

use host_protocol::shared::{HostTargetId, HostTransport};
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

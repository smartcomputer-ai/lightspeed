use std::collections::{BTreeMap, BTreeSet};

use engine::SessionId;
use host_protocol::{
    control::targets::{HostTargetStatus, HostTargetSummary},
    shared::{
        HostCapabilities, HostConnectionSpec, HostPath, HostScope, HostTargetId, HostTransport,
        ImplementationInfo,
    },
};

use super::*;

fn provider() -> RegisterEnvironmentProvider {
    RegisterEnvironmentProvider {
        provider_id: EnvironmentProviderId::new("bridge"),
        provider_kind: EnvironmentProviderKind::Bridge,
        display_name: None,
        controller_connection: HostControllerConnectionSpec::new(
            "http://bridge",
            HostTransport::Http,
        ),
        capabilities: EnvironmentProviderCapabilities {
            list_targets: true,
            create_target: true,
            get_target: true,
            close_target: true,
        },
        implementation: ImplementationInfo {
            name: "test".to_owned(),
            version: None,
        },
        lease_ttl_ms: 1_000,
        metadata: BTreeMap::new(),
        observed_at_ms: 100,
    }
}

fn observation(instance_id: &str, origin: EnvironmentInstanceOrigin) -> ObserveEnvironmentInstance {
    let target_id = HostTargetId::new("local");
    ObserveEnvironmentInstance::from_observation(
        EnvironmentInstanceId::new(instance_id),
        EnvironmentProviderId::new("bridge"),
        origin,
        ObservedEnvironmentTarget {
            target: HostTargetSummary {
                target_id: target_id.clone(),
                display_name: None,
                status: HostTargetStatus::Ready,
                scope: HostScope::Default,
                capabilities: HostCapabilities::filesystem(true, true)
                    .with_process()
                    .with_jobs(),
                default_cwd: Some(HostPath::new("/workspace").expect("path")),
                metadata: BTreeMap::new(),
            },
            connection: HostConnectionSpec {
                target_id,
                endpoint: "http://host".to_owned(),
                transport: HostTransport::Http,
                scope: HostScope::Default,
                default_cwd: Some(HostPath::new("/workspace").expect("path")),
                capabilities: HostCapabilities::filesystem(true, true)
                    .with_process()
                    .with_jobs(),
            },
        },
        200,
    )
}

#[test]
fn provider_presence_derives_stale_from_lease() {
    let record = provider().into_record().expect("provider");
    assert_eq!(record.presence_at(500), EnvironmentProviderPresence::Online);
    assert_eq!(
        record.presence_at(1_100),
        EnvironmentProviderPresence::Stale
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_target_identity_is_stable_across_observations() {
    let store = InMemoryEnvironmentRegistryStore::new();
    store.register_provider(provider()).await.expect("provider");
    let first = store
        .observe_instance(observation(
            "instance-a",
            EnvironmentInstanceOrigin::Provided,
        ))
        .await
        .expect("first");
    let second = store
        .observe_instance(observation(
            "instance-b",
            EnvironmentInstanceOrigin::Provided,
        ))
        .await
        .expect("second");
    assert_eq!(first.instance_id, second.instance_id);
}

#[tokio::test(flavor = "current_thread")]
async fn re_pointing_detached_binding_clears_credentials() {
    let store = InMemoryEnvironmentRegistryStore::new();
    store.register_provider(provider()).await.expect("provider");
    let first = store
        .observe_instance(observation(
            "instance-a",
            EnvironmentInstanceOrigin::Provided,
        ))
        .await
        .expect("first");
    let mut second_observation = observation("instance-b", EnvironmentInstanceOrigin::Provided);
    second_observation.provider_target_id = HostTargetId::new("other");
    second_observation.connection.target_id = HostTargetId::new("other");
    let second = store
        .observe_instance(second_observation)
        .await
        .expect("second");
    let session_id = SessionId::new("session");
    let env_id = EnvironmentId::new("dev");
    store
        .put_binding(PutSessionEnvironmentBinding {
            session_id: session_id.clone(),
            env_id: env_id.clone(),
            instance_id: first.instance_id,
            cwd: None,
            fs_routes: Vec::new(),
            updated_at_ms: 300,
        })
        .await
        .expect("binding");
    store
        .bind_credential(CreateSessionEnvironmentCredential {
            session_id: session_id.clone(),
            env_id: env_id.clone(),
            env_name: "TOKEN".to_owned(),
            source: SessionEnvironmentCredentialSource::DirectSecret {
                secret_id: SecretId::new("secret"),
            },
            created_at_ms: 301,
        })
        .await
        .expect("credential");
    store
        .update_binding_state(UpdateSessionEnvironmentBindingState {
            session_id: session_id.clone(),
            env_id: env_id.clone(),
            state: SessionEnvironmentBindingState::Detached,
            updated_at_ms: 302,
        })
        .await
        .expect("detach");
    store
        .put_binding(PutSessionEnvironmentBinding {
            session_id: session_id.clone(),
            env_id: env_id.clone(),
            instance_id: second.instance_id,
            cwd: None,
            fs_routes: Vec::new(),
            updated_at_ms: 303,
        })
        .await
        .expect("reattach");
    let credentials = store
        .list_credentials(ListSessionEnvironmentCredentials { session_id, env_id })
        .await
        .expect("credentials");
    assert!(credentials.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn close_rejects_bindings_and_active_job_groups() {
    let store = InMemoryEnvironmentRegistryStore::new();
    store.register_provider(provider()).await.expect("provider");
    let instance = store
        .observe_instance(observation(
            "instance",
            EnvironmentInstanceOrigin::Provisioned,
        ))
        .await
        .expect("instance");
    store
        .reserve_job_group(ReserveEnvironmentJobGroup {
            instance_id: instance.instance_id.clone(),
            job_group_id: EnvironmentJobGroupId::new("group"),
            request_id: "request".to_owned(),
            start_request_hash: "hash".to_owned(),
            created_at_ms: 300,
        })
        .await
        .expect("group");
    let error = store
        .begin_close_instance(BeginCloseEnvironmentInstance {
            instance_id: instance.instance_id,
            updated_at_ms: 400,
        })
        .await
        .expect_err("occupied");
    assert!(matches!(error, EnvironmentRegistryError::Occupied { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn missing_provided_targets_become_unknown() {
    let store = InMemoryEnvironmentRegistryStore::new();
    store.register_provider(provider()).await.expect("provider");
    let instance = store
        .observe_instance(observation("instance", EnvironmentInstanceOrigin::Provided))
        .await
        .expect("instance");
    let changed = store
        .mark_missing_provided_instances_unknown(
            &EnvironmentProviderId::new("bridge"),
            &BTreeSet::new(),
            500,
        )
        .await
        .expect("missing");
    assert_eq!(changed.len(), 1);
    assert_eq!(
        store
            .read_instance(&instance.instance_id)
            .await
            .expect("instance")
            .status,
        HostTargetStatus::Unknown
    );
}

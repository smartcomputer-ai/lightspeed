use std::path::PathBuf;

use environment_provider_incus::{IncusBackend as _, IncusClient, ProviderArgs};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a reachable standalone Incus server or Incus cluster"]
async fn configured_topology_and_binding_reconciliation_are_live() {
    let config_path = std::env::var("LIGHTSPEED_INCUS_PROVIDER_CONFIG")
        .map(PathBuf::from)
        .expect("LIGHTSPEED_INCUS_PROVIDER_CONFIG must name a live provider config");
    let config = ProviderArgs {
        config: config_path,
    }
    .load()
    .await
    .expect("load live provider config");
    let backend = IncusClient::new(config.clone())
        .await
        .expect("connect and validate live Incus topology");
    let topology = backend.topology().await.expect("read live topology");
    assert_eq!(topology.mode, config.incus_mode_name());
    assert!(!topology.members.is_empty());

    for binding in config.bindings.values() {
        backend
            .reconcile_binding(binding)
            .await
            .expect("reconcile live provider binding");
        let targets = backend
            .list_owned(binding)
            .await
            .expect("list live owned targets");
        for target in targets {
            assert_eq!(target.universe_id, binding.universe_id);
            assert_eq!(target.binding_id, binding.binding_id);
            if topology.mode == "cluster" {
                assert!(target.location.is_some());
            }
        }
    }
}

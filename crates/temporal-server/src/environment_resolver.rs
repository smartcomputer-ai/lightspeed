//! Internal environment lookup and liveness policy.
//!
//! This is a shared runtime service, not a provider/plugin extension seam.

use std::{collections::BTreeSet, sync::Arc};

use environments::{
    EnvironmentId, EnvironmentProviderStore, EnvironmentRecord, EnvironmentRegistryError,
    EnvironmentStore, ListEnvironments,
};
use store_pg::PgStore;
use thiserror::Error;

#[derive(Clone)]
pub(crate) struct EnvironmentResolver {
    environments: Arc<dyn EnvironmentStore>,
    providers: Arc<dyn EnvironmentProviderStore>,
}

impl EnvironmentResolver {
    pub(crate) fn new(
        environments: Arc<dyn EnvironmentStore>,
        providers: Arc<dyn EnvironmentProviderStore>,
    ) -> Self {
        Self {
            environments,
            providers,
        }
    }

    pub(crate) fn from_pg_store(store: Arc<PgStore>) -> Self {
        Self::new(store.clone(), store)
    }

    pub(crate) async fn list_allowed(
        &self,
        allowed_providers: Option<&BTreeSet<String>>,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentResolveError> {
        let mut environments = self
            .environments
            .list_environments(ListEnvironments::default())
            .await?;
        if let Some(allowed) = allowed_providers {
            environments.retain(|environment| {
                environment
                    .provider_id()
                    .is_some_and(|id| allowed.contains(id.as_str()))
            });
        }
        Ok(environments)
    }

    pub(crate) async fn read_allowed(
        &self,
        environment_id: &EnvironmentId,
        allowed_providers: Option<&BTreeSet<String>>,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self.environments.read_environment(environment_id).await?;
        if allowed_providers.is_some_and(|allowed| {
            environment
                .provider_id()
                .is_none_or(|id| !allowed.contains(id.as_str()))
        }) {
            return Err(EnvironmentResolveError::ProviderNotAllowed {
                provider_id: environment
                    .provider_id()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "enrolled".to_owned()),
            });
        }
        Ok(environment)
    }

    pub(crate) async fn selectable(
        &self,
        environment_id: &EnvironmentId,
        allowed_providers: Option<&BTreeSet<String>>,
        _now_ms: i64,
    ) -> Result<EnvironmentRecord, EnvironmentResolveError> {
        let environment = self.read_allowed(environment_id, allowed_providers).await?;
        if let Some(provider_id) = environment.provider_id() {
            self.providers.read_provider(provider_id).await?;
        }
        Err(EnvironmentResolveError::EnvironmentUnavailable {
            environment_id: environment.environment_id.as_str().to_owned(),
            status: "data-plane routing is introduced by P119".to_owned(),
        })
    }
}

#[derive(Debug, Error)]
pub(crate) enum EnvironmentResolveError {
    #[error(transparent)]
    Store(#[from] EnvironmentRegistryError),

    #[error("environment provider is not allowed by session config: {provider_id}")]
    ProviderNotAllowed { provider_id: String },

    #[error("environment is unavailable: {environment_id} ({status})")]
    EnvironmentUnavailable {
        environment_id: String,
        status: String,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use environments::{
        CreateEnvironment, EnvironmentIncarnationId, EnvironmentProviderBindingId,
        EnvironmentProviderBindingStatus, EnvironmentProviderBindingStore, EnvironmentProviderId,
        EnvironmentProvisionRequestId, EnvironmentStatus, EnvironmentTemplateId,
        HostControllerConnectionSpec, ObserveProvisionedEnvironment, PutEnvironmentProvider,
        PutEnvironmentProviderBinding,
    };
    use host_protocol::shared::{HostTargetId, HostTransport};

    use super::*;

    async fn resolver() -> (EnvironmentResolver, EnvironmentId) {
        let store = Arc::new(environments::InMemoryEnvironmentRegistryStore::new());
        let provider_id = EnvironmentProviderId::new("allowed");
        store
            .put_provider(PutEnvironmentProvider {
                provider_id: provider_id.clone(),
                display_name: None,
                controller_connection: HostControllerConnectionSpec::new(
                    "http://controller.test",
                    HostTransport::Http,
                ),
                metadata: BTreeMap::new(),
                updated_at_ms: 10,
            })
            .await
            .expect("provider");
        store
            .put_provider_binding(PutEnvironmentProviderBinding {
                universe_id: store.universe_id(),
                binding_id: EnvironmentProviderBindingId::new("primary"),
                provider_id: provider_id.clone(),
                status: EnvironmentProviderBindingStatus::Enabled,
                expected_revision: None,
                metadata: BTreeMap::new(),
                updated_at_ms: 10,
            })
            .await
            .expect("binding");
        let environment_id = EnvironmentId::new("environment-1");
        let target_id = HostTargetId::new("target-1");
        store
            .create_environment(CreateEnvironment {
                request_id: EnvironmentProvisionRequestId::new("request-1"),
                environment_id: environment_id.clone(),
                incarnation_id: EnvironmentIncarnationId::new("incarnation-1"),
                binding_id: EnvironmentProviderBindingId::new("primary"),
                template_id: EnvironmentTemplateId::new("test-template"),
                display_name: None,
                metadata: BTreeMap::new(),
                created_at_ms: 10,
            })
            .await
            .expect("create environment");
        store
            .observe_provisioned_environment(ObserveProvisionedEnvironment {
                environment_id: environment_id.clone(),
                provider_target_id: target_id.clone(),
                status: EnvironmentStatus::WaitingForDaemon,
                observed_at_ms: 10,
            })
            .await
            .expect("environment");
        (
            EnvironmentResolver::new(store.clone(), store),
            environment_id,
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_filter_applies_to_list_read_and_selection() {
        let (resolver, environment_id) = resolver().await;
        let denied = BTreeSet::from(["other".to_owned()]);
        assert!(
            resolver
                .list_allowed(Some(&denied))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            resolver.read_allowed(&environment_id, Some(&denied)).await,
            Err(EnvironmentResolveError::ProviderNotAllowed { .. })
        ));
        assert!(matches!(
            resolver
                .selectable(&environment_id, Some(&denied), 20)
                .await,
            Err(EnvironmentResolveError::ProviderNotAllowed { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn selection_is_deferred_until_p119_but_read_still_explores() {
        let (resolver, environment_id) = resolver().await;
        assert!(resolver.read_allowed(&environment_id, None).await.is_ok());
        assert!(matches!(
            resolver.selectable(&environment_id, None, 111).await,
            Err(EnvironmentResolveError::EnvironmentUnavailable { .. })
        ));
    }
}

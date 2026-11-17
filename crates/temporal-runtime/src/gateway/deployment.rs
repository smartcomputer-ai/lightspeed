//! Deployment-scoped (deployment-level) service: universe lifecycle over the
//! shared deployment resources, above the universe-bound store boundary.
//!
use std::sync::Arc;

use api::AccessScope;

use api::{
    AgentApiError, DeploymentApiService, DeploymentEnvironmentProviderDeleteParams,
    DeploymentEnvironmentProviderDeleteResponse, DeploymentEnvironmentProviderListParams,
    DeploymentEnvironmentProviderListResponse, DeploymentEnvironmentProviderPutParams,
    DeploymentEnvironmentProviderPutResponse, DeploymentEnvironmentProviderReadParams,
    DeploymentEnvironmentProviderReadResponse, DeploymentProviderBindingDeleteParams,
    DeploymentProviderBindingDeleteResponse, DeploymentProviderBindingPutParams,
    DeploymentProviderBindingPutResponse, DeploymentUniverseCreateParams,
    DeploymentUniverseCreateResponse, DeploymentUniverseDeleteParams,
    DeploymentUniverseDeleteResponse, DeploymentUniverseListParams, DeploymentUniverseListResponse,
    DeploymentUniverseReadParams, DeploymentUniverseReadResponse, DeploymentUniverseView,
};
use async_trait::async_trait;
use credentials::SecretValue;
use environment_protocol::routes::DataRouteBase;
use environments::{
    EnvironmentEndpoint, EnvironmentProviderBindingStore, EnvironmentProviderId,
    EnvironmentProviderStore, EnvironmentRegistryError, ProviderCredentialChange,
    PutEnvironmentProvider, PutEnvironmentProviderBinding,
};
use harness::SessionId;
use uuid::Uuid;

use crate::{
    environments::{
        provider_controllers::{WebSocketProviderControllerConnector, probe_controller},
        providers::environment_provider_view,
    },
    universe::UniverseRuntime,
};

pub struct GatewayDeploymentApi {
    runtime: Arc<UniverseRuntime>,
}

impl GatewayDeploymentApi {
    pub fn new(runtime: Arc<UniverseRuntime>) -> Self {
        Self { runtime }
    }

    fn pool(&self) -> &sqlx::PgPool {
        self.runtime.stores().pool()
    }

    /// Deployment handlers require an explicit deployment request target.
    async fn admitted<T>(
        &self,
        method: &str,
        operation: impl Future<Output = Result<T, AgentApiError>>,
    ) -> Result<T, AgentApiError> {
        admit(method)?;
        operation.await
    }

    async fn read_universe_view(
        &self,
        universe_id: Uuid,
    ) -> Result<Option<DeploymentUniverseView>, AgentApiError> {
        let stats = store_pg::read_universe_stats(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?;
        Ok(stats.map(universe_view))
    }

    /// The existing universe a request names by UUID or handle.
    async fn resolve_universe(&self, reference: &str) -> Result<Uuid, AgentApiError> {
        let reference = reference.trim();
        store_pg::resolve_universe_reference(self.pool(), reference)
            .await
            .map_err(map_store_error)?
            .ok_or_else(|| AgentApiError::not_found(format!("unknown universe: {reference}")))
    }
}

#[async_trait]
impl DeploymentApiService for GatewayDeploymentApi {
    async fn list_deleted_sessions(
        &self,
        params: api::DeploymentDeletedSessionsListParams,
    ) -> Result<api::DeploymentDeletedSessionsListResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_SESSIONS_DELETED_LIST, async {
            let universe_id = self.resolve_universe(&params.universe).await?;
            let rows = self
                .runtime
                .stores()
                .store_for(universe_id)
                .list_deleted_sessions(params.after.as_deref(), 101)
                .await
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
            let next_after = (rows.len() > 100).then(|| rows[99].0.clone());
            let sessions = rows
                .into_iter()
                .take(100)
                .map(
                    |(session_id, display_name, deleted_at_ms)| api::DeletedSessionView {
                        session_id,
                        display_name,
                        deleted_at_ms,
                    },
                )
                .collect();
            Ok(api::DeploymentDeletedSessionsListResponse {
                sessions,
                next_after,
            })
        })
        .await
    }

    async fn purge_session(
        &self,
        params: api::DeploymentSessionPurgeParams,
    ) -> Result<api::DeploymentSessionPurgeResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_SESSIONS_PURGE, async {
            let universe_id = self.resolve_universe(&params.universe).await?;
            SessionId::try_new(&params.session_id)
                .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
            let deleted_session_ids = self
                .runtime
                .stores()
                .store_for(universe_id)
                .purge_deleted_session(&params.session_id)
                .await
                .map_err(|error| match error {
                    store_pg::PurgeSessionError::NotDeleted { .. } => {
                        AgentApiError::rejected(error.to_string())
                    }
                    store_pg::PurgeSessionError::Postgres(_) => {
                        AgentApiError::internal(error.to_string())
                    }
                })?;
            Ok(api::DeploymentSessionPurgeResponse {
                deleted_session_ids,
            })
        })
        .await
    }

    async fn list_deployment_provider_bindings(
        &self,
        params: api::DeploymentUniverseReadParams,
    ) -> Result<api::EnvironmentProviderBindingListResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_LIST, async {
            let universe_id = self.resolve_universe(&params.universe).await?;
            let bindings = self
                .runtime
                .stores()
                .store_for(universe_id)
                .list_provider_bindings(universe_id)
                .await
                .map_err(super::service::environment_providers::map_environments_error)?;
            let bindings = bindings
                .iter()
                .map(super::service::environment_providers::environment_provider_binding_view)
                .collect();
            Ok(api::EnvironmentProviderBindingListResponse { bindings })
        })
        .await
    }

    async fn create_universe(
        &self,
        params: DeploymentUniverseCreateParams,
    ) -> Result<DeploymentUniverseCreateResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_UNIVERSES_CREATE, async {
            let universe_id = parse_universe_id(&params.universe_id)?;
            if let Some(handle) = &params.handle {
                validate_universe_handle(handle)?;
            }
            let created = store_pg::create_universe_with_configuration(
                self.pool(),
                universe_id,
                params.handle.as_deref(),
                &params.metadata,
                params.protected,
                expiry_i64(params.expires_at_ms)?,
            )
            .await
            .map_err(map_universe_store_error)?;
            let universe = self.read_universe_view(universe_id).await?.ok_or_else(|| {
                AgentApiError::internal(format!("universe disappeared after create: {universe_id}"))
            })?;
            Ok(DeploymentUniverseCreateResponse { universe, created })
        })
        .await
    }

    async fn put_environment_provider(
        &self,
        params: DeploymentEnvironmentProviderPutParams,
    ) -> Result<DeploymentEnvironmentProviderPutResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_PUT, async {
            let store = self.runtime.stores().store_for(Uuid::nil());
            let provider_id = parse_environment_provider_id(params.provider_id)?;
            let controller_endpoint = EnvironmentEndpoint::parse(&params.controller_endpoint)
                .map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid controller endpoint: {error}"))
                })?;
            let data_route_base = params
                .data_route_base
                .map(DataRouteBase::new)
                .transpose()
                .map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid data route base: {error}"))
                })?;
            let credential = match (params.credential, params.clear_credential) {
                (Some(_), true) => {
                    return Err(AgentApiError::invalid_request(
                        "credential and clearCredential are mutually exclusive",
                    ));
                }
                (Some(credential), false) => {
                    let credential = SecretValue::new(credential);
                    environments::validate_provider_credential(&credential)
                        .map_err(super::service::environment_providers::map_environments_error)?;
                    ProviderCredentialChange::Set(credential)
                }
                (None, true) => ProviderCredentialChange::Clear,
                (None, false) => ProviderCredentialChange::Keep,
            };
            let request = PutEnvironmentProvider {
                provider_id: provider_id.clone(),
                display_name: params.display_name,
                controller_endpoint: controller_endpoint.clone(),
                data_route_base,
                credential,
                available_to_all_universes: params.available_to_all_universes,
                updated_at_ms: i64::try_from(current_time_ms()?)
                    .map_err(|_| AgentApiError::internal("current timestamp exceeds i64"))?,
            };
            // Refuse a configuration the store would refuse (a unix://
            // controller without a data route base, a downgrading base)
            // before dialing anything.
            request
                .clone()
                .into_record(None)
                .map_err(super::service::environment_providers::map_environments_error)?;
            // The handshake presents the credential the provider will hold.
            let presented = match &request.credential {
                ProviderCredentialChange::Set(credential) => Some(credential.clone()),
                ProviderCredentialChange::Clear => None,
                ProviderCredentialChange::Keep => {
                    match store.read_provider_credential(&provider_id).await {
                        Ok(stored) => stored,
                        Err(EnvironmentRegistryError::NotFound { .. }) => None,
                        Err(error) => {
                            return Err(
                                super::service::environment_providers::map_environments_error(
                                    error,
                                ),
                            );
                        }
                    }
                }
            };
            probe_controller(
                &WebSocketProviderControllerConnector,
                &controller_endpoint,
                presented.as_ref(),
            )
            .await?;
            let provider = store
                .put_provider(request)
                .await
                .map_err(super::service::environment_providers::map_environments_error)?;
            Ok(DeploymentEnvironmentProviderPutResponse {
                provider: environment_provider_view(&provider),
            })
        })
        .await
    }

    async fn list_environment_providers(
        &self,
        _params: DeploymentEnvironmentProviderListParams,
    ) -> Result<DeploymentEnvironmentProviderListResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_LIST, async {
            let store = self.runtime.stores().store_for(Uuid::nil());
            let providers = store
                .list_providers()
                .await
                .map_err(super::service::environment_providers::map_environments_error)?;
            Ok(DeploymentEnvironmentProviderListResponse {
                providers: providers.iter().map(environment_provider_view).collect(),
            })
        })
        .await
    }

    async fn read_environment_provider(
        &self,
        params: DeploymentEnvironmentProviderReadParams,
    ) -> Result<DeploymentEnvironmentProviderReadResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_READ, async {
            let store = self.runtime.stores().store_for(Uuid::nil());
            let provider_id = parse_environment_provider_id(params.provider_id)?;
            let provider = store
                .read_provider(&provider_id)
                .await
                .map_err(super::service::environment_providers::map_environments_error)?;
            Ok(DeploymentEnvironmentProviderReadResponse {
                provider: environment_provider_view(&provider),
            })
        })
        .await
    }

    async fn delete_environment_provider(
        &self,
        params: DeploymentEnvironmentProviderDeleteParams,
    ) -> Result<DeploymentEnvironmentProviderDeleteResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_DELETE, async {
            let store = self.runtime.stores().store_for(Uuid::nil());
            let provider_id = parse_environment_provider_id(params.provider_id)?;
            let provider = store
                .delete_provider(&provider_id)
                .await
                .map_err(super::service::environment_providers::map_environments_error)?;
            Ok(DeploymentEnvironmentProviderDeleteResponse {
                provider: environment_provider_view(&provider),
            })
        })
        .await
    }

    async fn put_environment_provider_binding(
        &self,
        params: DeploymentProviderBindingPutParams,
    ) -> Result<DeploymentProviderBindingPutResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_PUT, async {
            let universe_id = self.resolve_universe(&params.universe).await?;
            let store = self.runtime.stores().store_for(universe_id);
            let binding = store
                .put_provider_binding(PutEnvironmentProviderBinding {
                    universe_id,
                    provider_id: parse_environment_provider_id(params.provider_id)?,
                    updated_at_ms: i64::try_from(current_time_ms()?)
                        .map_err(|_| AgentApiError::internal("current timestamp exceeds i64"))?,
                })
                .await
                .map_err(super::service::environment_providers::map_environments_error)?;
            Ok(DeploymentProviderBindingPutResponse {
                binding: super::service::environment_providers::environment_provider_binding_view(
                    &binding,
                ),
            })
        })
        .await
    }

    async fn delete_environment_provider_binding(
        &self,
        params: DeploymentProviderBindingDeleteParams,
    ) -> Result<DeploymentProviderBindingDeleteResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_DELETE, async {
            let universe_id = self.resolve_universe(&params.universe).await?;
            let provider_id = parse_environment_provider_id(params.provider_id)?;
            let store = self.runtime.stores().store_for(universe_id);
            let binding = store
                .delete_provider_binding(universe_id, &provider_id)
                .await
                .map_err(super::service::environment_providers::map_environments_error)?;
            Ok(DeploymentProviderBindingDeleteResponse {
                binding: super::service::environment_providers::environment_provider_binding_view(
                    &binding,
                ),
            })
        })
        .await
    }

    async fn put_universe_configuration(
        &self,
        params: api::DeploymentUniverseConfigurationPutParams,
    ) -> Result<DeploymentUniverseReadResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_UNIVERSES_CONFIGURATION_PUT, async {
            params
                .configuration
                .validate()
                .map_err(AgentApiError::invalid_request)?;
            let id = self.resolve_universe(&params.universe).await?;
            if !store_pg::put_universe_configuration(self.pool(), id, &params.configuration)
                .await
                .map_err(map_store_error)?
            {
                return Err(AgentApiError::not_found("universe no longer exists"));
            }
            let universe = self
                .read_universe_view(id)
                .await?
                .ok_or_else(|| AgentApiError::not_found("universe no longer exists"))?;
            Ok(DeploymentUniverseReadResponse { universe })
        })
        .await
    }

    async fn put_universe_expiry(
        &self,
        params: api::DeploymentUniverseExpiryPutParams,
    ) -> Result<DeploymentUniverseReadResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_UNIVERSES_EXPIRY_PUT, async {
            let id = self.resolve_universe(&params.universe).await?;
            if !store_pg::put_universe_expiry(self.pool(), id, expiry_i64(params.expires_at_ms)?)
                .await
                .map_err(map_store_error)?
            {
                return Err(AgentApiError::not_found("universe no longer exists"));
            }
            let universe = self
                .read_universe_view(id)
                .await?
                .ok_or_else(|| AgentApiError::not_found("universe no longer exists"))?;
            Ok(DeploymentUniverseReadResponse { universe })
        })
        .await
    }

    async fn put_universe_handle(
        &self,
        params: api::DeploymentUniverseHandlePutParams,
    ) -> Result<DeploymentUniverseReadResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_UNIVERSES_HANDLE_PUT, async {
            validate_universe_handle(&params.handle)?;
            let id = self.resolve_universe(&params.universe).await?;
            let unknown = || AgentApiError::not_found(format!("unknown universe: {id}"));
            if !store_pg::put_universe_handle(self.pool(), id, &params.handle)
                .await
                .map_err(map_universe_store_error)?
            {
                return Err(unknown());
            }
            let universe = self.read_universe_view(id).await?.ok_or_else(unknown)?;
            Ok(DeploymentUniverseReadResponse { universe })
        })
        .await
    }

    async fn list_universes(
        &self,
        params: DeploymentUniverseListParams,
    ) -> Result<DeploymentUniverseListResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_UNIVERSES_LIST, async {
            let universes = store_pg::list_universe_stats_matching(self.pool(), &params.metadata)
                .await
                .map_err(map_store_error)?;
            Ok(DeploymentUniverseListResponse {
                universes: universes.into_iter().map(universe_view).collect(),
            })
        })
        .await
    }

    async fn list_deployment_channel_accounts(
        &self,
        params: api::DeploymentChannelAccountListParams,
    ) -> Result<api::DeploymentChannelAccountListResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_CHANNELS_ACCOUNTS_LIST, async {
            let accounts = store_pg::list_channel_accounts_all(
                self.pool(),
                params.provider,
                params.include_disabled,
            )
            .await
            .map_err(map_store_error)?;
            Ok(api::DeploymentChannelAccountListResponse {
                accounts: accounts
                    .into_iter()
                    .map(|(universe_id, record)| api::DeploymentChannelAccountView {
                        universe_id: universe_id.to_string(),
                        account: record.view(),
                    })
                    .collect(),
            })
        })
        .await
    }

    async fn read_universe(
        &self,
        params: DeploymentUniverseReadParams,
    ) -> Result<DeploymentUniverseReadResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_UNIVERSES_READ, async {
            let universe_id = match Uuid::parse_str(params.universe.trim()) {
                Ok(id) => id,
                Err(_) => self.resolve_universe(&params.universe).await?,
            };
            let universe = match self.read_universe_view(universe_id).await? {
                Some(view) => view,
                None => {
                    if let Some(record) = store_pg::read_deleted_universe(self.pool(), universe_id)
                        .await
                        .map_err(map_store_error)?
                    {
                        return Err(AgentApiError::gone(record));
                    }
                    return Err(AgentApiError::not_found(format!(
                        "unknown universe: {universe_id}"
                    )));
                }
            };
            Ok(DeploymentUniverseReadResponse { universe })
        })
        .await
    }

    async fn delete_universe(
        &self,
        params: DeploymentUniverseDeleteParams,
    ) -> Result<DeploymentUniverseDeleteResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_UNIVERSES_DELETE, async {
            let universe_id = match Uuid::parse_str(params.universe.trim()) {
                Ok(id) => id,
                Err(_) => self.resolve_universe(&params.universe).await?,
            };
            let requested_by = super::request_context::request_context()?.attribution();
            let state = store_pg::begin_universe_deletion(
                self.pool(),
                universe_id,
                &requested_by,
                current_time_ms()? as i64,
            )
            .await
            .map_err(map_store_error)?
            .ok_or_else(|| AgentApiError::not_found(format!("unknown universe: {universe_id}")))?;
            self.runtime
                .stores()
                .changes()
                .publish(store_pg::changes::Change::new(
                    universe_id,
                    store_pg::changes::ChangeKind::Universe,
                    universe_id.to_string(),
                ));
            self.runtime.evict(universe_id).await;
            Ok(DeploymentUniverseDeleteResponse {
                universe_id: universe_id.to_string(),
                state,
            })
        })
        .await
    }

    async fn list_deleted_universes(
        &self,
        params: api::DeploymentDeletedUniversesListParams,
    ) -> Result<api::DeploymentDeletedUniversesListResponse, AgentApiError> {
        self.admitted(api::METHOD_DEPLOYMENT_UNIVERSES_DELETED_LIST, async {
            if let Some(cursor) = &params.after {
                let valid = cursor.split_once(':').is_some_and(|(time, id)| {
                    time.parse::<i64>().is_ok() && Uuid::parse_str(id).is_ok()
                });
                if !valid {
                    return Err(AgentApiError::invalid_request(
                        "invalid deleted universe cursor",
                    ));
                }
            }
            store_pg::list_deleted_universes(self.pool(), params.after.as_deref())
                .await
                .map_err(map_store_error)
        })
        .await
    }
}

fn parse_universe_id(value: &str) -> Result<Uuid, AgentApiError> {
    Uuid::parse_str(value.trim())
        .map_err(|error| AgentApiError::invalid_request(format!("invalid universe id: {error}")))
}

fn parse_environment_provider_id(value: String) -> Result<EnvironmentProviderId, AgentApiError> {
    EnvironmentProviderId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid provider id: {error}")))
}

fn universe_view(stats: store_pg::UniverseStats) -> DeploymentUniverseView {
    DeploymentUniverseView {
        deletion: stats.deletion,
        configuration: stats.configuration,
        expires_at_ms: stats.expires_at_ms.map(|time| time as u64),
        universe_id: stats.universe_id.to_string(),
        handle: stats.handle,
        created_at_ms: u64::try_from(stats.created_at_ms).unwrap_or(0),
        last_activity_at_ms: stats
            .last_activity_at_ms
            .map(|value| u64::try_from(value).unwrap_or(0)),
        sessions: stats.sessions,
        workspaces: stats.workspaces,
        profiles: stats.profiles,
        blob_bytes: stats.blob_bytes,
    }
}

fn current_time_ms() -> Result<u64, AgentApiError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .map_err(|error| {
            AgentApiError::internal(format!("system clock before unix epoch: {error}"))
        })
}

fn validate_universe_handle(handle: &str) -> Result<(), AgentApiError> {
    if !store_pg::is_valid_universe_handle(handle) {
        return Err(AgentApiError::invalid_request(
            "a universe handle is 1-63 lowercase letters, digits and hyphens, starts and ends with a letter or digit, and is not in the form of a UUID",
        ));
    }
    Ok(())
}

fn map_universe_store_error(error: store_pg::PgStoreError) -> AgentApiError {
    if matches!(error, store_pg::PgStoreError::UniverseDeleted { .. }) {
        return AgentApiError::conflict(error.to_string());
    }
    if let store_pg::PgStoreError::Postgres(sqlx::Error::Database(ref db)) = error
        && db.is_unique_violation()
    {
        return AgentApiError::conflict("universe handle is already in use");
    }
    map_store_error(error)
}

fn map_store_error(error: store_pg::PgStoreError) -> AgentApiError {
    match error {
        store_pg::PgStoreError::UniverseProtected { .. }
        | store_pg::PgStoreError::UniverseProtectionExpiryConflict => {
            AgentApiError::rejected(error.to_string())
        }
        store_pg::PgStoreError::InvalidUniverseConfiguration { .. } => {
            AgentApiError::invalid_request(error.to_string())
        }
        _ => AgentApiError::internal(error.to_string()),
    }
}
fn expiry_i64(value: Option<u64>) -> Result<Option<i64>, AgentApiError> {
    value
        .map(|value| {
            i64::try_from(value).map_err(|_| {
                AgentApiError::invalid_request("expiresAtMs exceeds the supported timestamp range")
            })
        })
        .transpose()
}

/// A deployment method requires an explicit deployment request target.
fn admit(_method: &str) -> Result<super::request_context::RequestContext, AgentApiError> {
    let context = super::request_context::request_context()?;
    if context.scope != AccessScope::Deployment {
        return Err(AgentApiError::forbidden());
    }
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_the_store_refuses_is_an_invalid_request() {
        for handle in ["research", "team-2", "0"] {
            assert!(validate_universe_handle(handle).is_ok(), "{handle}");
        }
        for handle in [
            "",
            "Research",
            "my.universe",
            "-research",
            "00000000-0000-0000-0000-000000000001",
        ] {
            assert_eq!(
                validate_universe_handle(handle).unwrap_err().kind,
                api::AgentApiErrorKind::InvalidRequest,
                "{handle}"
            );
        }
    }
}

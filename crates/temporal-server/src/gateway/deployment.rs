//! Deployment-scoped (deployment-level) service: universe lifecycle over the
//! shared deployment resources, above the universe-bound store boundary.
//!
//! Purge ordering (`deployment/universes/delete`): terminate live session
//! workflows, sweep externally stored blob objects, delete the `universes`
//! row (every universe-scoped table cascades from it), then evict the cached
//! runtime state. Each step is idempotent and the row is deleted last, so a
//! partial failure leaves the universe visible and a re-run resumes where it
//! stopped. Callers stop routing traffic to the universe before purging (the
//! platform archives first); a write racing the purge can lazily re-insert an
//! empty universe row via `ensure_universe`, which a re-run removes.

use access::{AccessScope, AccessStore};
use std::sync::Arc;

use api::{
    AgentApiError, AgentApiOutcome, DeploymentApiKeyCreateParams, DeploymentApiKeyCreateResponse,
    DeploymentApiKeyListParams, DeploymentApiKeyListResponse, DeploymentApiKeyRevokeParams,
    DeploymentApiKeyRevokeResponse, DeploymentApiKeyView, DeploymentApiService,
    DeploymentEnvironmentAdoptParams, DeploymentEnvironmentAdoptResponse,
    DeploymentEnvironmentProviderConnection, DeploymentEnvironmentProviderDeleteParams,
    DeploymentEnvironmentProviderDeleteResponse, DeploymentEnvironmentProviderListParams,
    DeploymentEnvironmentProviderListResponse, DeploymentEnvironmentProviderPutParams,
    DeploymentEnvironmentProviderPutResponse, DeploymentEnvironmentProviderReadParams,
    DeploymentEnvironmentProviderReadResponse, DeploymentEnvironmentProviderTransport,
    DeploymentEnvironmentProviderView, DeploymentProviderBindingDeleteParams,
    DeploymentProviderBindingDeleteResponse, DeploymentProviderBindingPutParams,
    DeploymentProviderBindingPutResponse, DeploymentUniverseCreateParams,
    DeploymentUniverseCreateResponse, DeploymentUniverseDeleteParams,
    DeploymentUniverseDeleteResponse, DeploymentUniverseListParams, DeploymentUniverseListResponse,
    DeploymentUniverseReadParams, DeploymentUniverseReadResponse, DeploymentUniverseView,
};
use async_trait::async_trait;
use auth::ApiKeyStore as _;
use engine::SessionId;
use environment_protocol::shared::EnvironmentTransport;
use environments::{
    AdoptEnvironment, EnvironmentConnectionSpec, EnvironmentId, EnvironmentIncarnationId,
    EnvironmentProviderBindingId, EnvironmentProviderBindingStatus,
    EnvironmentProviderBindingStore, EnvironmentProviderId, EnvironmentProviderRecord,
    EnvironmentProviderStore, EnvironmentProvisionRequestId, EnvironmentStore,
    ListEnvironmentProviders, PutEnvironmentProvider, PutEnvironmentProviderBinding,
};
use object_store::ObjectStoreExt as _;
use object_store::path::Path as ObjectPath;
use temporal_workflow::{AgentSessionWorkflow, compose_workflow_id};
use temporalio_client::{Client, WorkflowTerminateOptions, errors::WorkflowInteractionError};
use uuid::Uuid;

use crate::universe::UniverseRuntime;

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

    fn temporal(&self) -> &Client {
        self.runtime.client()
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

    async fn require_universe(&self, universe_id: Uuid) -> Result<(), AgentApiError> {
        if store_pg::universe_exists(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?
        {
            Ok(())
        } else {
            Err(AgentApiError::not_found(format!(
                "unknown universe: {universe_id}"
            )))
        }
    }

    /// Terminate the session's live workflow. `NotFound` covers both "never
    /// started" and "already closed" and counts as nothing-to-terminate; any
    /// other failure aborts the purge before rows are touched (a purge that
    /// leaves a live workflow writing into a half-deleted universe is worse
    /// than a retryable error).
    async fn terminate_session_workflow(
        &self,
        universe_id: Uuid,
        session_id: &str,
    ) -> Result<bool, AgentApiError> {
        let session_id = SessionId::try_new(session_id).map_err(|error| {
            AgentApiError::internal(format!("stored session id is invalid: {error}"))
        })?;
        let handle =
            self.temporal()
                .get_workflow_handle::<AgentSessionWorkflow>(compose_workflow_id(
                    universe_id,
                    &session_id,
                ));
        match handle
            .terminate(
                WorkflowTerminateOptions::builder()
                    .reason("deployment universe purge")
                    .build(),
            )
            .await
        {
            Ok(()) => Ok(true),
            Err(WorkflowInteractionError::NotFound(_)) => Ok(false),
            Err(error) => Err(AgentApiError::internal(format!(
                "terminate session workflow {session_id}: {error}"
            ))),
        }
    }
}

#[async_trait]
impl DeploymentApiService for GatewayDeploymentApi {
    async fn apply_identity(
        &self,
        change: access::AccessChange,
    ) -> Result<AgentApiOutcome<access::AccessChangeResult>, AgentApiError> {
        let context = super::principal::request_context()?;
        if context.credential_scope != AccessScope::Deployment {
            return Err(AgentApiError::rejected("deployment credential required"));
        }
        store_pg::PgAccessStore::new(self.pool().clone())
            .apply(context.acting_principal.id, change, current_time_ms()?)
            .await
            .map(AgentApiOutcome::new)
            .map_err(|e| AgentApiError::rejected(e.to_string()))
    }

    async fn create_universe(
        &self,
        params: DeploymentUniverseCreateParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseCreateResponse>, AgentApiError> {
        let universe_id = parse_universe_id(&params.universe_id)?;
        let actor = super::principal::request_context()?.acting_principal.id;
        let created = store_pg::PgAccessStore::new(self.pool().clone())
            .apply(
                actor,
                access::AccessChange::CreateUniverse {
                    universe_id,
                    slug: None,
                },
                current_time_ms()?,
            )
            .await
            .map_err(|e| AgentApiError::rejected(e.to_string()))?
            .changed;
        let universe = self.read_universe_view(universe_id).await?.ok_or_else(|| {
            AgentApiError::internal(format!("universe disappeared after create: {universe_id}"))
        })?;
        Ok(AgentApiOutcome::new(DeploymentUniverseCreateResponse {
            universe,
            created,
        }))
    }

    async fn put_environment_provider(
        &self,
        params: DeploymentEnvironmentProviderPutParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderPutResponse>, AgentApiError> {
        let store = self.runtime.stores().store_for(Uuid::nil());
        let provider = store
            .put_provider(PutEnvironmentProvider {
                provider_id: parse_environment_provider_id(params.provider_id)?,
                display_name: params.display_name,
                controller_connection: provider_connection_from_api(params.controller_connection),
                metadata: params.metadata,
                updated_at_ms: i64::try_from(current_time_ms()?)
                    .map_err(|_| AgentApiError::internal("current timestamp exceeds i64"))?,
            })
            .await
            .map_err(super::service::environment_providers::map_environments_error)?;
        Ok(AgentApiOutcome::new(
            DeploymentEnvironmentProviderPutResponse {
                provider: environment_provider_view(provider),
            },
        ))
    }

    async fn list_environment_providers(
        &self,
        _params: DeploymentEnvironmentProviderListParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderListResponse>, AgentApiError> {
        let store = self.runtime.stores().store_for(Uuid::nil());
        let providers = store
            .list_providers(ListEnvironmentProviders::default())
            .await
            .map_err(super::service::environment_providers::map_environments_error)?;
        Ok(AgentApiOutcome::new(
            DeploymentEnvironmentProviderListResponse {
                providers: providers
                    .into_iter()
                    .map(environment_provider_view)
                    .collect(),
            },
        ))
    }

    async fn read_environment_provider(
        &self,
        params: DeploymentEnvironmentProviderReadParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderReadResponse>, AgentApiError> {
        let store = self.runtime.stores().store_for(Uuid::nil());
        let provider_id = parse_environment_provider_id(params.provider_id)?;
        let provider = store
            .read_provider(&provider_id)
            .await
            .map_err(super::service::environment_providers::map_environments_error)?;
        Ok(AgentApiOutcome::new(
            DeploymentEnvironmentProviderReadResponse {
                provider: environment_provider_view(provider),
            },
        ))
    }

    async fn delete_environment_provider(
        &self,
        params: DeploymentEnvironmentProviderDeleteParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderDeleteResponse>, AgentApiError> {
        let store = self.runtime.stores().store_for(Uuid::nil());
        let provider_id = parse_environment_provider_id(params.provider_id)?;
        let provider = store
            .delete_provider(&provider_id)
            .await
            .map_err(super::service::environment_providers::map_environments_error)?;
        Ok(AgentApiOutcome::new(
            DeploymentEnvironmentProviderDeleteResponse {
                provider: environment_provider_view(provider),
            },
        ))
    }

    async fn put_environment_provider_binding(
        &self,
        params: DeploymentProviderBindingPutParams,
    ) -> Result<AgentApiOutcome<DeploymentProviderBindingPutResponse>, AgentApiError> {
        let universe_id = parse_universe_id(&params.universe_id)?;
        self.require_universe(universe_id).await?;
        let store = self.runtime.stores().store_for(universe_id);
        let binding = store
            .put_provider_binding(PutEnvironmentProviderBinding {
                universe_id,
                binding_id: EnvironmentProviderBindingId::try_new(params.binding_id).map_err(
                    |error| AgentApiError::invalid_request(format!("invalid binding id: {error}")),
                )?,
                provider_id: EnvironmentProviderId::try_new(params.provider_id).map_err(
                    |error| AgentApiError::invalid_request(format!("invalid provider id: {error}")),
                )?,
                status: match params.status {
                    api::EnvironmentProviderBindingStatusView::Enabled => {
                        EnvironmentProviderBindingStatus::Enabled
                    }
                    api::EnvironmentProviderBindingStatusView::Disabled => {
                        EnvironmentProviderBindingStatus::Disabled
                    }
                },
                metadata: params.metadata,
                expected_revision: params.expected_revision,
                updated_at_ms: i64::try_from(current_time_ms()?)
                    .map_err(|_| AgentApiError::internal("current timestamp exceeds i64"))?,
            })
            .await
            .map_err(super::service::environment_providers::map_environments_error)?;
        Ok(AgentApiOutcome::new(DeploymentProviderBindingPutResponse {
            binding: super::service::environment_providers::environment_provider_binding_view(
                &binding,
            ),
        }))
    }

    async fn delete_environment_provider_binding(
        &self,
        params: DeploymentProviderBindingDeleteParams,
    ) -> Result<AgentApiOutcome<DeploymentProviderBindingDeleteResponse>, AgentApiError> {
        let universe_id = parse_universe_id(&params.universe_id)?;
        self.require_universe(universe_id).await?;
        let binding_id =
            EnvironmentProviderBindingId::try_new(params.binding_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid binding id: {error}"))
            })?;
        let store = self.runtime.stores().store_for(universe_id);
        let binding = store
            .delete_provider_binding(universe_id, &binding_id)
            .await
            .map_err(super::service::environment_providers::map_environments_error)?;
        Ok(AgentApiOutcome::new(
            DeploymentProviderBindingDeleteResponse {
                binding: super::service::environment_providers::environment_provider_binding_view(
                    &binding,
                ),
            },
        ))
    }

    async fn adopt_environment(
        &self,
        params: DeploymentEnvironmentAdoptParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentAdoptResponse>, AgentApiError> {
        if !params.take_ownership {
            return Err(AgentApiError::invalid_request(
                "takeOwnership must be true because adoption transfers lifecycle ownership to Lightspeed",
            ));
        }
        let universe_id = parse_universe_id(&params.universe_id)?;
        self.require_universe(universe_id).await?;
        let request_id =
            EnvironmentProvisionRequestId::try_new(params.request_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid environment request id: {error}"))
            })?;
        let binding_id =
            EnvironmentProviderBindingId::try_new(params.binding_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid binding id: {error}"))
            })?;
        validate_adoption_source(&params.source_target)?;
        let store = self.runtime.stores().store_for(universe_id);
        let environment = EnvironmentStore::adopt_environment(
            store.as_ref(),
            AdoptEnvironment {
                request_id,
                environment_id: EnvironmentId::new(format!(
                    "environment_{}",
                    Uuid::new_v4().simple()
                )),
                incarnation_id: EnvironmentIncarnationId::new(format!(
                    "incarnation_{}",
                    Uuid::new_v4().simple()
                )),
                binding_id,
                source_target: params.source_target,
                display_name: params.display_name,
                metadata: params.metadata,
                created_at_ms: i64::try_from(current_time_ms()?)
                    .map_err(|_| AgentApiError::internal("current timestamp exceeds i64"))?,
            },
        )
        .await
        .map_err(super::service::environment_providers::map_environments_error)?;
        Ok(AgentApiOutcome::new(DeploymentEnvironmentAdoptResponse {
            environment: super::service::environment_providers::environment_view(&environment),
        }))
    }

    async fn list_universes(
        &self,
        _params: DeploymentUniverseListParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseListResponse>, AgentApiError> {
        let universes = store_pg::list_universe_stats(self.pool())
            .await
            .map_err(map_store_error)?;
        Ok(AgentApiOutcome::new(DeploymentUniverseListResponse {
            universes: universes.into_iter().map(universe_view).collect(),
        }))
    }

    /// The connector host's discovery call: every enabled provider account
    /// of the deployment with its universe id.
    async fn list_deployment_channel_accounts(
        &self,
        params: api::DeploymentChannelAccountListParams,
    ) -> Result<AgentApiOutcome<api::DeploymentChannelAccountListResponse>, AgentApiError> {
        let accounts = store_pg::list_channel_accounts_all(
            self.pool(),
            params.provider,
            params.include_disabled,
        )
        .await
        .map_err(map_store_error)?;
        Ok(AgentApiOutcome::new(
            api::DeploymentChannelAccountListResponse {
                accounts: accounts
                    .into_iter()
                    .map(|(universe_id, record)| api::DeploymentChannelAccountView {
                        universe_id: universe_id.to_string(),
                        account: record.view(),
                    })
                    .collect(),
            },
        ))
    }

    async fn read_universe(
        &self,
        params: DeploymentUniverseReadParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseReadResponse>, AgentApiError> {
        let universe_id = parse_universe_id(&params.universe_id)?;
        let universe = self
            .read_universe_view(universe_id)
            .await?
            .ok_or_else(|| AgentApiError::not_found(format!("unknown universe: {universe_id}")))?;
        Ok(AgentApiOutcome::new(DeploymentUniverseReadResponse {
            universe,
        }))
    }

    async fn delete_universe(
        &self,
        params: DeploymentUniverseDeleteParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseDeleteResponse>, AgentApiError> {
        let universe_id = parse_universe_id(&params.universe_id)?;
        if !store_pg::universe_exists(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?
        {
            return Err(AgentApiError::not_found(format!(
                "unknown universe: {universe_id}"
            )));
        }

        let session_ids = store_pg::list_universe_session_ids(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?;
        let mut workflows_terminated = 0u64;
        for session_id in &session_ids {
            if self
                .terminate_session_workflow(universe_id, session_id)
                .await?
            {
                workflows_terminated += 1;
            }
        }

        let object_keys = store_pg::list_universe_object_keys(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?;
        let mut blob_objects_deleted = 0u64;
        if !object_keys.is_empty() {
            let Some(object_store) = self.runtime.stores().object_store() else {
                return Err(AgentApiError::internal(format!(
                    "universe {universe_id} has externally stored blobs but no object store is configured"
                )));
            };
            for key in &object_keys {
                match object_store.delete(&ObjectPath::from(key.as_str())).await {
                    Ok(()) => blob_objects_deleted += 1,
                    // Already swept by an earlier partial purge.
                    Err(object_store::Error::NotFound { .. }) => {}
                    Err(error) => {
                        return Err(AgentApiError::internal(format!(
                            "delete blob object {key}: {error}"
                        )));
                    }
                }
            }
        }

        store_pg::delete_universe(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?;
        // Objects whose catalog rows were already swept, or whose deletion
        // failed after the row went, are not in the listing above; clear the
        // universe's whole CAS prefix so nothing outlives the row cascade.
        if let Some(object_store) = self.runtime.stores().object_store() {
            let prefix = store_pg::universe_cas_object_prefix(
                self.runtime.stores().object_prefix(),
                universe_id,
            );
            match store_pg::delete_objects_under_prefix(object_store.as_ref(), &prefix).await {
                Ok(deleted) => blob_objects_deleted += deleted,
                Err(error) => tracing::warn!(
                    target: "temporal_server",
                    universe_id = %universe_id,
                    prefix,
                    %error,
                    "universe rows are deleted but clearing its object prefix failed"
                ),
            }
        }
        self.runtime.evict(universe_id).await;

        tracing::info!(
            target: "temporal_server",
            universe_id = %universe_id,
            sessions = session_ids.len(),
            workflows_terminated,
            blob_objects_deleted,
            "universe purged"
        );
        Ok(AgentApiOutcome::new(DeploymentUniverseDeleteResponse {
            universe_id: universe_id.to_string(),
            workflows_terminated,
            blob_objects_deleted,
        }))
    }

    async fn create_api_key(
        &self,
        params: DeploymentApiKeyCreateParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyCreateResponse>, AgentApiError> {
        let context = key_context(params.scope)?;
        let display_name = params.display_name.trim();
        if display_name.is_empty() {
            return Err(AgentApiError::invalid_request(
                "api key displayName must not be empty",
            ));
        }
        let store = store_pg::PgApiKeyStore::new(self.pool().clone());
        for _ in 0..3 {
            let minted = auth::mint_api_key(
                params.scope,
                params.principal_id,
                context.acting_principal.id,
                Some(display_name.to_owned()),
                current_time_ms()?,
            );
            match store
                .create_api_key(auth::CreateApiKey {
                    authority_scope: context.credential_scope,
                    key_hash: minted.key_hash,
                    record: minted.record.clone(),
                })
                .await
            {
                Ok(()) => {
                    return Ok(AgentApiOutcome::new(DeploymentApiKeyCreateResponse {
                        api_key: api_key_view(minted.record),
                        secret: minted.secret.expose().to_owned(),
                    }));
                }
                // A display-prefix collision is rare and entirely
                // server-generated, so retry instead of burdening callers.
                Err(auth::ApiKeyError::AlreadyExists { .. }) => continue,
                Err(error) => return Err(map_api_key_error(error)),
            }
        }
        Err(AgentApiError::internal(
            "could not allocate a unique api key prefix",
        ))
    }

    async fn list_api_keys(
        &self,
        params: DeploymentApiKeyListParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyListResponse>, AgentApiError> {
        let context = key_context(params.scope)?;
        let api_keys = store_pg::PgApiKeyStore::new(self.pool().clone())
            .list_managed_keys(
                context.acting_principal.id,
                context.credential_scope,
                params.scope,
            )
            .await
            .map_err(map_api_key_error)?
            .into_iter()
            .map(api_key_view)
            .collect();
        Ok(AgentApiOutcome::new(DeploymentApiKeyListResponse {
            api_keys,
        }))
    }

    async fn revoke_api_key(
        &self,
        params: DeploymentApiKeyRevokeParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyRevokeResponse>, AgentApiError> {
        let context = key_context(params.scope)?;
        let key_prefix = params.key_prefix.trim();
        if key_prefix.is_empty() {
            return Err(AgentApiError::invalid_request(
                "api key keyPrefix must not be empty",
            ));
        }
        let record = store_pg::PgApiKeyStore::new(self.pool().clone())
            .revoke_managed_key(
                context.acting_principal.id,
                context.credential_scope,
                params.scope,
                key_prefix,
                current_time_ms()?,
            )
            .await
            .map_err(map_api_key_error)?
            .ok_or_else(|| AgentApiError::not_found("unknown api key prefix"))?;
        Ok(AgentApiOutcome::new(DeploymentApiKeyRevokeResponse {
            api_key: api_key_view(record),
        }))
    }
}

fn parse_universe_id(value: &str) -> Result<Uuid, AgentApiError> {
    Uuid::parse_str(value.trim())
        .map_err(|error| AgentApiError::invalid_request(format!("invalid universe id: {error}")))
}

fn validate_adoption_source(value: &str) -> Result<(), AgentApiError> {
    if value.is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
        return Err(AgentApiError::invalid_request(
            "sourceTarget must be a non-empty provider-native reference of at most 255 bytes",
        ));
    }
    Ok(())
}

fn parse_environment_provider_id(value: String) -> Result<EnvironmentProviderId, AgentApiError> {
    EnvironmentProviderId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid provider id: {error}")))
}

fn provider_connection_from_api(
    connection: DeploymentEnvironmentProviderConnection,
) -> EnvironmentConnectionSpec {
    EnvironmentConnectionSpec {
        endpoint: connection.endpoint,
        transport: match connection.transport {
            DeploymentEnvironmentProviderTransport::WebSocket => EnvironmentTransport::WebSocket,
            DeploymentEnvironmentProviderTransport::Http => EnvironmentTransport::Http,
            DeploymentEnvironmentProviderTransport::Stdio => EnvironmentTransport::Stdio,
            DeploymentEnvironmentProviderTransport::Ssh => EnvironmentTransport::Ssh,
            DeploymentEnvironmentProviderTransport::Provider { provider_type } => {
                EnvironmentTransport::Provider { provider_type }
            }
        },
    }
}

fn environment_provider_view(
    record: EnvironmentProviderRecord,
) -> DeploymentEnvironmentProviderView {
    DeploymentEnvironmentProviderView {
        provider_id: record.provider_id.to_string(),
        display_name: record.display_name,
        controller_connection: DeploymentEnvironmentProviderConnection {
            endpoint: record.controller_connection.endpoint,
            transport: match record.controller_connection.transport {
                EnvironmentTransport::WebSocket => {
                    DeploymentEnvironmentProviderTransport::WebSocket
                }
                EnvironmentTransport::Http => DeploymentEnvironmentProviderTransport::Http,
                EnvironmentTransport::Stdio => DeploymentEnvironmentProviderTransport::Stdio,
                EnvironmentTransport::Ssh => DeploymentEnvironmentProviderTransport::Ssh,
                EnvironmentTransport::Provider { provider_type } => {
                    DeploymentEnvironmentProviderTransport::Provider { provider_type }
                }
            },
        },
        metadata: record.metadata,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

fn universe_view(stats: store_pg::UniverseStats) -> DeploymentUniverseView {
    DeploymentUniverseView {
        universe_id: stats.universe_id.to_string(),
        slug: stats.slug,
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

fn api_key_view(record: auth::ApiKeyRecord) -> DeploymentApiKeyView {
    DeploymentApiKeyView {
        key_prefix: record.key_prefix,
        scope: record.scope,
        principal_id: record.principal_id,
        created_by: record.created_by,
        display_name: record.display_name,
        created_at_ms: record.created_at_ms,
        revoked_at_ms: record.revoked_at_ms,
        last_used_at_ms: record.last_used_at_ms,
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

fn map_api_key_error(error: auth::ApiKeyError) -> AgentApiError {
    match error {
        auth::ApiKeyError::AlreadyExists { .. } => {
            AgentApiError::internal("generated api key prefix collision")
        }
        auth::ApiKeyError::Denied => AgentApiError::rejected("api key operation denied"),
        auth::ApiKeyError::Store { message } => AgentApiError::internal(message),
    }
}

fn map_store_error(error: store_pg::PgStoreError) -> AgentApiError {
    AgentApiError::internal(error.to_string())
}

fn key_context(scope: AccessScope) -> Result<access::RequestContext, AgentApiError> {
    let context = super::principal::request_context()?;
    if context.credential_scope != AccessScope::Deployment && context.credential_scope != scope {
        return Err(AgentApiError::rejected(
            "key scope exceeds credential scope",
        ));
    }
    // Assertions must be authorized for the scope whose keys are managed.
    if context.target_scope != scope
        && context.acting_principal.id != context.authenticated_principal.id
    {
        return Err(AgentApiError::rejected(
            "assertion scope does not match key scope",
        ));
    }
    Ok(context)
}

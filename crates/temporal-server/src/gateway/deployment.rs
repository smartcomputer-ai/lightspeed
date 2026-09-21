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

    /// Apply the same current-credential checks to direct calls as HTTP calls.
    /// Contextual identity/key policy remains transactional in the store. These
    /// records distinguish method admission from its eventual operation result.
    async fn audited<T>(
        &self,
        method: &str,
        target: Option<serde_json::Value>,
        operation: impl Future<Output = Result<T, AgentApiError>>,
    ) -> Result<T, AgentApiError> {
        super::audit::operation(async {
            let mut audit = super::audit::request(method, access::AuditStage::Admission);
            audit.target = target;
            let admission = async {
                let (context, rights) = super::authentication::current_context(self.pool()).await?;
                let requirement = api::method_access(method)
                    .ok_or_else(|| AgentApiError::rejected("unknown method"))?;
                let deployment_only = matches!(
                    requirement,
                    api::MethodAccess::DeploymentAdmin
                        | api::MethodAccess::DeploymentAdminOrCapability(_)
                );
                if (deployment_only
                    && (context.credential_scope != AccessScope::Deployment
                        || context.target_scope != AccessScope::Deployment))
                    || !super::authentication::method_permitted(&rights, requirement)
                {
                    return Err(AgentApiError::rejected("request is not authorized"));
                }
                audit.policy_revision = Some(rights.policy_revision);
                Ok(())
            }
            .await;
            if let Err(error) = admission {
                super::audit::failure(self.pool(), &mut audit, &error).await?;
                return Err(error);
            }
            // Inventories and self queries are quiet; mutations retain admission/outcome.
            let administrative = super::audit::significant(method);
            if administrative {
                super::audit::record(self.pool(), &mut audit).await?;
            }
            let result = operation.await;
            audit.stage = access::AuditStage::Completion;
            match &result {
                Err(error) if administrative || error.kind == api::AgentApiErrorKind::Rejected => {
                    super::audit::failure(self.pool(), &mut audit, error).await?
                }
                Err(_) => {}
                Ok(_) if administrative => {
                    audit.outcome = access::AuditOutcome::Succeeded;
                    super::audit::record(self.pool(), &mut audit).await?;
                }
                Ok(_) => {}
            }
            result
        })
        .await
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
    async fn list_deployment_provider_bindings(
        &self,
        params: api::DeploymentUniverseReadParams,
    ) -> Result<AgentApiOutcome<api::EnvironmentProviderBindingListResponse>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_LIST,
            Some(
                serde_json::json!({"universeId": Uuid::parse_str(params.universe_id.trim()).ok()}),
            ),
            async {
                let universe_id = parse_universe_id(&params.universe_id)?;
                self.require_universe(universe_id).await?;
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
                Ok(AgentApiOutcome::new(
                    api::EnvironmentProviderBindingListResponse { bindings },
                ))
            },
        )
        .await
    }

    async fn identity_self(
        &self,
        params: api::IdentityScopeParams,
    ) -> Result<AgentApiOutcome<api::IdentitySelfResponse>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_IDENTITY_SELF,
            Some(serde_json::json!({"scope": params.scope})),
            async {
                let (context, _) = super::authentication::current_context(self.pool()).await?;
                require_identity_scope(&context, params.scope)?;
                let store = store_pg::PgAccessStore::new(self.pool().clone());
                let access = store
                    .effective_access(context.acting_principal.id, params.scope)
                    .await
                    .map_err(identity_error)?;
                let mut universes = Vec::new();
                for universe_id in store
                    .accessible_universes(context.acting_principal.id)
                    .await
                    .map_err(identity_error)?
                {
                    let scope = AccessScope::Universe { universe_id };
                    let credential_allows = context.credential_scope == AccessScope::Deployment
                        || context.credential_scope == scope;
                    let assertion_allows = context.acting_principal.id
                        == context.authenticated_principal.id
                        || context.target_scope == AccessScope::Deployment
                        || context.target_scope == scope;
                    if credential_allows && assertion_allows {
                        universes.push(
                            store
                                .effective_access(context.acting_principal.id, scope)
                                .await
                                .map_err(identity_error)?,
                        );
                    }
                }
                Ok(AgentApiOutcome::new(api::IdentitySelfResponse {
                    access,
                    universes,
                }))
            },
        )
        .await
    }

    async fn identity_directory(
        &self,
        params: api::IdentityScopeParams,
    ) -> Result<AgentApiOutcome<api::AccessDirectory>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_IDENTITY_DIRECTORY,
            Some(serde_json::json!({"scope": params.scope})),
            async {
                let (context, _) = super::authentication::current_context(self.pool()).await?;
                require_identity_scope(&context, params.scope)?;
                store_pg::PgAccessStore::new(self.pool().clone())
                    .directory(context.acting_principal.id, params.scope)
                    .await
                    .map(AgentApiOutcome::new)
                    .map_err(identity_error)
            },
        )
        .await
    }

    async fn apply_identity(
        &self,
        change: access::AccessChange,
    ) -> Result<AgentApiOutcome<access::AccessChangeResult>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_IDENTITY_APPLY,
            Some(identity_audit_target(&change)),
            async {
                let (context, _) = super::authentication::current_context(self.pool()).await?;
                let scope = match &change {
                    access::AccessChange::AssignRole { assignment }
                    | access::AccessChange::RevokeRole { assignment }
                    | access::AccessChange::ReplaceRole { assignment, .. } => assignment.scope,
                    access::AccessChange::CreatePrincipal {
                        management_scope, ..
                    } => *management_scope,
                    _ => AccessScope::Deployment,
                };
                require_identity_scope(&context, scope)?;
                store_pg::PgAccessStore::new(self.pool().clone())
                    .apply(context.acting_principal.id, change, current_time_ms()?)
                    .await
                    .map(AgentApiOutcome::new)
                    .map_err(identity_error)
            },
        )
        .await
    }

    async fn create_universe(
        &self,
        params: DeploymentUniverseCreateParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseCreateResponse>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_UNIVERSES_CREATE,
            Some(
                serde_json::json!({"universeId": Uuid::parse_str(params.universe_id.trim()).ok()}),
            ),
            async {
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
                    .map_err(identity_error)?
                    .changed;
                let universe = self.read_universe_view(universe_id).await?.ok_or_else(|| {
                    AgentApiError::internal(format!(
                        "universe disappeared after create: {universe_id}"
                    ))
                })?;
                Ok(AgentApiOutcome::new(DeploymentUniverseCreateResponse {
                    universe,
                    created,
                }))
            },
        )
        .await
    }

    async fn put_environment_provider(
        &self,
        params: DeploymentEnvironmentProviderPutParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderPutResponse>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_PUT,
            Some(serde_json::json!({"providerId": audit_identifier(&params.provider_id)})),
            async {
                let store = self.runtime.stores().store_for(Uuid::nil());
                let provider = store
                    .put_provider(PutEnvironmentProvider {
                        provider_id: parse_environment_provider_id(params.provider_id)?,
                        display_name: params.display_name,
                        controller_connection: provider_connection_from_api(
                            params.controller_connection,
                        ),
                        metadata: params.metadata,
                        updated_at_ms: i64::try_from(current_time_ms()?).map_err(|_| {
                            AgentApiError::internal("current timestamp exceeds i64")
                        })?,
                    })
                    .await
                    .map_err(super::service::environment_providers::map_environments_error)?;
                Ok(AgentApiOutcome::new(
                    DeploymentEnvironmentProviderPutResponse {
                        provider: environment_provider_view(provider),
                    },
                ))
            },
        )
        .await
    }

    async fn list_environment_providers(
        &self,
        _params: DeploymentEnvironmentProviderListParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderListResponse>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_LIST,
            None,
            async {
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
            },
        )
        .await
    }

    async fn read_environment_provider(
        &self,
        params: DeploymentEnvironmentProviderReadParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderReadResponse>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_READ,
            Some(serde_json::json!({"providerId": audit_identifier(&params.provider_id)})),
            async {
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
            },
        )
        .await
    }

    async fn delete_environment_provider(
        &self,
        params: DeploymentEnvironmentProviderDeleteParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentProviderDeleteResponse>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_DELETE,
            Some(serde_json::json!({"providerId": audit_identifier(&params.provider_id)})),
            async {
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
            },
        )
        .await
    }

    async fn put_environment_provider_binding(
        &self,
        params: DeploymentProviderBindingPutParams,
    ) -> Result<AgentApiOutcome<DeploymentProviderBindingPutResponse>, AgentApiError> {
        self.audited(api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_PUT, Some(serde_json::json!({"universeId": Uuid::parse_str(params.universe_id.trim()).ok(), "bindingId": audit_identifier(&params.binding_id), "providerId": audit_identifier(&params.provider_id)})), async {
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
        }).await
    }

    async fn delete_environment_provider_binding(
        &self,
        params: DeploymentProviderBindingDeleteParams,
    ) -> Result<AgentApiOutcome<DeploymentProviderBindingDeleteResponse>, AgentApiError> {
        self.audited(api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_DELETE, Some(serde_json::json!({"universeId": Uuid::parse_str(params.universe_id.trim()).ok(), "bindingId": audit_identifier(&params.binding_id)})), async {
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
        }).await
    }

    async fn adopt_environment(
        &self,
        params: DeploymentEnvironmentAdoptParams,
    ) -> Result<AgentApiOutcome<DeploymentEnvironmentAdoptResponse>, AgentApiError> {
        self.audited(api::METHOD_DEPLOYMENT_ENVIRONMENTS_ADOPT, Some(serde_json::json!({"universeId": Uuid::parse_str(params.universe_id.trim()).ok(), "bindingId": audit_identifier(&params.binding_id), "requestId": audit_identifier(&params.request_id)})), async {
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
        }).await
    }

    async fn list_universes(
        &self,
        _params: DeploymentUniverseListParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseListResponse>, AgentApiError> {
        self.audited(api::METHOD_DEPLOYMENT_UNIVERSES_LIST, None, async {
            let universes = store_pg::list_universe_stats(self.pool())
                .await
                .map_err(map_store_error)?;
            Ok(AgentApiOutcome::new(DeploymentUniverseListResponse {
                universes: universes.into_iter().map(universe_view).collect(),
            }))
        })
        .await
    }

    async fn list_deployment_channel_accounts(
        &self,
        params: api::DeploymentChannelAccountListParams,
    ) -> Result<AgentApiOutcome<api::DeploymentChannelAccountListResponse>, AgentApiError> {
        self.audited(api::METHOD_DEPLOYMENT_CHANNELS_ACCOUNTS_LIST, None, async {
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
        })
        .await
    }

    async fn read_universe(
        &self,
        params: DeploymentUniverseReadParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseReadResponse>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_UNIVERSES_READ,
            Some(
                serde_json::json!({"universeId": Uuid::parse_str(params.universe_id.trim()).ok()}),
            ),
            async {
                let universe_id = parse_universe_id(&params.universe_id)?;
                let universe = self.read_universe_view(universe_id).await?.ok_or_else(|| {
                    AgentApiError::not_found(format!("unknown universe: {universe_id}"))
                })?;
                Ok(AgentApiOutcome::new(DeploymentUniverseReadResponse {
                    universe,
                }))
            },
        )
        .await
    }

    async fn delete_universe(
        &self,
        params: DeploymentUniverseDeleteParams,
    ) -> Result<AgentApiOutcome<DeploymentUniverseDeleteResponse>, AgentApiError> {
        self.audited(api::METHOD_DEPLOYMENT_UNIVERSES_DELETE, Some(serde_json::json!({"universeId": Uuid::parse_str(params.universe_id.trim()).ok()})), async {
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
        }).await
    }

    async fn create_api_key(
        &self,
        params: DeploymentApiKeyCreateParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyCreateResponse>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_API_KEYS_CREATE,
            Some(serde_json::json!({"scope": params.scope, "principalId": params.principal_id})),
            async {
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
            },
        )
        .await
    }

    async fn list_api_keys(
        &self,
        params: DeploymentApiKeyListParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyListResponse>, AgentApiError> {
        self.audited(
            api::METHOD_DEPLOYMENT_API_KEYS_LIST,
            Some(serde_json::json!({"scope": params.scope})),
            async {
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
            },
        )
        .await
    }

    async fn revoke_api_key(
        &self,
        params: DeploymentApiKeyRevokeParams,
    ) -> Result<AgentApiOutcome<DeploymentApiKeyRevokeResponse>, AgentApiError> {
        self.audited(api::METHOD_DEPLOYMENT_API_KEYS_REVOKE, Some(serde_json::json!({"scope": params.scope, "keyPrefix": audit_identifier(&params.key_prefix)})), async {
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
        }).await
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

fn require_identity_scope(
    context: &access::RequestContext,
    scope: AccessScope,
) -> Result<(), AgentApiError> {
    if context.credential_scope != AccessScope::Deployment && context.credential_scope != scope {
        return Err(AgentApiError::rejected(
            "identity scope exceeds credential scope",
        ));
    }
    if context.acting_principal.id != context.authenticated_principal.id
        && context.target_scope != AccessScope::Deployment
        && context.target_scope != scope
    {
        return Err(AgentApiError::rejected(
            "identity scope exceeds assertion scope",
        ));
    }
    Ok(())
}
fn identity_error(error: access::AccessError) -> AgentApiError {
    match error {
        access::AccessError::NotFound => AgentApiError::not_found(error.to_string()),
        access::AccessError::Conflict | access::AccessError::LastAdministrator { .. } => {
            AgentApiError::conflict(error.to_string())
        }
        access::AccessError::Store(_) => AgentApiError::internal(error.to_string()),
        _ => AgentApiError::rejected(error.to_string()),
    }
}

// Invalid or oversized identifiers are not copied into audit records.
fn audit_identifier(value: &str) -> Option<&str> {
    (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        .then_some(value)
}

fn identity_audit_target(change: &access::AccessChange) -> serde_json::Value {
    use access::AccessChange::*;
    use serde_json::json;
    let operation = match change {
        CreatePrincipal { .. } => "create_principal",
        SetPrincipalStatus { .. } => "set_principal_status",
        CreateGroup { .. } => "create_group",
        RenameGroup { .. } => "rename_group",
        PutMembership { .. } => "put_membership",
        RemoveMembership { .. } => "remove_membership",
        AssignRole { .. } => "assign_role",
        RevokeRole { .. } => "revoke_role",
        ReplaceRole { .. } => "replace_role",
        AssignCapability { .. } => "assign_capability",
        RevokeCapability { .. } => "revoke_capability",
        CreateUniverse { .. } => "create_universe",
        RecoverUniverse { .. } => "recover_universe",
    };
    let mut target = match change {
        CreatePrincipal { id, .. } | SetPrincipalStatus { id, .. } => json!({"principalId": id}),
        CreateGroup { id, .. } | RenameGroup { id, .. } => json!({"groupId": id}),
        PutMembership { membership } | RemoveMembership { membership } => {
            json!({"membership": membership})
        }
        AssignRole { assignment } | RevokeRole { assignment } => {
            json!({"roleAssignment": assignment})
        }
        ReplaceRole { assignment, role } => {
            json!({"roleAssignment": assignment, "role": role})
        }
        AssignCapability { assignment } | RevokeCapability { assignment } => {
            json!({"capabilityAssignment": assignment})
        }
        CreateUniverse { universe_id, .. } => json!({"universeId": universe_id}),
        RecoverUniverse {
            universe_id,
            principal_id,
        } => json!({"universeId": universe_id, "principalId": principal_id}),
    };
    target["operation"] = json!(operation);
    target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_audit_targets_exclude_display_data_and_distinguish_operations() {
        let id = Uuid::from_u128(1);
        let create = access::AccessChange::CreateGroup {
            id,
            display_name: "sensitive display data".into(),
        };
        let rename = access::AccessChange::RenameGroup {
            id,
            display_name: "another sensitive value".into(),
        };
        assert_eq!(
            identity_audit_target(&create),
            serde_json::json!({"operation":"create_group","groupId":id})
        );
        assert_eq!(
            identity_audit_target(&rename),
            serde_json::json!({"operation":"rename_group","groupId":id})
        );
        assert_eq!(audit_identifier("provider-1"), Some("provider-1"));
        assert_eq!(audit_identifier("unsafe\nidentifier"), None);
        assert_eq!(audit_identifier(&"x".repeat(257)), None);
    }
}

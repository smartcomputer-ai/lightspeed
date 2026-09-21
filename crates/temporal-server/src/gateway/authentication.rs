//! Authentication, credential ceilings, and authenticated user assertions.
use access::{
    AccessScope, AccessStore, AuthenticationReference, PrincipalKind, PrincipalStatus,
    RequestContext, Role, ServiceCapability,
};
use api::{AgentApiError, MethodAccess};
use auth::{ApiKeyStore, api_key_hash};
use axum::http::{HeaderMap, header};
use store_pg::{PgAccessStore, PgApiKeyStore};
use uuid::Uuid;

pub const UNIVERSE_HEADER: &str = "x-lightspeed-universe";
pub const PRINCIPAL_HEADER: &str = "x-lightspeed-principal";

fn denied() -> AgentApiError {
    AgentApiError::rejected("request is not authorized")
}
fn store_error(error: impl std::fmt::Display) -> AgentApiError {
    AgentApiError::internal(error.to_string())
}

/// Reject duplicates rather than allowing proxy/framework disagreement.
fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, AgentApiError> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(denied());
    }
    value.map(|v| v.to_str().map_err(|_| denied())).transpose()
}

pub fn reject_identity_headers(headers: &HeaderMap) -> Result<(), AgentApiError> {
    for name in [
        header::AUTHORIZATION.as_str(),
        UNIVERSE_HEADER,
        PRINCIPAL_HEADER,
    ] {
        if headers.contains_key(name) {
            return Err(AgentApiError::rejected(
                "identity headers are not accepted in single mode",
            ));
        }
    }
    Ok(())
}

pub async fn authenticate(
    keys: &PgApiKeyStore,
    access: &PgAccessStore,
    headers: &HeaderMap,
    method: &str,
    now_ms: u64,
) -> Result<RequestContext, AgentApiError> {
    let mut audit = super::audit::request(method, access::AuditStage::Authentication);
    // Authentication starts without trusting an outer task context or any header claims.
    audit.identity = Default::default();
    audit.actor = None;
    audit.scope = None;
    let result = authenticate_inner(keys, access, headers, method, now_ms, &mut audit).await;
    if let Err(error) = &result {
        audit.actor = audit
            .identity
            .acting_principal
            .map(|id| access::ActionActor::Principal { id });
        audit.occurred_at_ms = now_ms;
        super::audit::failed(&mut audit, error);
        access
            .record_audit_event(&audit)
            .await
            .map_err(store_error)?;
    }
    result
}

async fn authenticate_inner(
    keys: &PgApiKeyStore,
    access: &PgAccessStore,
    headers: &HeaderMap,
    method: &str,
    now_ms: u64,
    audit: &mut access::AuditEvent,
) -> Result<RequestContext, AgentApiError> {
    let method = api::method_access(method).ok_or_else(denied)?;
    let value = header_value(headers, header::AUTHORIZATION.as_str())?.ok_or_else(denied)?;
    let secret = value
        .strip_prefix("Bearer ")
        .filter(|s| s.starts_with("lsk_") && !s.chars().any(char::is_whitespace))
        .ok_or_else(denied)?;
    let key = keys
        .resolve_api_key(&api_key_hash(secret), now_ms)
        .await
        .map_err(store_error)?
        .ok_or_else(denied)?;
    audit.identity.authenticated_principal = Some(key.principal_id);
    audit.identity.credential = Some(AuthenticationReference::ApiKey {
        key_prefix: key.key_prefix.clone(),
    });
    audit.identity.credential_scope = Some(key.scope);
    let selected = header_value(headers, UNIVERSE_HEADER)?
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| denied())?;
    let target_scope = match method {
        MethodAccess::CredentialManagement | MethodAccess::Identity => {
            match (key.scope, selected) {
                (AccessScope::Universe { universe_id }, Some(id)) if id != universe_id => {
                    return Err(denied());
                }
                (_, Some(universe_id)) => AccessScope::Universe { universe_id },
                (scope, None) => scope,
            }
        }
        MethodAccess::Universe(_) | MethodAccess::Service(_) => match (key.scope, selected) {
            (AccessScope::Universe { universe_id }, None) => AccessScope::Universe { universe_id },
            (AccessScope::Universe { universe_id }, Some(id)) if id == universe_id => key.scope,
            (AccessScope::Deployment, Some(universe_id)) => AccessScope::Universe { universe_id },
            _ => return Err(denied()),
        },
        _ if key.scope == AccessScope::Deployment && selected.is_none() => AccessScope::Deployment,
        _ => return Err(denied()),
    };
    audit.scope = Some(target_scope);
    let authenticated_principal = access
        .principal(key.principal_id)
        .await
        .map_err(store_error)?
        .filter(|p| p.status == PrincipalStatus::Active)
        .ok_or_else(denied)?;
    let acting_principal = if let Some(asserted) = header_value(headers, PRINCIPAL_HEADER)? {
        let id = Uuid::parse_str(asserted.strip_prefix("user:").unwrap_or(asserted))
            .map_err(|_| denied())?;
        let scoped = access
            .effective_access(key.principal_id, target_scope)
            .await
            .map_err(store_error)?;
        let deployment_assertion = if key.scope == AccessScope::Deployment {
            access
                .effective_access(key.principal_id, AccessScope::Deployment)
                .await
                .map_err(store_error)?
                .has_capability(ServiceCapability::AssertUser)
        } else {
            false
        };
        if authenticated_principal.kind != PrincipalKind::Service
            || !(scoped.has_capability(ServiceCapability::AssertUser) || deployment_assertion)
        {
            return Err(denied());
        }
        access
            .principal(id)
            .await
            .map_err(store_error)?
            .filter(|p| p.kind == PrincipalKind::User && p.status == PrincipalStatus::Active)
            .ok_or_else(denied)?
    } else {
        authenticated_principal.clone()
    };
    audit.identity.acting_principal = Some(acting_principal.id);
    let rights = access
        .effective_access(acting_principal.id, target_scope)
        .await
        .map_err(store_error)?;
    if !method_permitted(&rights, method) {
        return Err(denied());
    }
    Ok(RequestContext {
        acting_principal,
        authenticated_principal,
        authentication: AuthenticationReference::ApiKey {
            key_prefix: key.key_prefix,
        },
        credential_scope: key.scope,
        target_scope,
    })
}

pub fn local_context(principal: access::Principal, target_scope: AccessScope) -> RequestContext {
    RequestContext {
        acting_principal: principal.clone(),
        authenticated_principal: principal,
        authentication: AuthenticationReference::LocalDevelopment,
        credential_scope: AccessScope::Deployment,
        target_scope,
    }
}

pub(super) fn method_permitted(rights: &access::EffectiveAccess, method: MethodAccess) -> bool {
    match method {
        // Contextual decisions are completed by the shared service after resolving ownership.
        MethodAccess::CredentialManagement | MethodAccess::Identity => rights.active(),
        MethodAccess::Universe(action) => {
            rights.universe_action(action) != access::RoleDecision::Denied
        }
        MethodAccess::Service(capability) => rights.has_capability(capability),
        MethodAccess::DeploymentAdmin => rights.has_role(Role::DeploymentAdmin),
        MethodAccess::DeploymentAdminOrCapability(capability) => {
            rights.has_role(Role::DeploymentAdmin) || rights.has_capability(capability)
        }
    }
}

/// Revalidate a captured context at shared-service admission, including revoked keys/assertion rights.
pub(crate) async fn current_context(
    pool: &sqlx::PgPool,
) -> Result<(RequestContext, access::EffectiveAccess), AgentApiError> {
    let context = super::principal::request_context()?;
    let scope = context.target_scope;
    if context.credential_scope != AccessScope::Deployment && context.credential_scope != scope {
        return Err(denied());
    }
    let store = PgAccessStore::new(pool.clone());
    let authenticated = store
        .principal(context.authenticated_principal.id)
        .await
        .map_err(store_error)?
        .filter(|p| p.status == access::PrincipalStatus::Active)
        .ok_or_else(denied)?;
    if let access::AuthenticationReference::ApiKey { ref key_prefix } = context.authentication {
        let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_keys WHERE key_prefix=$1 AND principal_id=$2 AND universe_id IS NOT DISTINCT FROM $3 AND revoked_at_ms IS NULL)")
                .bind(key_prefix).bind(authenticated.id)
                .bind(match context.credential_scope { access::AccessScope::Deployment => None, access::AccessScope::Universe { universe_id } => Some(universe_id) })
                .fetch_one(pool).await.map_err(|e| AgentApiError::internal(e.to_string()))?;
        if !active {
            return Err(denied());
        }
    }
    if authenticated.id != context.acting_principal.id {
        let scoped = store
            .effective_access(authenticated.id, scope)
            .await
            .map_err(store_error)?;
        let deployment = context.credential_scope == access::AccessScope::Deployment
            && store
                .effective_access(authenticated.id, access::AccessScope::Deployment)
                .await
                .map_err(store_error)?
                .has_capability(access::ServiceCapability::AssertUser);
        if !scoped.has_capability(access::ServiceCapability::AssertUser) && !deployment {
            return Err(denied());
        }
        if context.acting_principal.kind != access::PrincipalKind::User {
            return Err(denied());
        }
    }
    let rights = store
        .effective_access(context.acting_principal.id, scope)
        .await
        .map_err(store_error)?;
    if !rights.active() {
        return Err(denied());
    }
    Ok((context, rights))
}

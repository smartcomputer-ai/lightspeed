//! Authentication, credential ceilings, and authenticated user assertions.
//!
//! A request is resolved once at the transport boundary: the presented key and
//! its principal, the asserting service's capability, and the acting principal's
//! rights in the target scope. Handlers decide from the resulting context and
//! never re-resolve it; only parked requests revalidate, and then an unchanged
//! policy revision proves that every resolved fact still holds.
use access::{
    AccessScope, AccessStore, AuditEvent, AuditOutcome, AuthenticationReference, EffectiveAccess,
    PrincipalKind, RequestContext, Role,
};
use api::{AgentApiError, MethodAccess};
use auth::{ApiKeyStore, api_key_hash};
use axum::http::{HeaderMap, header};
use store_pg::{PgAccessStore, PgApiKeyStore};
use uuid::Uuid;

pub const UNIVERSE_HEADER: &str = "x-lightspeed-universe";
pub const PRINCIPAL_HEADER: &str = "x-lightspeed-principal";

/// A refused request. Callers that presented no valid credential are only
/// logged; a refused authenticated caller carries the verified identity facts
/// for its audit record.
#[derive(Debug)]
pub struct Refusal {
    pub error: AgentApiError,
    pub event: Option<Box<AuditEvent>>,
}

impl Refusal {
    fn unauthenticated() -> Self {
        Self {
            error: AgentApiError::unauthenticated(),
            event: None,
        }
    }

    fn store(error: impl std::fmt::Display) -> Self {
        Self {
            error: AgentApiError::internal(error.to_string()),
            event: None,
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for Refusal {}

impl From<AgentApiError> for Refusal {
    fn from(error: AgentApiError) -> Self {
        Self { error, event: None }
    }
}

/// Reject duplicates rather than allowing proxy/framework disagreement.
fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, Refusal> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(Refusal::unauthenticated());
    }
    value
        .map(|v| v.to_str().map_err(|_| Refusal::unauthenticated()))
        .transpose()
}

pub fn reject_identity_headers(headers: &HeaderMap) -> Result<(), AgentApiError> {
    for name in [
        header::AUTHORIZATION.as_str(),
        UNIVERSE_HEADER,
        PRINCIPAL_HEADER,
    ] {
        if headers.contains_key(name) {
            return Err(AgentApiError::invalid_request(
                "identity headers are not accepted in single mode",
            ));
        }
    }
    Ok(())
}

pub fn unknown_method() -> AgentApiError {
    AgentApiError::invalid_request("unknown method")
}

pub async fn authenticate(
    keys: &PgApiKeyStore,
    access: &PgAccessStore,
    headers: &HeaderMap,
    method: &str,
    now_ms: u64,
) -> Result<RequestContext, Refusal> {
    let requirement = api::method_access(method).ok_or_else(unknown_method)?;
    let secret = header_value(headers, header::AUTHORIZATION.as_str())?
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|s| s.starts_with("lsk_") && !s.chars().any(char::is_whitespace))
        .ok_or_else(Refusal::unauthenticated)?;
    let key = keys
        .resolve_api_key(&api_key_hash(secret), now_ms)
        .await
        .map_err(Refusal::store)?
        .ok_or_else(Refusal::unauthenticated)?;

    // The caller is authenticated from here on; refusals are attributable.
    let mut event = AuditEvent::new(method, AuditOutcome::Denied, now_ms);
    event.authenticated_principal = Some(key.principal.id);
    event.credential = Some(key.record.key_prefix.clone());
    let forbidden = |event: &AuditEvent| Refusal {
        error: AgentApiError::forbidden(),
        event: Some(Box::new(event.clone())),
    };

    let selected = match header_value(headers, UNIVERSE_HEADER)?.map(Uuid::parse_str) {
        Some(Ok(universe_id)) => Some(universe_id),
        Some(Err(_)) => return Err(forbidden(&event)),
        None => None,
    };
    let target_scope =
        target_scope(requirement, key.record.scope, selected).ok_or_else(|| forbidden(&event))?;
    event.scope = Some(target_scope);

    let acting = match header_value(headers, PRINCIPAL_HEADER)? {
        None => key.principal.id,
        Some(asserted) => {
            let id = Uuid::parse_str(asserted.strip_prefix("user:").unwrap_or(asserted))
                .map_err(|_| forbidden(&event))?;
            // An unauthorized assertion never attributes the claimed user.
            if !access
                .may_assert_user(key.principal.id, target_scope, key.record.scope)
                .await
                .map_err(Refusal::store)?
            {
                return Err(forbidden(&event));
            }
            id
        }
    };
    event.acting_principal = Some(acting);
    let mut rights = match access.effective_access(acting, target_scope).await {
        Ok(rights) => rights,
        Err(access::AccessError::NotFound) => return Err(forbidden(&event)),
        Err(error) => return Err(Refusal::store(error)),
    };
    // The key was resolved first, so its revision is the baseline: a change
    // committed after it, even between these statements, is seen as a change.
    rights.policy_revision = key.policy_revision;
    event.policy_revision = Some(rights.policy_revision);
    // Only people are asserted; a service never borrows another service.
    let asserted = acting != key.principal.id;
    if (asserted && rights.principal.kind != PrincipalKind::User)
        || !method_permitted(&rights, requirement)
    {
        return Err(forbidden(&event));
    }
    Ok(RequestContext {
        rights,
        authenticated_principal: key.principal,
        authentication: AuthenticationReference::ApiKey {
            key_prefix: key.record.key_prefix,
        },
        credential_scope: key.record.scope,
    })
}

/// The scope a request addresses: a key's scope is a ceiling, deployment keys
/// select a universe by header, and deployment methods take no universe.
fn target_scope(
    requirement: MethodAccess,
    key_scope: AccessScope,
    selected: Option<Uuid>,
) -> Option<AccessScope> {
    let selected = selected.map(|universe_id| AccessScope::Universe { universe_id });
    match requirement {
        MethodAccess::CredentialManagement | MethodAccess::Identity => {
            let target = selected.unwrap_or(key_scope);
            key_scope.permits(target).then_some(target)
        }
        MethodAccess::Universe(_) | MethodAccess::Service(_) => {
            let target = selected.or(match key_scope {
                AccessScope::Universe { .. } => Some(key_scope),
                AccessScope::Deployment => None,
            })?;
            key_scope.permits(target).then_some(target)
        }
        MethodAccess::DeploymentAdmin | MethodAccess::DeploymentAdminOrCapability(_) => {
            (key_scope == AccessScope::Deployment && selected.is_none())
                .then_some(AccessScope::Deployment)
        }
    }
}

/// A context without a presented credential, for the explicit local development
/// identity of `single` mode and for direct in-process callers. Rights are the
/// principal's committed rights in `scope`, exactly as for a key.
pub async fn local_context(
    access: &PgAccessStore,
    principal: Uuid,
    scope: AccessScope,
) -> Result<RequestContext, AgentApiError> {
    let rights = access
        .effective_access(principal, scope)
        .await
        .map_err(|error| match error {
            access::AccessError::NotFound => AgentApiError::forbidden(),
            error => AgentApiError::internal(error.to_string()),
        })?;
    Ok(RequestContext {
        authenticated_principal: rights.principal.clone(),
        rights,
        authentication: AuthenticationReference::LocalDevelopment,
        credential_scope: AccessScope::Deployment,
    })
}

/// The role- or capability-level decision for a method. Ownership-dependent
/// actions pass here and are completed by the handler that resolves the target.
pub fn method_permitted(rights: &EffectiveAccess, method: MethodAccess) -> bool {
    match method {
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

/// Keeps a parked request honest. Every identity, role, capability and key
/// change advances the policy revision, so one cheap read proves the resolved
/// context still holds; only a changed revision re-resolves it.
pub(crate) struct Revalidation {
    context: RequestContext,
}

impl Revalidation {
    pub(crate) fn new(context: RequestContext) -> Self {
        Self { context }
    }

    /// `Ok` while the caller may still perform `requirement`, on `resource`
    /// when one is parked on: a share revoked while a reader waits ends the
    /// wait, because every policy change advances the revision too.
    pub(crate) async fn check(
        &mut self,
        pool: &sqlx::PgPool,
        requirement: MethodAccess,
        resource: Option<&access::ResourceRef>,
    ) -> Result<(), AgentApiError> {
        let store_error =
            |error: &dyn std::fmt::Display| AgentApiError::internal(error.to_string());
        let access = PgAccessStore::new(pool.clone());
        let revision = access
            .policy_revision()
            .await
            .map_err(|e| store_error(&e))?;
        if revision == self.context.rights.policy_revision {
            return Ok(());
        }
        let context = &self.context;
        if let AuthenticationReference::ApiKey { key_prefix } = &context.authentication
            && !PgApiKeyStore::new(pool.clone())
                .key_is_active(
                    key_prefix,
                    context.authenticated_principal.id,
                    context.credential_scope,
                )
                .await
                .map_err(|e| store_error(&e))?
        {
            return Err(AgentApiError::unauthenticated());
        }
        if context.asserted()
            && !access
                .may_assert_user(
                    context.authenticated_principal.id,
                    context.target_scope(),
                    context.credential_scope,
                )
                .await
                .map_err(|e| store_error(&e))?
        {
            return Err(AgentApiError::forbidden());
        }
        let mut rights = access
            .effective_access(context.acting_principal().id, context.target_scope())
            .await
            .map_err(|error| match error {
                access::AccessError::NotFound => AgentApiError::forbidden(),
                error => store_error(&error),
            })?;
        if !method_permitted(&rights, requirement) {
            return Err(AgentApiError::forbidden());
        }
        if let (Some(resource), MethodAccess::Universe(action)) = (resource, requirement)
            && !access
                .decide(access::Caller::Request(&rights), action, resource)
                .await
                .map_err(|e| store_error(&e))?
                .is_some_and(access::Decision::allows)
        {
            return Err(AgentApiError::forbidden());
        }
        // Baseline on the revision read first, so a change that committed while
        // this re-resolution ran is caught by the next check.
        rights.policy_revision = revision;
        self.context.rights = rights;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use access::UniverseAction;

    fn universe(id: u128) -> AccessScope {
        AccessScope::Universe {
            universe_id: Uuid::from_u128(id),
        }
    }

    #[test]
    fn key_scope_is_a_ceiling_for_every_method_class() {
        let read = MethodAccess::Universe(UniverseAction::Read);
        // A universe key addresses only its universe, with or without a header.
        assert_eq!(target_scope(read, universe(1), None), Some(universe(1)));
        assert_eq!(
            target_scope(read, universe(1), Some(Uuid::from_u128(1))),
            Some(universe(1))
        );
        assert_eq!(
            target_scope(read, universe(1), Some(Uuid::from_u128(2))),
            None
        );
        // A deployment key must select a universe for universe methods.
        assert_eq!(target_scope(read, AccessScope::Deployment, None), None);
        assert_eq!(
            target_scope(read, AccessScope::Deployment, Some(Uuid::from_u128(2))),
            Some(universe(2))
        );
        // Deployment methods need a deployment key and take no universe.
        let admin = MethodAccess::DeploymentAdmin;
        assert_eq!(
            target_scope(admin, AccessScope::Deployment, None),
            Some(AccessScope::Deployment)
        );
        assert_eq!(
            target_scope(admin, AccessScope::Deployment, Some(Uuid::from_u128(2))),
            None
        );
        assert_eq!(target_scope(admin, universe(1), None), None);
        // Identity/key management follows the ceiling and defaults to the key's scope.
        let identity = MethodAccess::Identity;
        assert_eq!(target_scope(identity, universe(1), None), Some(universe(1)));
        assert_eq!(
            target_scope(identity, universe(1), Some(Uuid::from_u128(2))),
            None
        );
        assert_eq!(
            target_scope(identity, AccessScope::Deployment, Some(Uuid::from_u128(2))),
            Some(universe(2))
        );
        assert_eq!(
            target_scope(identity, AccessScope::Deployment, None),
            Some(AccessScope::Deployment)
        );
    }
}

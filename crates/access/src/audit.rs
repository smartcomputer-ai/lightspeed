//! Significant access decisions complement transactional identity changes.
//! Only trusted identity references and explicitly selected target identifiers belong here.
use crate::{AccessScope, AuthenticationReference, RequestContext};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Reject malformed/oversized references rather than copying arbitrary caller
/// strings into a durable security log before parameter validation runs.
pub fn auditable_resource(resource: &crate::ResourceRef) -> Option<&crate::ResourceRef> {
    let (crate::ResourceRef::Session(id)
    | crate::ResourceRef::Bot(id)
    | crate::ResourceRef::Profile(id)) = resource;
    (!id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)).then_some(resource)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditIdentity {
    pub authenticated_principal: Option<Uuid>,
    pub acting_principal: Option<Uuid>,
    pub credential: Option<AuthenticationReference>,
    pub credential_scope: Option<AccessScope>,
}

impl From<&RequestContext> for AuditIdentity {
    fn from(context: &RequestContext) -> Self {
        Self {
            authenticated_principal: Some(context.authenticated_principal.id),
            acting_principal: Some(context.acting_principal.id),
            credential: Some(context.authentication.clone()),
            credential_scope: Some(context.credential_scope),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditStage {
    Authentication,
    Admission,
    Completion,
    Delivery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Allowed,
    Denied,
    Succeeded,
    Failed,
}

/// An admission does not claim completion. Some actions have admission-only
/// auditing; where policy also requires completion, its absence means an unknown
/// outcome (cancellation, a crash, or failure to persist after a side effect).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    pub attempt_id: Uuid,
    /// A recognized API method, or None for an unknown method. Never raw input.
    pub method: Option<String>,
    pub identity: AuditIdentity,
    /// Explicit controller attribution for background work; no impersonated user.
    pub actor: Option<crate::ActionActor>,
    pub policy_revision: Option<u64>,
    pub scope: Option<AccessScope>,
    /// Resource identifiers only, never arbitrary metadata or a request body.
    pub target: Option<serde_json::Value>,
    pub stage: AuditStage,
    pub outcome: AuditOutcome,
    /// Stable error category, never an error message or provider response.
    pub error_kind: Option<String>,
    pub occurred_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourceRef;

    #[test]
    fn audit_references_omit_malformed_resource_strings() {
        for id in [String::new(), "x".repeat(257), "session\ncontent".into()] {
            assert!(auditable_resource(&ResourceRef::Session(id)).is_none());
        }
        let valid = ResourceRef::Session("bot:v1:example".into());
        assert_eq!(auditable_resource(&valid), Some(&valid));
    }
}

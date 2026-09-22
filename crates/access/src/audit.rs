//! Durable record of significant access decisions: one row per audited operation
//! or post-authentication denial. It complements the transactional identity
//! change log and holds trusted identity references and selected target
//! identifiers only, never request bodies, results or error messages.
use crate::{AccessScope, AuthenticationReference, RequestContext};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Malformed or oversized identifiers are dropped rather than copied into a
/// durable security log before parameter validation has run.
pub fn auditable_identifier(value: &str) -> Option<&str> {
    (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        .then_some(value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Succeeded,
    /// The operation was admitted and then failed; effects may be partial.
    Failed,
    /// An authenticated caller was refused.
    Denied,
}

impl AuditOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Denied => "denied",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    /// A recognized API method, never raw input.
    pub method: String,
    pub authenticated_principal: Option<Uuid>,
    pub acting_principal: Option<Uuid>,
    /// Non-secret credential reference: a key's display prefix.
    pub credential: Option<String>,
    pub scope: Option<AccessScope>,
    /// Resource identifiers only, never arbitrary metadata or a request body.
    pub target: Option<serde_json::Value>,
    /// Policy revision the decision was made under; joins the change log.
    pub policy_revision: Option<u64>,
    pub outcome: AuditOutcome,
    /// Stable error category, never an error message or provider response.
    pub error_kind: Option<String>,
    pub occurred_at_ms: u64,
}

impl AuditEvent {
    pub fn new(method: &str, outcome: AuditOutcome, occurred_at_ms: u64) -> Self {
        Self {
            method: method.to_owned(),
            authenticated_principal: None,
            acting_principal: None,
            credential: None,
            scope: None,
            target: None,
            policy_revision: None,
            outcome,
            error_kind: None,
            occurred_at_ms,
        }
    }

    pub fn with_context(mut self, context: &RequestContext) -> Self {
        self.authenticated_principal = Some(context.authenticated_principal.id);
        self.acting_principal = Some(context.acting_principal().id);
        self.credential = credential_reference(&context.authentication);
        self.scope = Some(context.target_scope());
        self.policy_revision = Some(context.rights.policy_revision);
        self
    }
}

pub fn credential_reference(authentication: &AuthenticationReference) -> Option<String> {
    match authentication {
        AuthenticationReference::ApiKey { key_prefix } => Some(key_prefix.clone()),
        AuthenticationReference::LocalDevelopment => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_identifiers_omit_malformed_strings() {
        for id in [String::new(), "x".repeat(257), "session\ncontent".into()] {
            assert!(auditable_identifier(&id).is_none());
        }
        assert_eq!(
            auditable_identifier("bot:v1:example"),
            Some("bot:v1:example")
        );
    }
}

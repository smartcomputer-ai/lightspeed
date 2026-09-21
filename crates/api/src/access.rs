//! Required authorization classification of every public method. These are
//! contract metadata; transport authentication and target-dependent enforcement
//! belong in the runtime. A role decision requiring ownership is not permission.

use super::*;
pub use ::access::{
    AccessChange, AccessChangeResult, AccessDirectory, AccessScope, CapabilityAssignment,
    EffectiveAccess, Group as IdentityGroup, Membership, Principal as IdentityPrincipal,
    PrincipalKind as IdentityPrincipalKind, PrincipalStatus, ResourceRef, Role as AccessRole,
    RoleAssignment, RoleDecision, ServiceCapability, Subject, UniverseAction,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "requirement", rename_all = "snake_case")]
pub enum MethodAccess {
    /// A current universe role permitting the action, with authoritative ownership where required.
    Universe(UniverseAction),
    /// An explicitly scoped service capability; service kind is insufficient.
    Service(ServiceCapability),
    DeploymentAdmin,
    /// Authenticated identity; key ownership and issuance rules apply in the handler.
    CredentialManagement,
    /// Authenticated caller; handler enforces requested identity scope and operation.
    Identity,
    /// Deployment discovery serves both administration and scoped connectors.
    DeploymentAdminOrCapability(ServiceCapability),
}

impl MethodAccess {
    pub const fn scope(self) -> MethodScope {
        match self {
            Self::Universe(_) => MethodScope::Universe,
            Self::Service(_) => MethodScope::Service,
            Self::CredentialManagement
            | Self::Identity
            | Self::DeploymentAdmin
            | Self::DeploymentAdminOrCapability(_) => MethodScope::Deployment,
        }
    }
}

/// Find the declared requirement. Unknown methods never inherit a default.
pub fn method_access(method: &str) -> Option<MethodAccess> {
    crate::rpc::universe_method_access(method)
        .or_else(|| crate::deployment::deployment_method_access(method))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_method_has_explicit_access_and_unknown_names_have_none() {
        for spec in crate::schema_export::full_method_manifest() {
            assert_eq!(method_access(spec.method), Some(spec.access));
            assert_eq!(spec.access.scope(), spec.scope);
            assert_eq!(
                is_service_method(spec.method),
                spec.scope == MethodScope::Service
            );
        }
        for method in [
            "operator/universes/create",
            "deployment/missing",
            "session/missing",
            "",
        ] {
            assert_eq!(method_access(method), None);
            assert!(!is_service_method(method));
        }
    }

    #[test]
    fn control_and_stop_have_distinct_requirements_and_services_require_capabilities() {
        for method in [
            METHOD_SESSION_CONFIG_PUT,
            METHOD_SESSION_RUNS_START,
            METHOD_SESSION_RUNS_STEER,
            METHOD_SESSION_RUNS_APPROVALS_DECIDE,
        ] {
            assert_eq!(
                method_access(method),
                Some(MethodAccess::Universe(UniverseAction::ControlSession))
            );
        }
        assert_eq!(
            method_access(METHOD_SESSION_RUNS_CANCEL),
            Some(MethodAccess::Universe(UniverseAction::StopSession))
        );
        assert_eq!(
            method_access(METHOD_AUTH_GRANTS_LEASE),
            Some(MethodAccess::Service(ServiceCapability::LeaseCredentials))
        );
        assert_eq!(
            method_access(METHOD_CHANNELS_INBOUND_ADMIT),
            Some(MethodAccess::Service(
                ServiceCapability::AdmitChannelInbound
            ))
        );
        assert_eq!(
            method_access(METHOD_DEPLOYMENT_CHANNELS_ACCOUNTS_LIST),
            Some(MethodAccess::DeploymentAdminOrCapability(
                ServiceCapability::DiscoverChannelAccounts
            ))
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityScopeParams {
    pub scope: AccessScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IdentitySelfResponse {
    pub access: EffectiveAccess,
    pub universes: Vec<EffectiveAccess>,
}

/// Current caller's action permissions. This is an advisory snapshot: mutations
/// always authorize again, and runtime prerequisites still apply.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessReadParams {
    /// Up to 100 existing sessions, bots or profiles in the selected universe.
    /// Missing resources return no actions.
    #[serde(default)]
    pub resources: Vec<ResourceRef>,
    /// Include every retention descendant when previewing session deletion.
    #[serde(default)]
    pub session_delete_cascade: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAccessView {
    pub resource: ResourceRef,
    pub actions: Vec<UniverseAction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessReadResponse {
    /// Allowed actions that do not require a target. Resource actions are
    /// returned only under `resources`, even when the caller has a broad role.
    pub actions: Vec<UniverseAction>,
    pub resources: Vec<ResourceAccessView>,
}

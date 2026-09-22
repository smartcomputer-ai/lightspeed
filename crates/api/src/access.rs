//! Required authorization classification of every public method. These are
//! contract metadata; transport authentication and target-dependent enforcement
//! belong in the runtime. A role decision requiring ownership is not permission.

use super::*;
pub use ::access::{
    AccessChange, AccessChangeResult, AccessDirectory, AccessScope, ActionActor, Capability,
    CapabilityAssignment, EffectiveAccess, Execution, ExecutionKind, Group as IdentityGroup,
    Membership, Principal as IdentityPrincipal, PrincipalKind as IdentityPrincipalKind,
    PrincipalStatus, ResourceAccessSummary, ResourceGrant, ResourcePermission, ResourceRef,
    Role as AccessRole, RoleAssignment, RoleDecision, Subject, UniverseAction,
    UniverseExecutionPolicy, Visibility,
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "requirement", rename_all = "snake_case")]
pub enum MethodAccess {
    /// A current universe role permitting the action, with authoritative ownership where required.
    Universe(UniverseAction),
    /// An explicitly scoped service capability; service kind is insufficient.
    Service(Capability),
    DeploymentAdmin,
    /// Authenticated identity; key ownership and issuance rules apply in the handler.
    CredentialManagement,
    /// Authenticated caller; handler enforces requested identity scope and operation.
    Identity,
    /// Deployment discovery serves both administration and scoped connectors.
    DeploymentAdminOrCapability(Capability),
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

/// Whether the method declares a durable audit record for each call.
pub fn method_audited(method: &str) -> bool {
    crate::rpc::universe_method_audited(method)
        .or_else(|| crate::deployment::deployment_method_audited(method))
        .unwrap_or(false)
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
    fn audit_policy_is_declared_per_method_and_reads_stay_quiet() {
        let manifest = crate::schema_export::full_method_manifest();
        for spec in &manifest {
            assert_eq!(method_audited(spec.method), spec.audited);
            // A read never leaves a record; neither does credential leasing,
            // which workers perform continuously.
            if matches!(
                spec.access,
                MethodAccess::Universe(UniverseAction::Read) | MethodAccess::Service(_)
            ) {
                assert!(!spec.audited, "{}", spec.method);
            }
        }
        // Destructive and access-changing operations always do.
        for method in [
            METHOD_SESSION_DELETE,
            METHOD_BOTS_DELETE,
            METHOD_DEPLOYMENT_UNIVERSES_DELETE,
            METHOD_DEPLOYMENT_IDENTITY_APPLY,
            METHOD_DEPLOYMENT_API_KEYS_CREATE,
            METHOD_DEPLOYMENT_API_KEYS_REVOKE,
        ] {
            assert!(method_audited(method), "{method}");
        }
        assert!(manifest.iter().any(|spec| spec.audited));
        assert!(!method_audited("session/missing"));
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
            Some(MethodAccess::Service(Capability::LeaseCredentials))
        );
        assert_eq!(
            method_access(METHOD_CHANNELS_INBOUND_ADMIT),
            Some(MethodAccess::Service(Capability::AdmitChannelInbound))
        );
        assert_eq!(
            method_access(METHOD_DEPLOYMENT_CHANNELS_ACCOUNTS_LIST),
            Some(MethodAccess::DeploymentAdminOrCapability(
                Capability::DiscoverChannelAccounts
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

/// Audience of a root, requested at creation. A session or bot created under
/// another root (a bot's session, a delegated child) has no audience of its
/// own and refuses this.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessInput {
    /// `universe` lets every member read; `restricted` limits reading to the
    /// owner and the grants below. Absent means `universe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    /// Readers and writers of the root. Each subject must currently hold a
    /// role in the universe.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<AccessGrantInput>,
    /// Create the resource as a member of this collection, which the caller
    /// must be able to write to. A member shares the collection's audience
    /// and takes no `visibility` or `grants` of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<ResourceRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessGrantInput {
    pub subject: Subject,
    pub permission: ResourcePermission,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessPolicyReadParams {
    pub resource: ResourceRef,
}

/// The policy governing a resource: its root's owner, visibility and grants.
/// A resource below a root (a bot's session, a delegated child) shows its
/// root's policy; sharing it means sharing the root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessPolicyView {
    pub resource: ResourceRef,
    /// The root whose policy this is; equal to `resource` for a root.
    pub root: ResourceRef,
    pub owner: Uuid,
    pub visibility: Visibility,
    /// Who the root's work runs as; absent for a profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<Execution>,
    pub grants: Vec<ResourceGrant>,
    /// Advances with every replacement; pass it as `expectedRevision`.
    pub revision: u64,
    pub updated_by: ActionActor,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessPolicyReadResponse {
    pub policy: AccessPolicyView,
}

/// Replace a root's visibility and grant set. Only the owner may grant
/// `write`; writers may share `read` and change visibility; readers change
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessPolicyPutParams {
    pub resource: ResourceRef,
    pub visibility: Visibility,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<AccessGrantInput>,
    /// Hand the root to another member of the universe. Only the current
    /// owner may set it; the previous owner keeps no permission of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Uuid>,
    /// The revision from `access/policy/read`; absent replaces unconditionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessPolicyPutResponse {
    pub policy: AccessPolicyView,
}

/// A collection: a root with a name that gives the sessions and bots in it
/// one audience. It routes nothing and runs nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionView {
    pub collection_id: String,
    pub display_name: String,
    /// Advances with every update; pass it as `expectedRevision`.
    pub revision: u64,
    pub access: ResourceAccessSummary,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionCreateParams {
    /// Client-chosen id (same form as a session id); absent allocates one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
    pub display_name: String,
    /// Audience of the collection; `root` is not accepted here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessInput>,
    /// Execution identity of the collection and everything created in it;
    /// absent means the universe's execution service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionCreateResponse {
    pub collection: CollectionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionReadParams {
    pub collection_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionReadResponse {
    pub collection: CollectionView,
    /// Sessions and bots whose audience this collection is.
    pub members: Vec<ResourceRef>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionListResponse {
    pub collections: Vec<CollectionView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionUpdateParams {
    pub collection_id: String,
    pub display_name: String,
    /// The revision from `collection/read`; absent replaces unconditionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionUpdateResponse {
    pub collection: CollectionView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionDeleteParams {
    pub collection_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDeleteResponse {
    pub collection: CollectionView,
}

/// Requested execution identity of a new root. `service` runs as the
/// universe's execution service; `personal` runs as the creating person and
/// needs the universe to allow it. Absent means `service`. A resource
/// created under another root inherits and refuses this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionInput {
    pub kind: ExecutionKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessExecutionReadParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessExecutionReadResponse {
    pub policy: UniverseExecutionPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessExecutionUpdateParams {
    pub personal_execution_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessExecutionUpdateResponse {
    pub policy: UniverseExecutionPolicy,
}

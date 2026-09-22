//! Deployment identity and authorization contracts. No credentials or I/O.
//!
//! Platform authenticates people and maps external identities to these stable
//! records. Stores supply current facts; callers must satisfy contextual
//! requirements before treating a role decision as permission to act.

use std::collections::BTreeSet;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum AccessScope {
    Deployment,
    Universe { universe_id: Uuid },
}

impl AccessScope {
    /// A credential's scope is a ceiling: deployment credentials reach every
    /// scope, universe credentials only their own universe.
    pub fn permits(self, target: AccessScope) -> bool {
        self == AccessScope::Deployment || self == target
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "IdentityPrincipalKind")]
pub enum PrincipalKind {
    User,
    Service,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalStatus {
    Active,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename = "IdentityPrincipal")]
pub struct Principal {
    pub id: Uuid,
    pub kind: PrincipalKind,
    pub status: PrincipalStatus,
    pub display_name: String,
    /// Who may manage this service identity; it does not grant access itself.
    /// Human identities are deployment-managed.
    pub management_scope: AccessScope,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename = "IdentityGroup")]
pub struct Group {
    pub id: Uuid,
    pub display_name: String,
    pub created_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Membership {
    pub group_id: Uuid,
    pub principal_id: Uuid,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Subject {
    Principal(Uuid),
    Group(Uuid),
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Contributor,
    Operator,
    Admin,
    DeploymentAdmin,
}

impl Role {
    pub fn valid_in(self, scope: AccessScope) -> bool {
        matches!(
            (self, scope),
            (Self::DeploymentAdmin, AccessScope::Deployment)
        ) || (self != Self::DeploymentAdmin && matches!(scope, AccessScope::Universe { .. }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoleAssignment {
    pub scope: AccessScope,
    pub subject: Subject,
    pub role: Role,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ServiceCapability {
    AssertUser,
    LeaseCredentials,
    AdmitChannelInbound,
    DiscoverChannelAccounts,
    ManageIdentity,
}

impl ServiceCapability {
    pub fn valid_in(self, scope: AccessScope) -> bool {
        match self {
            Self::AssertUser => true,
            Self::LeaseCredentials | Self::AdmitChannelInbound => {
                matches!(scope, AccessScope::Universe { .. })
            }
            Self::DiscoverChannelAccounts | Self::ManageIdentity => {
                scope == AccessScope::Deployment
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityAssignment {
    pub scope: AccessScope,
    pub principal_id: Uuid,
    pub capability: ServiceCapability,
}

/// Actions are independent of RPC spelling. Ownership and resource policy are
/// evaluated by the service that resolves the target, not from client claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UniverseAction {
    Read,
    CreateSession,
    ControlSession,
    StopSession,
    DeleteSession,
    CreateProfile,
    ManageProfile,
    CreateBot,
    ManageBot,
    InvokeBot,
    UseResource,
    ConfigureResource,
    ManageAccess,
    /// Change who may see or control a root: its visibility and grants.
    ShareResource,
    CreateCollection,
    ManageCollection,
    DeleteCollection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveAccess {
    pub principal: Principal,
    pub scope: AccessScope,
    /// Only assignments in this exact scope, including group-derived roles.
    pub roles: BTreeSet<Role>,
    /// Explicit service capabilities in this exact scope; no role implies one.
    pub capabilities: BTreeSet<ServiceCapability>,
    pub policy_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleDecision {
    Allowed,
    /// Not an allowance. Resolve authoritative ownership/controller lineage.
    RequiresOwnership,
    Denied,
}

/// Durable control facts, distinct from an agent's execution credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ResourceRef {
    Session(String),
    Bot(String),
    Profile(String),
    /// A root that gives sessions and bots one audience and, later, one
    /// execution identity. It routes nothing and runs nothing.
    Collection(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ActionActor {
    Principal { id: Uuid },
    Internal { component: String, cause: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ResourceController {
    Principal(Uuid),
    Bot(String),
    /// A separately admitted delegation, never inferred from provenance.
    Session(String),
}

/// Who may see a root's tree without a grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Universe,
    Restricted,
}

/// One permission a grant confers on a root. `Read` sees the tree; `Write`
/// also controls it.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ResourcePermission {
    Read,
    Write,
}

/// Immutable facts of one governed resource, reserved before it exists. The
/// audience root and managing bot are copied from the admitted controller at
/// reservation, so a decision reads one row instead of walking a lineage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAnchor {
    pub resource: ResourceRef,
    pub created_by: ActionActor,
    /// The immediate, separately admitted controller.
    pub controller: ResourceController,
    /// The root whose policy governs this resource; itself for a root.
    pub audience_root: ResourceRef,
    /// The bot whose worker controls this resource, if any.
    pub bot: Option<String>,
    pub created_at_ms: u64,
}

impl ResourceAnchor {
    pub fn is_root(&self) -> bool {
        self.audience_root == self.resource
    }
}

/// What can change about a root: its current owner and visibility. The
/// revision guards replacement of the policy and its grants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourcePolicy {
    pub owner: Uuid,
    pub visibility: Visibility,
    pub revision: u64,
    pub updated_by: ActionActor,
    pub updated_at_ms: u64,
}

/// One grant on a root as stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGrant {
    pub subject: Subject,
    pub permission: ResourcePermission,
    pub granted_by: Uuid,
    pub granted_at_ms: u64,
}

/// Everything one decision about one resource needs, loaded in one statement:
/// the anchor, its root's policy, and the caller's best grant on that root.
/// A missing policy denies: the root was never admitted or is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceAccess {
    pub anchor: ResourceAnchor,
    pub policy: Option<ResourcePolicy>,
    pub grant: Option<ResourcePermission>,
}

/// Authority of the runtime's own work for an admitted bot or delegated
/// session. It holds no roles and is not a bypass: it reads its own root and
/// universe-visible content, creates in its own root, and controls only
/// itself, its bot's sessions and the children it admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControllerContext {
    pub universe_id: Uuid,
    pub actor: ResourceRef,
    /// The actor's audience root, resolved when the context is built.
    pub root: ResourceRef,
    pub cause: String,
}

/// Who asks: a request resolved at the trusted boundary, or internal work.
#[derive(Clone, Copy, Debug)]
pub enum Caller<'a> {
    Request(&'a EffectiveAccess),
    Controller(&'a ControllerContext),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// The caller may see the resource but not do this.
    Forbidden,
    /// The caller may not see the resource at all; it should look absent.
    Hidden,
}

/// The one evaluator for resource decisions. Roles decide first; a resource
/// decision then reads the loaded access facts and never storage.
pub fn authorize(
    caller: Caller<'_>,
    action: UniverseAction,
    resource: Option<&ResourceAccess>,
) -> Decision {
    match caller {
        Caller::Request(rights) => authorize_request(rights, action, resource),
        Caller::Controller(context) => authorize_controller(context, action, resource),
    }
}

fn authorize_request(
    rights: &EffectiveAccess,
    action: UniverseAction,
    resource: Option<&ResourceAccess>,
) -> Decision {
    use UniverseAction::*;
    let role = rights.universe_action(action);
    let Some(access) = resource else {
        return if role == RoleDecision::Allowed {
            Decision::Allowed
        } else {
            Decision::Forbidden
        };
    };
    let Some(policy) = &access.policy else {
        return Decision::Hidden;
    };
    let owner = policy.owner == rights.principal.id;
    let universe_visible = policy.visibility == Visibility::Universe;
    let readable = rights.universe_action(Read) == RoleDecision::Allowed
        && (universe_visible || owner || access.grant.is_some());
    let by_role = role == RoleDecision::Allowed;
    if !readable {
        // Governance without content: Operator/Admin stop, Admin deletes,
        // restricted work included. Everything else looks absent.
        let governs = match action {
            StopSession => by_role,
            DeleteSession | DeleteCollection => rights.has_role(Role::Admin),
            _ => false,
        };
        return if governs {
            Decision::Allowed
        } else {
            Decision::Hidden
        };
    }
    if role == RoleDecision::Denied {
        return Decision::Forbidden;
    }
    let writer = owner || access.grant == Some(ResourcePermission::Write);
    // A bot's sessions follow the bot's managers.
    let manages_bot =
        access.anchor.bot.is_some() && rights.universe_action(ManageBot) == RoleDecision::Allowed;
    let allowed = match action {
        Read => true,
        // Control of a collection is creating in it.
        ControlSession => writer || manages_bot,
        StopSession => by_role || writer,
        DeleteSession => owner || manages_bot || rights.has_role(Role::Admin),
        ManageCollection => owner || (by_role && universe_visible),
        DeleteCollection => owner || rights.has_role(Role::Admin),
        InvokeBot => (by_role && universe_visible) || writer,
        ManageBot => owner || (by_role && universe_visible),
        ManageProfile => owner || by_role,
        // Profiles stay on role rules and have no audience to share.
        ShareResource => writer && !matches!(access.anchor.resource, ResourceRef::Profile(_)),
        CreateSession | CreateProfile | CreateBot | CreateCollection | UseResource
        | ConfigureResource | ManageAccess => by_role,
    };
    if allowed {
        Decision::Allowed
    } else {
        Decision::Forbidden
    }
}

fn authorize_controller(
    context: &ControllerContext,
    action: UniverseAction,
    resource: Option<&ResourceAccess>,
) -> Decision {
    use UniverseAction::*;
    let is_bot = matches!(context.actor, ResourceRef::Bot(_));
    let Some(access) = resource else {
        return match action {
            Read | CreateSession => Decision::Allowed,
            UseResource if is_bot => Decision::Allowed,
            _ => Decision::Forbidden,
        };
    };
    let Some(policy) = &access.policy else {
        return Decision::Hidden;
    };
    if policy.visibility != Visibility::Universe && access.anchor.audience_root != context.root {
        return Decision::Hidden;
    }
    let anchor = &access.anchor;
    let controls = anchor.resource == context.actor
        || matches!((&context.actor, &anchor.bot), (ResourceRef::Bot(bot), Some(managed)) if bot == managed)
        || matches!((&context.actor, &anchor.controller), (ResourceRef::Session(actor), ResourceController::Session(parent)) if actor == parent);
    let allowed = match action {
        Read | CreateSession => true,
        UseResource => is_bot,
        ControlSession | StopSession | DeleteSession | ManageBot => controls,
        _ => false,
    };
    if allowed {
        Decision::Allowed
    } else {
        Decision::Forbidden
    }
}

impl EffectiveAccess {
    pub fn active(&self) -> bool {
        self.principal.status == PrincipalStatus::Active
    }

    pub fn has_role(&self, role: Role) -> bool {
        self.active() && role.valid_in(self.scope) && self.roles.contains(&role)
    }

    pub fn has_capability(&self, capability: ServiceCapability) -> bool {
        self.active()
            && self.principal.kind == PrincipalKind::Service
            && capability.valid_in(self.scope)
            && self.capabilities.contains(&capability)
    }

    pub fn universe_action(&self, action: UniverseAction) -> RoleDecision {
        use RoleDecision::*;
        use UniverseAction::*;
        if !self.active() || !matches!(self.scope, AccessScope::Universe { .. }) {
            return Denied;
        }
        let admin = self.has_role(Role::Admin);
        let operator = admin || self.has_role(Role::Operator);
        let contributor = operator || self.has_role(Role::Contributor);
        let viewer = contributor || self.has_role(Role::Viewer);
        match action {
            Read if viewer => Allowed,
            CreateSession | CreateProfile | CreateBot | CreateCollection | InvokeBot
            | UseResource
                if contributor =>
            {
                Allowed
            }
            ControlSession | ShareResource | DeleteCollection if contributor => RequiresOwnership,
            StopSession if operator => Allowed,
            StopSession | DeleteSession if contributor => RequiresOwnership,
            ManageProfile | ManageBot | ManageCollection if operator => Allowed,
            ManageProfile | ManageBot | ManageCollection if contributor => RequiresOwnership,
            ConfigureResource if operator => Allowed,
            ManageAccess if admin => Allowed,
            _ => Denied,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "operation",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum AccessChange {
    CreatePrincipal {
        id: Uuid,
        kind: PrincipalKind,
        display_name: String,
        management_scope: AccessScope,
    },
    SetPrincipalStatus {
        id: Uuid,
        status: PrincipalStatus,
    },
    CreateGroup {
        id: Uuid,
        display_name: String,
    },
    RenameGroup {
        id: Uuid,
        display_name: String,
    },
    PutMembership {
        membership: Membership,
    },
    RemoveMembership {
        membership: Membership,
    },
    AssignRole {
        assignment: RoleAssignment,
    },
    RevokeRole {
        assignment: RoleAssignment,
    },
    /// Atomically replaces one existing grant, preserving other direct/group grants.
    /// A missing source grant is a conflict; last-administrator protection applies.
    ReplaceRole {
        assignment: RoleAssignment,
        role: Role,
    },
    AssignCapability {
        assignment: CapabilityAssignment,
    },
    RevokeCapability {
        assignment: CapabilityAssignment,
    },
    /// Creates the universe and the actor's Admin assignment atomically.
    CreateUniverse {
        universe_id: Uuid,
        slug: Option<String>,
    },
    /// Explicit deployment administration; does not grant content access to
    /// the recovering administrator. Only valid for an orphaned universe.
    RecoverUniverse {
        universe_id: Uuid,
        principal_id: Uuid,
    },
}

impl AccessChange {
    pub fn validate(&self) -> Result<(), AccessError> {
        use AccessChange::*;
        match self {
            CreatePrincipal {
                id,
                kind,
                display_name,
                management_scope,
            } => {
                valid_id(*id)?;
                valid_name(display_name)?;
                if *kind == PrincipalKind::User && *management_scope != AccessScope::Deployment {
                    return Err(AccessError::Invalid("users are deployment-managed".into()));
                }
                valid_scope(*management_scope)
            }
            SetPrincipalStatus { id, .. } => valid_id(*id),
            CreateGroup { id, display_name } | RenameGroup { id, display_name } => {
                valid_id(*id)?;
                valid_name(display_name)
            }
            PutMembership { membership } | RemoveMembership { membership } => {
                valid_id(membership.group_id)?;
                valid_id(membership.principal_id)
            }
            AssignRole { assignment } | RevokeRole { assignment } => {
                valid_scope(assignment.scope)?;
                valid_id(match assignment.subject {
                    Subject::Principal(id) | Subject::Group(id) => id,
                })?;
                if !assignment.role.valid_in(assignment.scope) {
                    return Err(AccessError::Invalid("role does not belong to scope".into()));
                }
                Ok(())
            }
            ReplaceRole { assignment, role } => {
                AssignRole {
                    assignment: *assignment,
                }
                .validate()?;
                AssignRole {
                    assignment: RoleAssignment {
                        role: *role,
                        ..*assignment
                    },
                }
                .validate()
            }
            AssignCapability { assignment } | RevokeCapability { assignment } => {
                valid_id(assignment.principal_id)?;
                valid_scope(assignment.scope)?;
                if !assignment.capability.valid_in(assignment.scope) {
                    return Err(AccessError::Invalid(
                        "capability does not belong to scope".into(),
                    ));
                }
                Ok(())
            }
            CreateUniverse { universe_id, slug } => {
                valid_id(*universe_id)?;
                if let Some(slug) = slug
                    && (slug.is_empty()
                        || slug.len() > 128
                        || !slug.as_bytes()[0].is_ascii_alphanumeric()
                        || !slug
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c)))
                {
                    return Err(AccessError::Invalid("invalid universe slug".into()));
                }
                Ok(())
            }
            RecoverUniverse {
                universe_id,
                principal_id,
            } => {
                valid_id(*universe_id)?;
                valid_id(*principal_id)
            }
        }
    }
}

fn valid_id(id: Uuid) -> Result<(), AccessError> {
    if id.is_nil() {
        Err(AccessError::Invalid("nil identity or universe id".into()))
    } else {
        Ok(())
    }
}
fn valid_scope(scope: AccessScope) -> Result<(), AccessError> {
    match scope {
        AccessScope::Deployment => Ok(()),
        AccessScope::Universe { universe_id } => valid_id(universe_id),
    }
}
fn valid_name(name: &str) -> Result<(), AccessError> {
    if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        Err(AccessError::Invalid(
            "name must contain 1–256 bytes and no control characters".into(),
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessChangeResult {
    pub changed: bool,
    pub policy_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum AccessError {
    #[error("invalid access request: {0}")]
    Invalid(String),
    #[error("identity, group, or universe not found")]
    NotFound,
    #[error("access denied")]
    Denied,
    #[error("identity or assignment conflicts with existing state")]
    Conflict,
    #[error("policy revision {actual} does not match expected {expected}")]
    RevisionMismatch { expected: u64, actual: u64 },
    #[error("cannot remove the last active administrator of {scope:?}")]
    LastAdministrator { scope: AccessScope },
    #[error("identity bootstrap has already completed")]
    AlreadyBootstrapped,
    #[error("access store failed: {0}")]
    Store(String),
}

/// Administrative directory view. Assignments are limited to the requested scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccessDirectory {
    pub principals: Vec<Principal>,
    pub groups: Vec<Group>,
    pub memberships: Vec<Membership>,
    pub roles: Vec<RoleAssignment>,
    pub capabilities: Vec<CapabilityAssignment>,
    pub policy_revision: u64,
}

/// Runtime persistence boundary. `actor` must come from authenticated context
/// or an explicitly trusted host-administration entry point, never RPC params.
/// Mutations authorize against committed facts in the same transaction as the
/// write, last-admin check, policy revision, and durable audit record.
#[async_trait]
pub trait AccessStore: Send + Sync {
    async fn principal(&self, id: Uuid) -> Result<Option<Principal>, AccessError>;
    async fn effective_access(
        &self,
        principal_id: Uuid,
        scope: AccessScope,
    ) -> Result<EffectiveAccess, AccessError>;
    /// Only universes with a role assignment; DeploymentAdmin is not implicit
    /// universe membership. Disabled identities see no universes.
    async fn accessible_universes(&self, principal_id: Uuid) -> Result<Vec<Uuid>, AccessError>;
    async fn apply(
        &self,
        actor: Uuid,
        change: AccessChange,
        now_ms: u64,
    ) -> Result<AccessChangeResult, AccessError>;
    /// Host-only first-admin bootstrap. Same-id retries are idempotent; a
    /// different id, or a disabled former bootstrap identity, never resets it.
    async fn bootstrap(
        &self,
        principal_id: Uuid,
        display_name: String,
        now_ms: u64,
    ) -> Result<AccessChangeResult, AccessError>;
}

/// Whether an actor may mint a credential for a principal. Scope is a ceiling,
/// never a source of authority. Management ownership matters for service keys.
pub fn may_issue_key(
    credential_scope: AccessScope,
    actor: &EffectiveAccess,
    deployment: &EffectiveAccess,
    target: &Principal,
) -> bool {
    if !actor.active()
        || target.status != PrincipalStatus::Active
        || !credential_scope.permits(actor.scope)
    {
        return false;
    }
    let deployment_admin = credential_scope == AccessScope::Deployment
        && deployment.principal.id == actor.principal.id
        && deployment.has_role(Role::DeploymentAdmin);
    match actor.scope {
        AccessScope::Deployment => deployment_admin,
        AccessScope::Universe { .. } => {
            deployment_admin
                || (actor.principal.id == target.id && !actor.roles.is_empty())
                || (actor.has_role(Role::Admin)
                    && target.kind == PrincipalKind::Service
                    && target.management_scope == actor.scope)
        }
    }
}

/// Non-secret reference to the authentication that admitted a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthenticationReference {
    ApiKey { key_prefix: String },
    LocalDevelopment,
}

/// Trusted transport context, resolved once at the authenticated boundary and
/// never deserialized from input. Assertions change the acting principal only;
/// they never add the authenticated service's authority to the user's rights.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestContext {
    /// The acting principal's committed rights in the target scope. Handlers
    /// decide from these; long-lived waits revalidate against `policy_revision`.
    pub rights: EffectiveAccess,
    pub authenticated_principal: Principal,
    pub authentication: AuthenticationReference,
    pub credential_scope: AccessScope,
}

impl RequestContext {
    pub fn acting_principal(&self) -> &Principal {
        &self.rights.principal
    }

    pub fn target_scope(&self) -> AccessScope {
        self.rights.scope
    }

    /// An authenticated service acting for a user it asserted.
    pub fn asserted(&self) -> bool {
        self.rights.principal.id != self.authenticated_principal.id
    }
}

mod audit;
pub use audit::*;

#[cfg(test)]
mod tests;

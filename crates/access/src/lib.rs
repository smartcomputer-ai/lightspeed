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

/// A role in a universe, or `deployment_admin` in the deployment. `executor`
/// is the role of agent identities: it sees universe-visible resources and
/// uses resources, nothing else. The runtime assigns it to a universe's
/// execution principal; identity administration never assigns or revokes
/// it, and its holders never get a key.
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
    Executor,
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
pub enum Capability {
    AssertUser,
    LeaseCredentials,
    AdmitChannelInbound,
    DiscoverChannelAccounts,
    ManageIdentity,
    /// Read restricted content of a universe without a grant. Held by a
    /// person, never implied by a role, and every read that relies on it is
    /// audited. It reads; it never controls, shares or takes ownership.
    ReadPrivateContent,
}

impl Capability {
    pub fn valid_in(self, scope: AccessScope) -> bool {
        match self {
            Self::AssertUser => true,
            Self::LeaseCredentials | Self::AdmitChannelInbound | Self::ReadPrivateContent => {
                matches!(scope, AccessScope::Universe { .. })
            }
            Self::DiscoverChannelAccounts | Self::ManageIdentity => {
                scope == AccessScope::Deployment
            }
        }
    }

    /// The kind of principal that may hold the capability: services act
    /// for systems, privileged reading is a person's accountable act.
    pub fn holder(self) -> PrincipalKind {
        match self {
            Self::ReadPrivateContent => PrincipalKind::User,
            _ => PrincipalKind::Service,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityAssignment {
    pub scope: AccessScope,
    pub principal_id: Uuid,
    pub capability: Capability,
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
    /// Use a workspace, environment or MCP server; without a target, the
    /// eligibility to use resources at all.
    UseResource,
    /// Configure a workspace, environment or MCP server; without a target,
    /// the universe's shared configuration.
    ConfigureResource,
    ManageAccess,
    /// Change who may see, control or use a root: its visibility and grants.
    ShareResource,
    /// Create a workspace, which its creator owns.
    CreateWorkspace,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveAccess {
    pub principal: Principal,
    pub scope: AccessScope,
    /// Only assignments in this exact scope, including group-derived roles.
    pub roles: BTreeSet<Role>,
    /// Explicit capabilities in this exact scope; no role implies one.
    pub capabilities: BTreeSet<Capability>,
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
    Workspace(String),
    Environment(String),
    McpServer(String),
}

impl ResourceRef {
    /// The stored and wire spelling of the kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Session(_) => "session",
            Self::Bot(_) => "bot",
            Self::Profile(_) => "profile",
            Self::Workspace(_) => "workspace",
            Self::Environment(_) => "environment",
            Self::McpServer(_) => "mcp_server",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Session(id)
            | Self::Bot(id)
            | Self::Profile(id)
            | Self::Workspace(id)
            | Self::Environment(id)
            | Self::McpServer(id) => id,
        }
    }

    /// Workspaces, environments and MCP servers: roots that sessions use
    /// and that run nothing of their own.
    pub fn is_operational(&self) -> bool {
        matches!(
            self,
            Self::Workspace(_) | Self::Environment(_) | Self::McpServer(_)
        )
    }

    /// The kind as people read it, in refusals and not-found errors.
    pub fn label(&self) -> &'static str {
        match self {
            Self::McpServer(_) => "MCP server",
            other => other.kind(),
        }
    }

    pub fn from_kind(kind: &str, id: String) -> Option<Self> {
        Some(match kind {
            "session" => Self::Session(id),
            "bot" => Self::Bot(id),
            "profile" => Self::Profile(id),
            "workspace" => Self::Workspace(id),
            "environment" => Self::Environment(id),
            "mcp_server" => Self::McpServer(id),
            _ => return None,
        })
    }
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

/// One permission a grant confers on a root. On sessions and bots `Read`
/// sees the tree and `Write` also controls it; on workspaces, environments
/// and MCP servers `Use` is the only permission.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ResourcePermission {
    Read,
    Write,
    Use,
}

impl ResourcePermission {
    /// The stored and wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Use => "use",
        }
    }

    /// Whether a grant of this permission exists on the kind of `resource`.
    /// Profiles take no grants.
    pub fn valid_for(self, resource: &ResourceRef) -> bool {
        match self {
            Self::Use => resource.is_operational(),
            Self::Read | Self::Write => {
                matches!(resource, ResourceRef::Session(_) | ResourceRef::Bot(_))
            }
        }
    }
}

/// Under whose authority a root's work runs: the universe's execution
/// service, or the person who created it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind {
    Service,
    Personal,
}

/// The execution identity of a root, fixed at creation and copied to every
/// resource below it. Never a login identity of someone else: `personal`
/// means the owner itself, `service` a keyless service principal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Execution {
    pub run_as: Uuid,
    pub kind: ExecutionKind,
}

/// Immutable facts of one governed resource, reserved before it exists. The
/// audience root, managing bot and execution are copied from the admitted
/// controller at reservation, so a decision reads one row instead of walking
/// a lineage.
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
    /// Absent only for kinds that run nothing: profiles, workspaces,
    /// environments and MCP servers.
    pub execution: Option<Execution>,
    pub created_at_ms: u64,
}

/// What a viewer needs to show "shared through X, running as Y" without a
/// second request: the root and its policy, plus the execution identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAccessSummary {
    pub root: ResourceRef,
    pub owner: Uuid,
    pub visibility: Visibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<Execution>,
}

/// A universe's execution policy: the service principal its work runs as by
/// default, and whether people may run work as themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UniverseExecutionPolicy {
    pub execution_principal_id: Uuid,
    pub personal_execution_enabled: bool,
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

impl ResourceAccess {
    /// What a view shows of these facts; `None` without a root policy.
    pub fn summary(&self) -> Option<ResourceAccessSummary> {
        let policy = self.policy.as_ref()?;
        Some(ResourceAccessSummary {
            root: self.anchor.audience_root.clone(),
            owner: policy.owner,
            visibility: policy.visibility,
            execution: self.anchor.execution,
        })
    }
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
    /// The principal the actor's work runs as; resource use is its rights,
    /// checked at admission and at every model call.
    pub execution_principal: Option<Uuid>,
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
    /// Allowed only because the caller holds `read_private_content`: the
    /// read proceeds and the request is audited as privileged.
    Privileged,
    /// The caller may see the resource but not do this.
    Forbidden,
    /// The caller may not see the resource at all; it should look absent.
    Hidden,
}

impl Decision {
    pub fn allows(self) -> bool {
        matches!(self, Self::Allowed | Self::Privileged)
    }
}

/// The first of `resources` the principal holding `rights` may not use,
/// with the decision that refused it; `None` when it may use them all.
/// `accesses` are the loaded facts of those that have an anchor; one
/// without is hidden. Attachment admission, run admission, the per-turn
/// check and bot triggers all ask this of an execution identity.
pub fn first_unusable(
    rights: &EffectiveAccess,
    resources: &[ResourceRef],
    accesses: &[ResourceAccess],
) -> Option<(ResourceRef, Decision)> {
    resources.iter().find_map(|resource| {
        let decision = accesses
            .iter()
            .find(|access| &access.anchor.resource == resource)
            .map_or(Decision::Hidden, |access| {
                authorize(
                    Caller::Request(rights),
                    UniverseAction::UseResource,
                    Some(access),
                )
            });
        (!decision.allows()).then(|| (resource.clone(), decision))
    })
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
    if access.anchor.resource.is_operational() {
        return authorize_operational(rights, action, role, access, policy);
    }
    let owner = policy.owner == rights.principal.id;
    let universe_visible = policy.visibility == Visibility::Universe;
    let readable = rights.universe_action(Read) == RoleDecision::Allowed
        && (universe_visible || owner || access.grant.is_some());
    let by_role = role == RoleDecision::Allowed;
    if !readable {
        // Governance without content: Operator/Admin stop, Admin deletes,
        // restricted work included. Everything else looks absent, except to
        // a holder of the privileged-read capability, who reads and is
        // audited for it, and is refused, not hidden, for anything else.
        let governs = match action {
            StopSession => by_role,
            DeleteSession => rights.has_role(Role::Admin),
            _ => false,
        };
        let privileged = rights.universe_action(Read) == RoleDecision::Allowed
            && rights.has_capability(Capability::ReadPrivateContent);
        return match (governs, privileged, action) {
            (true, _, _) => Decision::Allowed,
            (false, true, Read) => Decision::Privileged,
            (false, true, _) => Decision::Forbidden,
            (false, false, _) => Decision::Hidden,
        };
    }
    if role == RoleDecision::Denied {
        return Decision::Forbidden;
    }
    let writer = owner || access.grant == Some(ResourcePermission::Write);
    // Personal work runs as its owner, so nobody else works in it or
    // configures it, whatever its visibility. Governance still applies:
    // Operators stop it and Admins delete it.
    let personal = access
        .anchor
        .execution
        .is_some_and(|execution| execution.kind == ExecutionKind::Personal);
    // Shared work is universe-visible and runs as the universe's agent
    // identity: every Contributor and above works in it. Viewers were
    // refused above, since their role denies every such action.
    let shared = universe_visible && !personal;
    // A bot's sessions follow the bot's managers.
    let manages_bot = access.anchor.bot.is_some()
        && !personal
        && rights.universe_action(ManageBot) == RoleDecision::Allowed;
    let allowed = match action {
        Read => true,
        ControlSession => writer || shared || manages_bot,
        StopSession => by_role || writer || shared,
        DeleteSession => owner || manages_bot || rights.has_role(Role::Admin),
        InvokeBot => writer || (by_role && shared),
        ManageBot => owner || (by_role && shared),
        ManageProfile => owner || by_role,
        // Profiles stay on role rules and have no audience to share.
        ShareResource => writer && !matches!(access.anchor.resource, ResourceRef::Profile(_)),
        CreateSession | CreateProfile | CreateBot | CreateWorkspace | UseResource
        | ConfigureResource | ManageAccess => by_role,
    };
    if allowed {
        Decision::Allowed
    } else {
        Decision::Forbidden
    }
}

/// Workspaces, environments and MCP servers. Restriction is governance, not
/// privacy: Admin always sees and may use, configure and share them, and no
/// privileged read applies. A restricted resource replaces the role
/// allowance: seeing and using need ownership or a grant, configuring and
/// sharing need ownership. Roles stay ceilings, so a grant or ownership never
/// lets a role do what it may not do anywhere.
fn authorize_operational(
    rights: &EffectiveAccess,
    action: UniverseAction,
    role: RoleDecision,
    access: &ResourceAccess,
    policy: &ResourcePolicy,
) -> Decision {
    use UniverseAction::*;
    let admin = rights.has_role(Role::Admin);
    let owner = policy.owner == rights.principal.id;
    let universe_visible = policy.visibility == Visibility::Universe;
    let visible = universe_visible || owner || access.grant.is_some() || admin;
    if rights.universe_action(Read) != RoleDecision::Allowed || !visible {
        return Decision::Hidden;
    }
    if role == RoleDecision::Denied {
        return Decision::Forbidden;
    }
    let by_role = role == RoleDecision::Allowed;
    let allowed = match action {
        Read => true,
        UseResource => {
            by_role
                && (universe_visible
                    || owner
                    || access.grant == Some(ResourcePermission::Use)
                    || admin)
        }
        ConfigureResource => owner || admin || (by_role && universe_visible),
        ShareResource => owner || admin,
        _ => false,
    };
    if allowed {
        Decision::Allowed
    } else {
        Decision::Forbidden
    }
}

/// What a replacement of a root's policy asks for: the whole visibility and
/// grant set, and a new owner when it hands the root over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyReplacement {
    pub visibility: Visibility,
    pub grants: Vec<(Subject, ResourcePermission)>,
    /// `None`, or the current owner, keeps the owner.
    pub owner: Option<Uuid>,
}

/// Whether `rights` may replace a root's policy, decided from the facts the
/// store loaded under the policy row's lock, so a right revoked after the
/// request was admitted cannot commit. Sharing must be allowed; every grant
/// must be a permission of the root's kind; the owner takes no grant; only
/// the owner hands the root over; and on sessions and bots only the owner
/// adds or removes a `write` grant, so a writer cannot restore or widen its
/// own control.
pub fn authorize_policy_replacement(
    rights: &EffectiveAccess,
    current: &ResourceAccess,
    current_grants: &[ResourceGrant],
    replacement: &PolicyReplacement,
) -> Result<(), AccessError> {
    let Some(policy) = &current.policy else {
        return Err(AccessError::NotFound);
    };
    if authorize(
        Caller::Request(rights),
        UniverseAction::ShareResource,
        Some(current),
    ) != Decision::Allowed
    {
        return Err(AccessError::Denied);
    }
    let root = &current.anchor.audience_root;
    if let Some((_, permission)) = replacement
        .grants
        .iter()
        .find(|(_, permission)| !permission.valid_for(root))
    {
        return Err(AccessError::Invalid(format!(
            "a {} takes no {} grant",
            root.kind(),
            permission.as_str()
        )));
    }
    let owner = replacement.owner.unwrap_or(policy.owner);
    if replacement
        .grants
        .iter()
        .any(|(subject, _)| *subject == Subject::Principal(owner))
    {
        return Err(AccessError::Invalid(
            "the owner holds every permission and takes no grant".into(),
        ));
    }
    let acting_owner = policy.owner == rights.principal.id;
    if owner != policy.owner && !acting_owner {
        return Err(AccessError::Denied);
    }
    if !root.is_operational() && !acting_owner {
        let current_writers: BTreeSet<Subject> = current_grants
            .iter()
            .filter(|grant| grant.permission == ResourcePermission::Write)
            .map(|grant| grant.subject)
            .collect();
        let requested_writers: BTreeSet<Subject> = replacement
            .grants
            .iter()
            .filter(|(_, permission)| *permission == ResourcePermission::Write)
            .map(|(subject, _)| *subject)
            .collect();
        if current_writers != requested_writers {
            return Err(AccessError::Denied);
        }
    }
    Ok(())
}

fn authorize_controller(
    context: &ControllerContext,
    action: UniverseAction,
    resource: Option<&ResourceAccess>,
) -> Decision {
    use UniverseAction::*;
    let executes = context.execution_principal.is_some();
    let Some(access) = resource else {
        return match action {
            Read | CreateSession => Decision::Allowed,
            UseResource if executes => Decision::Allowed,
            _ => Decision::Forbidden,
        };
    };
    // The store decides a controller's use of a workspace, environment or
    // MCP server as its execution principal before reaching the evaluator,
    // so this refusal is never the real decision; it only keeps internal
    // work from inheriting rights of its own here.
    if access.anchor.resource.is_operational() {
        return Decision::Forbidden;
    }
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
        UseResource => executes,
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

    /// A principal holding Executor is an agent identity, and Executor alone
    /// decides for it: any other role it was given, directly or through a
    /// group, confers nothing.
    pub fn has_role(&self, role: Role) -> bool {
        self.active()
            && role.valid_in(self.scope)
            && self.roles.contains(&role)
            && (role == Role::Executor || !self.roles.contains(&Role::Executor))
    }

    pub fn has_capability(&self, capability: Capability) -> bool {
        self.active()
            && self.principal.kind == capability.holder()
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
        // Outside the chain: an agent identity reads and uses, nothing more,
        // and `has_role` gives it no other role.
        let executor = self.has_role(Role::Executor);
        match action {
            Read if viewer || executor => Allowed,
            UseResource if contributor || executor => Allowed,
            CreateSession | CreateProfile | CreateBot | CreateWorkspace | InvokeBot
                if contributor =>
            {
                Allowed
            }
            ControlSession | ShareResource if contributor => RequiresOwnership,
            StopSession if operator => Allowed,
            StopSession | DeleteSession if contributor => RequiresOwnership,
            ManageProfile | ManageBot if operator => Allowed,
            ManageProfile | ManageBot if contributor => RequiresOwnership,
            ConfigureResource if operator => Allowed,
            // A Contributor configures the workspaces, environments and MCP
            // servers it owns, never anything by role.
            ConfigureResource if contributor => RequiresOwnership,
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
                if assignment.role == Role::Executor {
                    return Err(AccessError::Invalid("executor is system-assigned".into()));
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

//! Access classification of every public method, and the few access inputs
//! and views sessions and bots carry.
//!
//! Core gates a request by its key: the key's scope and the method's group
//! (derived from the method name). Everything about people is decided by
//! whoever asserts actors, such as the Platform; the recommended role and the
//! target of each method are contract metadata for such gates, and core never
//! evaluates them.

use super::*;
use std::collections::BTreeSet;
use uuid::Uuid;

/// What a key reaches or a request addresses: one universe, or the
/// deployment.
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
    /// The universe addressed, if any.
    pub fn universe_id(self) -> Option<Uuid> {
        match self {
            Self::Deployment => None,
            Self::Universe { universe_id } => Some(universe_id),
        }
    }
}

/// The methods a key may call, by group. Every public method but
/// `initialize` belongs to exactly one group, derived from its name, so a
/// new method never changes what an existing key reaches beyond its group.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub enum MethodGroup {
    /// `session/*`, `blobs/read` and `blobs/has`.
    #[serde(rename = "session")]
    Session,
    /// `blobs/put` alone, so connectors can upload attachments without
    /// reading sessions.
    #[serde(rename = "blobs/put")]
    BlobsPut,
    #[serde(rename = "vfs")]
    Vfs,
    #[serde(rename = "profiles")]
    Profiles,
    #[serde(rename = "models")]
    Models,
    #[serde(rename = "mcp")]
    Mcp,
    /// `environments/*`, including jobs and registration keys.
    #[serde(rename = "environments")]
    Environments,
    #[serde(rename = "bots")]
    Bots,
    /// Channel accounts, pairings and conversations.
    #[serde(rename = "channels")]
    Channels,
    /// `channels/inbound/admit`: what a connector delivers.
    #[serde(rename = "channels/inbound")]
    ChannelsInbound,
    /// `auth/*` except leasing.
    #[serde(rename = "auth")]
    Auth,
    /// `auth/grants/lease`: resolving a brokered credential for delivery.
    #[serde(rename = "auth/lease")]
    AuthLease,
    #[serde(rename = "deployment/universes")]
    DeploymentUniverses,
    #[serde(rename = "deployment/api-keys")]
    DeploymentApiKeys,
    /// Environment providers, their bindings and adoption.
    #[serde(rename = "deployment/environment-providers")]
    DeploymentEnvironmentProviders,
    /// `deployment/channels/accounts/list`: a connector host's discovery.
    #[serde(rename = "deployment/channels")]
    DeploymentChannels,
}

impl MethodGroup {
    pub const ALL: [MethodGroup; 16] = [
        Self::Session,
        Self::BlobsPut,
        Self::Vfs,
        Self::Profiles,
        Self::Models,
        Self::Mcp,
        Self::Environments,
        Self::Bots,
        Self::Channels,
        Self::ChannelsInbound,
        Self::Auth,
        Self::AuthLease,
        Self::DeploymentUniverses,
        Self::DeploymentApiKeys,
        Self::DeploymentEnvironmentProviders,
        Self::DeploymentChannels,
    ];

    /// The stored and wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::BlobsPut => "blobs/put",
            Self::Vfs => "vfs",
            Self::Profiles => "profiles",
            Self::Models => "models",
            Self::Mcp => "mcp",
            Self::Environments => "environments",
            Self::Bots => "bots",
            Self::Channels => "channels",
            Self::ChannelsInbound => "channels/inbound",
            Self::Auth => "auth",
            Self::AuthLease => "auth/lease",
            Self::DeploymentUniverses => "deployment/universes",
            Self::DeploymentApiKeys => "deployment/api-keys",
            Self::DeploymentEnvironmentProviders => "deployment/environment-providers",
            Self::DeploymentChannels => "deployment/channels",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|group| group.as_str() == value)
    }

    /// Deployment groups address the deployment, never one universe; only a
    /// deployment key holds them.
    pub fn is_deployment(self) -> bool {
        matches!(
            self,
            Self::DeploymentUniverses
                | Self::DeploymentApiKeys
                | Self::DeploymentEnvironmentProviders
                | Self::DeploymentChannels
        )
    }

    /// Every group a key of `scope` may hold, which is also what a key
    /// minted without explicit groups receives.
    pub fn allowed_in(scope: AccessScope) -> BTreeSet<MethodGroup> {
        Self::ALL
            .into_iter()
            .filter(|group| scope == AccessScope::Deployment || !group.is_deployment())
            .collect()
    }

    /// The group a method belongs to, from its name. `None` for
    /// `initialize`, which every key may call, and for names outside the
    /// contract.
    pub fn of(method: &str) -> Option<MethodGroup> {
        let group = |prefix: &str| method.starts_with(prefix);
        Some(if method == "blobs/put" {
            Self::BlobsPut
        } else if group("session/") || group("blobs/") {
            Self::Session
        } else if group("vfs/") {
            Self::Vfs
        } else if group("profiles/") {
            Self::Profiles
        } else if group("models/") {
            Self::Models
        } else if group("mcp/") {
            Self::Mcp
        } else if group("environments/") {
            Self::Environments
        } else if group("bots/") {
            Self::Bots
        } else if group("channels/inbound/") {
            Self::ChannelsInbound
        } else if group("channels/") {
            Self::Channels
        } else if method == "auth/grants/lease" {
            Self::AuthLease
        } else if group("auth/") {
            Self::Auth
        } else if group("deployment/universes/") {
            Self::DeploymentUniverses
        } else if group("deployment/api-keys/") {
            Self::DeploymentApiKeys
        } else if group("deployment/environment-provider") || group("deployment/environments/") {
            Self::DeploymentEnvironmentProviders
        } else if group("deployment/channels/") {
            Self::DeploymentChannels
        } else {
            return None;
        })
    }
}

/// Who created a resource or authored bytes. An actor is whatever a key
/// allowed to assert one said; core compares it and never resolves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Attribution {
    /// The actor a key asserted for its request.
    Actor { id: String },
    /// A key acting for itself, named by its display prefix.
    Key { prefix: String },
    /// An unauthenticated local development request, or an in-process call.
    Local,
    /// The runtime's own work: a bot, a delegated session, a registration,
    /// or host administration through the server CLI.
    Internal { component: String, cause: String },
}

impl Attribution {
    /// The asserted actor, if this is one.
    pub fn actor(&self) -> Option<&str> {
        match self {
            Self::Actor { id } => Some(id),
            _ => None,
        }
    }
}

/// A resource a method names.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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
    /// The wire spelling of the kind.
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

    /// The kind as people read it, in not-found errors.
    pub fn label(&self) -> &'static str {
        match self {
            Self::McpServer(_) => "MCP server",
            other => other.kind(),
        }
    }
}

/// Who sees a root's tree. Sessions start unshared (`restricted`) and are
/// shared with the universe once, one way; everything else is shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Universe,
    Restricted,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Universe => "universe",
            Self::Restricted => "restricted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "universe" => Some(Self::Universe),
            "restricted" => Some(Self::Restricted),
            _ => None,
        }
    }
}

/// The audience of a session's or bot's root as views show it: whether it is
/// shared with the universe, and who created it. A bot's session and a
/// delegated child show their root's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAccessSummary {
    pub visibility: Visibility,
    /// Absent for work whose creator was not recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Attribution>,
}

/// What a public method does, independent of its RPC spelling. Requests are
/// gated by their key's groups; this classifies what the runtime's own work
/// may do and which role a gate built on the contract should require.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UniverseAction {
    Read,
    CreateSession,
    ControlSession,
    StopSession,
    DeleteSession,
    /// Share an unshared session with the universe.
    ShareSession,
    CreateProfile,
    ManageProfile,
    CreateBot,
    ManageBot,
    InvokeBot,
    /// Use a workspace, environment or MCP server, or store bytes.
    UseResource,
    /// Configure a workspace, environment, MCP server or shared
    /// configuration such as credentials and channels.
    ConfigureResource,
    CreateWorkspace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "action", rename_all = "snake_case")]
pub enum MethodAccess {
    /// A universe method, and the action internal work is held to when it
    /// calls it.
    Universe(UniverseAction),
    /// A universe method for machines only, such as connectors delivering
    /// messages or triggers leasing a credential. No person calls it.
    Service,
    /// A deployment method: it addresses the deployment and needs a
    /// deployment key.
    Deployment,
}

impl MethodAccess {
    pub const fn scope(self) -> MethodScope {
        match self {
            Self::Universe(_) => MethodScope::Universe,
            Self::Service => MethodScope::Service,
            Self::Deployment => MethodScope::Deployment,
        }
    }

    /// The least universe role a person should hold to call the method, for
    /// gates built on this contract. `None` for machine and deployment
    /// methods. Ownership rules (a Contributor deleting only their own work)
    /// are the gate's to add.
    pub const fn recommended_role(self) -> Option<RecommendedRole> {
        use UniverseAction::*;
        let Self::Universe(action) = self else {
            return None;
        };
        Some(match action {
            Read => RecommendedRole::Viewer,
            CreateSession | ControlSession | StopSession | DeleteSession | ShareSession
            | InvokeBot | UseResource | CreateWorkspace => RecommendedRole::Contributor,
            CreateProfile | ManageProfile | CreateBot | ManageBot | ConfigureResource => {
                RecommendedRole::Operator
            }
        })
    }
}

/// Universe roles as gates built on this contract name them. Core holds no
/// roles; this is metadata only.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedRole {
    Viewer,
    Contributor,
    Operator,
    Admin,
}

/// Where a method names the session it acts on, so a gate can decide per
/// session without a hand-written table. A creation method may name one that
/// does not exist yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum MethodTarget {
    /// `params.sessionId`.
    #[serde(rename = "sessionId")]
    SessionId,
}

/// Find the declared requirement. Unknown methods never inherit a default.
pub fn method_access(method: &str) -> Option<MethodAccess> {
    crate::rpc::universe_method_access(method)
        .or_else(|| crate::deployment::deployment_method_access(method))
}

/// The session a method acts on: every `session/` method but the list.
pub fn method_target(method: &str) -> Option<MethodTarget> {
    (method.starts_with("session/") && method != METHOD_SESSION_LIST)
        .then_some(MethodTarget::SessionId)
}

/// The audience of a new session, requested at creation. Absent means
/// unshared: visible to its creator until it is shared with the universe. A
/// session created under another root (a bot's session, a delegated child)
/// follows that root and refuses this.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionShareParams {
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionShareResponse {
    pub access: ResourceAccessSummary,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_group_round_trips_through_its_wire_spelling() {
        for group in MethodGroup::ALL {
            assert_eq!(MethodGroup::parse(group.as_str()), Some(group));
            assert_eq!(
                serde_json::to_value(group).unwrap(),
                serde_json::Value::String(group.as_str().to_owned())
            );
        }
        assert_eq!(MethodGroup::parse("identity"), None);
    }

    #[test]
    fn methods_belong_to_groups_by_name() {
        for (method, group) in [
            ("session/runs/start", Some(MethodGroup::Session)),
            ("session/share", Some(MethodGroup::Session)),
            ("blobs/read", Some(MethodGroup::Session)),
            ("blobs/has", Some(MethodGroup::Session)),
            ("blobs/put", Some(MethodGroup::BlobsPut)),
            ("vfs/workspaces/create", Some(MethodGroup::Vfs)),
            ("profiles/put", Some(MethodGroup::Profiles)),
            ("models/list", Some(MethodGroup::Models)),
            ("mcp/servers/put", Some(MethodGroup::Mcp)),
            ("environments/jobs/create", Some(MethodGroup::Environments)),
            ("bots/create", Some(MethodGroup::Bots)),
            ("channels/accounts/create", Some(MethodGroup::Channels)),
            ("channels/inbound/admit", Some(MethodGroup::ChannelsInbound)),
            ("auth/grants/import", Some(MethodGroup::Auth)),
            ("auth/grants/lease", Some(MethodGroup::AuthLease)),
            (
                "deployment/universes/create",
                Some(MethodGroup::DeploymentUniverses),
            ),
            (
                "deployment/api-keys/create",
                Some(MethodGroup::DeploymentApiKeys),
            ),
            (
                "deployment/environment-providers/bindings/put",
                Some(MethodGroup::DeploymentEnvironmentProviders),
            ),
            (
                "deployment/environment-provider-bindings/list",
                Some(MethodGroup::DeploymentEnvironmentProviders),
            ),
            (
                "deployment/environments/adopt",
                Some(MethodGroup::DeploymentEnvironmentProviders),
            ),
            (
                "deployment/channels/accounts/list",
                Some(MethodGroup::DeploymentChannels),
            ),
            ("initialize", None),
            ("deployment/identity/apply", None),
            ("access/policy/put", None),
        ] {
            assert_eq!(MethodGroup::of(method), group, "{method}");
        }
    }

    #[test]
    fn universe_keys_never_hold_deployment_groups() {
        let scope = AccessScope::Universe {
            universe_id: Uuid::from_u128(1),
        };
        let universe_groups = MethodGroup::allowed_in(scope);
        assert!(universe_groups.iter().all(|group| !group.is_deployment()));
        assert!(universe_groups.contains(&MethodGroup::Session));
        assert_eq!(
            MethodGroup::allowed_in(AccessScope::Deployment),
            MethodGroup::ALL.into_iter().collect()
        );
    }

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
            "deployment/identity/apply",
            "access/policy/put",
            "session/missing",
            "",
        ] {
            assert_eq!(method_access(method), None);
            assert!(!is_service_method(method));
        }
    }

    /// Every method but `initialize` belongs to a group, and deployment
    /// methods exactly to deployment groups.
    #[test]
    fn every_method_belongs_to_a_group_of_its_scope() {
        for spec in crate::schema_export::full_method_manifest() {
            let group = MethodGroup::of(spec.method);
            if spec.method == METHOD_INITIALIZE {
                assert_eq!(group, None);
                continue;
            }
            let group = group.unwrap_or_else(|| panic!("{} has no group", spec.method));
            assert_eq!(
                group.is_deployment(),
                spec.scope == MethodScope::Deployment,
                "{}",
                spec.method
            );
        }
        // Machine methods sit in groups of their own, so a connector's key
        // reaches nothing else.
        assert_eq!(
            MethodGroup::of(METHOD_CHANNELS_INBOUND_ADMIT),
            Some(MethodGroup::ChannelsInbound)
        );
        assert_eq!(
            MethodGroup::of(METHOD_AUTH_GRANTS_LEASE),
            Some(MethodGroup::AuthLease)
        );
    }

    #[test]
    fn people_get_roles_and_machines_and_deployments_none() {
        assert_eq!(
            method_access(METHOD_SESSION_READ).and_then(MethodAccess::recommended_role),
            Some(RecommendedRole::Viewer)
        );
        assert_eq!(
            method_access(METHOD_SESSION_RUNS_START).and_then(MethodAccess::recommended_role),
            Some(RecommendedRole::Contributor)
        );
        assert_eq!(
            method_access(METHOD_MCP_SERVERS_PUT).and_then(MethodAccess::recommended_role),
            Some(RecommendedRole::Operator)
        );
        for method in [
            METHOD_AUTH_GRANTS_LEASE,
            METHOD_CHANNELS_INBOUND_ADMIT,
            METHOD_DEPLOYMENT_UNIVERSES_CREATE,
        ] {
            assert_eq!(
                method_access(method).and_then(MethodAccess::recommended_role),
                None
            );
        }
    }

    #[test]
    fn session_methods_target_their_session() {
        assert_eq!(
            method_target(METHOD_SESSION_RUNS_START),
            Some(MethodTarget::SessionId)
        );
        assert_eq!(
            method_target(METHOD_SESSION_START),
            Some(MethodTarget::SessionId)
        );
        assert_eq!(method_target(METHOD_SESSION_LIST), None);
        assert_eq!(method_target(METHOD_BLOBS_READ), None);
        assert_eq!(method_target(METHOD_BOTS_LIST), None);
    }
}

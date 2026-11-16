//! Access classification of every public method, and the few access inputs
//! and views sessions and bots carry.
//!
//! Caller permissions belong to an external gate such as the Platform.
//! Scope, API family, recommended role and target are published metadata;
//! the runtime does not apply them as caller permissions.

use super::*;
use uuid::Uuid;

/// What a request addresses: one universe, or the deployment.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
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

    /// Whether this is the deployment or a universe, without naming which
    /// universe.
    pub fn kind(self) -> AccessScopeKind {
        match self {
            Self::Deployment => AccessScopeKind::Deployment,
            Self::Universe { .. } => AccessScopeKind::Universe,
        }
    }
}

/// The kind of request target: the deployment or a universe.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum AccessScopeKind {
    Deployment,
    Universe,
}

impl AccessScopeKind {
    /// The stored and wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deployment => "deployment",
            Self::Universe => "universe",
        }
    }
}

/// The API family of a method, published for external authorization gates.
/// `initialize` and `universe/watch` have no single family.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub enum MethodGroup {
    /// `session/*`, `blobs/read` and `blobs/has`.
    #[serde(rename = "session")]
    Session,
    /// Destructive session operations and deletion retention.
    #[serde(rename = "session/delete")]
    SessionDelete,
    /// `blobs/put` alone, so connectors can upload attachments without
    /// reading sessions.
    #[serde(rename = "blobs/put")]
    BlobsPut,
    #[serde(rename = "vfs")]
    Vfs,
    #[serde(rename = "profiles")]
    Profiles,
    #[serde(rename = "transcriptions")]
    Transcriptions,
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
    /// `credentials/*` except leasing.
    #[serde(rename = "credentials")]
    Credentials,
    /// `credentials/grants/lease`: resolving a brokered credential for delivery.
    #[serde(rename = "credentials/lease")]
    CredentialsLease,
    #[serde(rename = "deployment/universes")]
    DeploymentUniverses,
    /// Environment providers and their universe bindings.
    #[serde(rename = "deployment/environment-providers")]
    DeploymentEnvironmentProviders,
    /// `deployment/channels/accounts/list`: a connector host's discovery.
    #[serde(rename = "deployment/channels")]
    DeploymentChannels,
    #[serde(rename = "deployment/sessions")]
    DeploymentSessions,
}

impl MethodGroup {
    pub const ALL: [MethodGroup; 18] = [
        Self::Session,
        Self::SessionDelete,
        Self::BlobsPut,
        Self::Vfs,
        Self::Profiles,
        Self::Models,
        Self::Transcriptions,
        Self::Mcp,
        Self::Environments,
        Self::Bots,
        Self::Channels,
        Self::ChannelsInbound,
        Self::Credentials,
        Self::CredentialsLease,
        Self::DeploymentUniverses,
        Self::DeploymentEnvironmentProviders,
        Self::DeploymentChannels,
        Self::DeploymentSessions,
    ];

    /// The stored and wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::SessionDelete => "session/delete",
            Self::BlobsPut => "blobs/put",
            Self::Vfs => "vfs",
            Self::Profiles => "profiles",
            Self::Models => "models",
            Self::Transcriptions => "transcriptions",
            Self::Mcp => "mcp",
            Self::Environments => "environments",
            Self::Bots => "bots",
            Self::Channels => "channels",
            Self::ChannelsInbound => "channels/inbound",
            Self::Credentials => "credentials",
            Self::CredentialsLease => "credentials/lease",
            Self::DeploymentUniverses => "deployment/universes",
            Self::DeploymentEnvironmentProviders => "deployment/environment-providers",
            Self::DeploymentChannels => "deployment/channels",
            Self::DeploymentSessions => "deployment/sessions",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|group| group.as_str() == value)
    }

    /// Deployment families address the deployment, never one universe.
    pub fn is_deployment(self) -> bool {
        matches!(
            self,
            Self::DeploymentUniverses
                | Self::DeploymentEnvironmentProviders
                | Self::DeploymentChannels
                | Self::DeploymentSessions
        )
    }

    /// The group a method belongs to, from its name. `None` for
    /// `initialize`, `universe/watch`, and names outside the contract.
    pub fn of(method: &str) -> Option<MethodGroup> {
        let group = |prefix: &str| method.starts_with(prefix);
        Some(if method == "blobs/put" {
            Self::BlobsPut
        } else if matches!(method, "session/delete" | "session/retention/put") {
            Self::SessionDelete
        } else if group("deployment/sessions/") {
            Self::DeploymentSessions
        } else if group("session/") || group("blobs/") {
            Self::Session
        } else if group("transcriptions/") {
            Self::Transcriptions
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
        } else if method == "credentials/grants/lease" {
            Self::CredentialsLease
        } else if group("credentials/") {
            Self::Credentials
        } else if group("deployment/universes/") {
            Self::DeploymentUniverses
        } else if group("deployment/environment-provider") {
            Self::DeploymentEnvironmentProviders
        } else if group("deployment/channels/") {
            Self::DeploymentChannels
        } else {
            return None;
        })
    }
}

/// Who created a resource or authored bytes. Actors are opaque caller claims;
/// the runtime records them without resolving caller permissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Attribution {
    /// The actor the caller named for its request.
    Actor { id: String },
    /// The configured runtime token's name, never its secret.
    Token { name: String },
    /// An unauthenticated local development request, or an in-process call.
    Local,
    /// The runtime's own work: a bot, a delegated session, a registration,
    /// or host administration through the server CLI.
    Internal { component: String, cause: String },
}

impl Attribution {
    pub fn token(name: impl Into<String>) -> Self {
        Self::Token { name: name.into() }
    }

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
#[serde(tag = "kind", content = "id", rename_all = "camelCase")]
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
            Self::McpServer(_) => "mcpServer",
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
#[serde(rename_all = "camelCase")]
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

/// What a public method does, independent of its operation ID. This classifies
/// the runtime's own work and the role an external gate should require.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum UniverseAction {
    Read,
    CreateSession,
    ControlSession,
    ConfigureSession,
    StopSession,
    CloseSession,
    DeleteSession,
    SetSessionRetention,
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
#[serde(tag = "kind", content = "action", rename_all = "camelCase")]
pub enum MethodAccess {
    /// Connection discovery, authenticated independently of universe selection.
    Connection,
    /// A universe method, and the action internal work is held to when it
    /// calls it.
    Universe(UniverseAction),
    /// A universe method for machines only, such as connectors delivering
    /// messages or triggers leasing a credential. No person calls it.
    Service,
    /// A deployment method: it addresses the deployment.
    Deployment,
}

impl MethodAccess {
    pub const fn scope(self) -> MethodScope {
        match self {
            Self::Connection => MethodScope::Connection,
            Self::Universe(_) => MethodScope::Universe,
            Self::Service => MethodScope::Service,
            Self::Deployment => MethodScope::Deployment,
        }
    }

    /// The least universe role a person should hold to call the method, for
    /// gates built on this contract. `None` for machine and deployment
    /// methods. Ownership rules (a Contributor closing only their own unshared work)
    /// are the gate's to add.
    pub const fn recommended_role(self) -> Option<RecommendedRole> {
        use UniverseAction::*;
        let Self::Universe(action) = self else {
            return None;
        };
        Some(match action {
            Read => RecommendedRole::Viewer,
            SetSessionRetention => RecommendedRole::Admin,
            CreateSession | ControlSession | StopSession | CloseSession | ShareSession
            | InvokeBot | UseResource => RecommendedRole::Contributor,
            DeleteSession | ConfigureSession | CreateProfile | ManageProfile | CreateBot
            | ManageBot | ConfigureResource | CreateWorkspace => RecommendedRole::Operator,
        })
    }
}

/// Universe roles as gates built on this contract name them. Core holds no
/// roles; this is metadata only.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
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
    if method == crate::METHOD_BLOBS_UPLOAD {
        return Some(MethodAccess::Universe(UniverseAction::UseResource));
    }
    crate::operations::universe_method_access(method)
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

    /// Legacy key attribution must be migrated before it can be read.
    #[test]
    fn an_attribution_naming_a_key_by_its_secret_prefix_does_not_decode() {
        for stale in [
            serde_json::json!({"kind": "key", "prefix": "lsk_abcdefgh"}),
            serde_json::json!({"kind": "key", "keyId": "ci-bot"}),
            serde_json::json!({"kind": "key", "scopeKind": "deployment"}),
            serde_json::json!({"kind": "key", "keyId": "ci-bot", "scopeKind": "tenant"}),
        ] {
            assert!(
                serde_json::from_value::<Attribution>(stale.clone()).is_err(),
                "{stale}"
            );
        }
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
            ("credentials/grants/import", Some(MethodGroup::Credentials)),
            (
                "credentials/grants/lease",
                Some(MethodGroup::CredentialsLease),
            ),
            (
                "deployment/universes/create",
                Some(MethodGroup::DeploymentUniverses),
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
    fn session_setup_requires_an_operator_but_ordinary_creation_and_runs_do_not() {
        for method in [
            "session/config/put",
            "session/profiles/apply",
            "session/metadata/put",
            "session/environments/activate",
            "session/environments/deactivate",
        ] {
            assert_eq!(
                method_access(method).unwrap().recommended_role(),
                Some(RecommendedRole::Operator),
                "{method}",
            );
        }
        for method in ["session/start", "session/runs/start", "session/rename"] {
            assert_eq!(
                method_access(method).unwrap().recommended_role(),
                Some(RecommendedRole::Contributor),
                "{method}",
            );
        }
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
            if matches!(spec.method, METHOD_INITIALIZE | METHOD_UNIVERSE_WATCH) {
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
        // Machine methods have distinct families for external gates.
        assert_eq!(
            MethodGroup::of(METHOD_CHANNELS_INBOUND_ADMIT),
            Some(MethodGroup::ChannelsInbound)
        );
        assert_eq!(
            MethodGroup::of(METHOD_CREDENTIALS_GRANTS_LEASE),
            Some(MethodGroup::CredentialsLease)
        );
    }

    #[test]
    fn people_get_roles_and_machines_and_deployments_none() {
        assert_eq!(
            method_access(METHOD_VFS_WORKSPACES_CREATE).and_then(MethodAccess::recommended_role),
            Some(RecommendedRole::Operator)
        );
        assert_eq!(
            method_access(METHOD_VFS_WORKSPACES_UPDATE).and_then(MethodAccess::recommended_role),
            Some(RecommendedRole::Contributor)
        );
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
            METHOD_CREDENTIALS_GRANTS_LEASE,
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

//! JSON-RPC method and notification names.
//!
//! The first segment of a method name is its addressing scope: `session/`
//! methods act inside one session (params carry `sessionId`), `operator/`
//! methods (see [`crate::operator`]) address the deployment, and every other
//! prefix is a universe-scoped catalog or stream. Collection segments are
//! plural; uncountable facets (`context`, `mcp`) are singular; the
//! verb is always the last segment.

pub const PROTOCOL_VERSION: &str = "lightspeed.agent.api.v1";

// ── Protocol handshake ──────────────────────────────────────────────────────

pub const METHOD_INITIALIZE: &str = "initialize";

// ── Sessions: lifecycle ─────────────────────────────────────────────────────

pub const METHOD_SESSION_START: &str = "session/start";
pub const METHOD_SESSION_MANAGED_START: &str = "session/managed/start";
pub const METHOD_SESSION_READ: &str = "session/read";
pub const METHOD_SESSION_LIST: &str = "session/list";
pub const METHOD_SESSION_CONFIG_PUT: &str = "session/config/put";
pub const METHOD_SESSION_RENAME: &str = "session/rename";
pub const METHOD_SESSION_CLOSE: &str = "session/close";
pub const METHOD_SESSION_DELETE: &str = "session/delete";

// ── Sessions: facets (event log, tools, context, runs) ─────────────────────

pub const METHOD_SESSION_EVENTS_READ: &str = "session/events/read";
pub const METHOD_SESSION_CONTEXT_APPEND: &str = "session/context/append";
pub const METHOD_SESSION_CONTEXT_REMOVE: &str = "session/context/remove";
pub const METHOD_SESSION_CONTEXT_COMPACT: &str = "session/context/compact";
pub const METHOD_SESSION_RUNS_START: &str = "session/runs/start";
pub const METHOD_SESSION_RUNS_CANCEL: &str = "session/runs/cancel";

// ── Sessions: prompt and skill state ────────────────────────────────────────

pub const METHOD_SESSION_SKILLS_LIST: &str = "session/skills/list";
pub const METHOD_SESSION_SKILLS_ACTIVE: &str = "session/skills/active";
pub const METHOD_SESSION_SKILLS_ACTIVATE: &str = "session/skills/activate";
pub const METHOD_SESSION_SKILLS_DEACTIVATE: &str = "session/skills/deactivate";

// ── Sessions: bindings to universe resources ───────────────────────────────

pub const METHOD_SESSION_PROFILES_APPLY: &str = "session/profiles/apply";

// ── Sessions: environments ──────────────────────────────────────────────────

pub const METHOD_SESSION_ENVIRONMENTS_ACTIVATE: &str = "session/environments/activate";
pub const METHOD_SESSION_ENVIRONMENTS_DEACTIVATE: &str = "session/environments/deactivate";
pub const METHOD_ENVIRONMENTS_CREDENTIALS_BIND: &str = "environments/credentials/bind";
pub const METHOD_ENVIRONMENTS_CREDENTIALS_LIST: &str = "environments/credentials/list";
pub const METHOD_ENVIRONMENTS_CREDENTIALS_UNBIND: &str = "environments/credentials/unbind";

// ── Universe: agent profile catalog ─────────────────────────────────────────

// ── Universe: direct provider model discovery ───────────────────────────────

pub const METHOD_MODELS_LIST: &str = "models/list";

pub const METHOD_PROFILES_CREATE: &str = "profiles/create";
pub const METHOD_PROFILES_READ: &str = "profiles/read";
pub const METHOD_PROFILES_LIST: &str = "profiles/list";
pub const METHOD_PROFILES_PUT: &str = "profiles/put";
pub const METHOD_PROFILES_DELETE: &str = "profiles/delete";

// ── Universe: content-addressed storage and VFS ─────────────────────────────

pub const METHOD_BLOBS_PUT: &str = "blobs/put";
pub const METHOD_BLOBS_READ: &str = "blobs/read";
pub const METHOD_BLOBS_HAS: &str = "blobs/has";
pub const METHOD_VFS_SNAPSHOTS_COMMIT: &str = "vfs/snapshots/commit";
pub const METHOD_VFS_SNAPSHOTS_READ: &str = "vfs/snapshots/read";
pub const METHOD_VFS_WORKSPACES_CREATE: &str = "vfs/workspaces/create";
pub const METHOD_VFS_WORKSPACES_READ: &str = "vfs/workspaces/read";
pub const METHOD_VFS_WORKSPACES_LIST: &str = "vfs/workspaces/list";
pub const METHOD_VFS_WORKSPACES_UPDATE: &str = "vfs/workspaces/update";
pub const METHOD_VFS_WORKSPACES_DELETE: &str = "vfs/workspaces/delete";

// ── Universe: MCP server catalog ────────────────────────────────────────────

pub const METHOD_MCP_SERVERS_PUT: &str = "mcp/servers/put";
pub const METHOD_MCP_SERVERS_READ: &str = "mcp/servers/read";
pub const METHOD_MCP_SERVERS_LIST: &str = "mcp/servers/list";
pub const METHOD_MCP_SERVERS_DELETE: &str = "mcp/servers/delete";

// ── Universe: environments and provider presence ────────────────────────────

pub const METHOD_ENVIRONMENTS_CREATE: &str = "environments/create";
pub const METHOD_ENVIRONMENTS_READ: &str = "environments/read";
pub const METHOD_ENVIRONMENTS_LIST: &str = "environments/list";
pub const METHOD_ENVIRONMENTS_CLOSE: &str = "environments/close";
pub const METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_LIST: &str = "environments/provider-bindings/list";
pub const METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_READ: &str = "environments/provider-bindings/read";
pub const METHOD_ENVIRONMENTS_TEMPLATES_LIST: &str = "environments/templates/list";
pub const METHOD_ENVIRONMENTS_TEMPLATES_READ: &str = "environments/templates/read";
pub const METHOD_ENVIRONMENTS_JOBS_CREATE: &str = "environments/jobs/create";
pub const METHOD_ENVIRONMENTS_JOBS_READ: &str = "environments/jobs/read";
pub const METHOD_ENVIRONMENTS_JOBS_CANCEL: &str = "environments/jobs/cancel";

// ── Universe: auth ──────────────────────────────────────────────────────────

pub const METHOD_AUTH_GRANTS_IMPORT: &str = "auth/grants/import";
pub const METHOD_AUTH_GRANTS_READ: &str = "auth/grants/read";
pub const METHOD_AUTH_GRANTS_LIST: &str = "auth/grants/list";
pub const METHOD_AUTH_GRANTS_REVOKE: &str = "auth/grants/revoke";
pub const METHOD_AUTH_CLIENTS_CREATE: &str = "auth/clients/create";
pub const METHOD_AUTH_CLIENTS_READ: &str = "auth/clients/read";
pub const METHOD_AUTH_CLIENTS_LIST: &str = "auth/clients/list";
pub const METHOD_AUTH_CLIENTS_DELETE: &str = "auth/clients/delete";
pub const METHOD_AUTH_FLOWS_START: &str = "auth/flows/start";
pub const METHOD_AUTH_FLOWS_READ: &str = "auth/flows/read";
pub const METHOD_AUTH_PROVIDERS_CREATE: &str = "auth/providers/create";
pub const METHOD_AUTH_PROVIDERS_READ: &str = "auth/providers/read";
pub const METHOD_AUTH_PROVIDERS_LIST: &str = "auth/providers/list";
pub const METHOD_AUTH_PROVIDERS_DELETE: &str = "auth/providers/delete";
pub const METHOD_AUTH_GITHUB_INSTALLATIONS_LIST: &str = "auth/github/installations/list";
pub const METHOD_AUTH_GITHUB_INSTALLATIONS_GRANT: &str = "auth/github/installations/grant";

// ── Notifications ───────────────────────────────────────────────────────────

pub const NOTIFY_SESSION_STARTED: &str = "session/started";
pub const NOTIFY_SESSION_STATUS_CHANGED: &str = "session/status/changed";
pub const NOTIFY_SESSION_EVENT: &str = "session/event";
pub const NOTIFY_SESSION_RUNS_STARTED: &str = "session/runs/started";
pub const NOTIFY_SESSION_RUNS_COMPLETED: &str = "session/runs/completed";
pub const NOTIFY_ERROR: &str = "error";

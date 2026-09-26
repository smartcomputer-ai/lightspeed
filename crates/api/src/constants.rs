//! JSON-RPC method and notification names.
//!
//! The first segment of a method name is its addressing scope: `session/`
//! methods act inside one session (params carry `sessionId`), `deployment/`
//! methods (see [`crate::deployment`]) address the deployment, and every other
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
pub const METHOD_SESSION_METADATA_PUT: &str = "session/metadata/put";
pub const METHOD_SESSION_RETENTION_PUT: &str = "session/retention/put";
pub const METHOD_SESSION_CLOSE: &str = "session/close";
pub const METHOD_SESSION_DELETE: &str = "session/delete";
pub const METHOD_SESSION_SHARE: &str = "session/share";

// ── Sessions: facets (event log, tools, context, runs) ─────────────────────

pub const METHOD_SESSION_EVENTS_READ: &str = "session/events/read";
pub const METHOD_SESSION_CONTEXT_APPEND: &str = "session/context/append";
pub const METHOD_SESSION_CONTEXT_REMOVE: &str = "session/context/remove";
pub const METHOD_SESSION_CONTEXT_COMPACT: &str = "session/context/compact";
pub const METHOD_SESSION_RUNS_START: &str = "session/runs/start";
pub const METHOD_SESSION_RUNS_LIST: &str = "session/runs/list";
pub const METHOD_SESSION_RUNS_READ: &str = "session/runs/read";
pub const METHOD_SESSION_RUNS_CANCEL: &str = "session/runs/cancel";
pub const METHOD_SESSION_RUNS_APPROVALS_DECIDE: &str = "session/runs/approvals/decide";
pub const METHOD_SESSION_RUNS_STEER: &str = "session/runs/steer";

// ── Sessions: prompt and skill state ────────────────────────────────────────

pub const METHOD_SESSION_SKILLS_LIST: &str = "session/skills/list";

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
pub const METHOD_VFS_WORKSPACES_FILES_READ: &str = "vfs/workspaces/files/read";
pub const METHOD_VFS_WORKSPACES_LIST: &str = "vfs/workspaces/list";
pub const METHOD_VFS_WORKSPACES_UPDATE: &str = "vfs/workspaces/update";
pub const METHOD_VFS_WORKSPACES_DELETE: &str = "vfs/workspaces/delete";

// ── Universe: MCP server catalog ────────────────────────────────────────────

pub const METHOD_MCP_SERVERS_PUT: &str = "mcp/servers/put";
pub const METHOD_MCP_SERVERS_AUTH_DISCOVER: &str = "mcp/servers/auth/discover";
pub const METHOD_MCP_SERVERS_TOOLS_DISCOVER: &str = "mcp/servers/tools/discover";
pub const METHOD_MCP_SERVERS_READ: &str = "mcp/servers/read";
pub const METHOD_MCP_SERVERS_LIST: &str = "mcp/servers/list";
pub const METHOD_MCP_SERVERS_DELETE: &str = "mcp/servers/delete";

// ── Universe: environments and provider presence ────────────────────────────

pub const METHOD_ENVIRONMENTS_CREATE: &str = "environments/create";
pub const METHOD_ENVIRONMENTS_READ: &str = "environments/read";
pub const METHOD_ENVIRONMENTS_LIST: &str = "environments/list";
pub const METHOD_ENVIRONMENTS_CLOSE: &str = "environments/close";
pub const METHOD_ENVIRONMENTS_EXTERNAL_CREATE: &str = "environments/external/create";
pub const METHOD_ENVIRONMENTS_INGRESS_PUT: &str = "environments/ingress/put";
pub const METHOD_ENVIRONMENTS_POWER_PUT: &str = "environments/power/put";
pub const METHOD_ENVIRONMENTS_IDLE_POLICY_PUT: &str = "environments/idle-policy/put";
pub const METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_LIST: &str = "environments/provider-bindings/list";
pub const METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_READ: &str = "environments/provider-bindings/read";
pub const METHOD_ENVIRONMENTS_TEMPLATES_LIST: &str = "environments/templates/list";
pub const METHOD_ENVIRONMENTS_TEMPLATES_READ: &str = "environments/templates/read";
pub const METHOD_ENVIRONMENTS_JOBS_CREATE: &str = "environments/jobs/create";
pub const METHOD_ENVIRONMENTS_JOBS_READ: &str = "environments/jobs/read";
pub const METHOD_ENVIRONMENTS_JOBS_CANCEL: &str = "environments/jobs/cancel";
pub const METHOD_ENVIRONMENTS_REGISTRATION_KEYS_CREATE: &str =
    "environments/registration-keys/create";
pub const METHOD_ENVIRONMENTS_REGISTRATION_KEYS_READ: &str = "environments/registration-keys/read";
pub const METHOD_ENVIRONMENTS_REGISTRATION_KEYS_LIST: &str = "environments/registration-keys/list";
pub const METHOD_ENVIRONMENTS_REGISTRATION_KEYS_REVOKE: &str =
    "environments/registration-keys/revoke";

// ── Universe: auth ──────────────────────────────────────────────────────────

pub const METHOD_AUTH_GRANTS_IMPORT: &str = "auth/grants/import";
pub const METHOD_AUTH_GRANTS_LEASE: &str = "auth/grants/lease";
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

// ── Universe: bots ──────────────────────────────────────────────────────────

pub const METHOD_BOTS_CREATE: &str = "bots/create";
pub const METHOD_BOTS_PUT: &str = "bots/put";
pub const METHOD_BOTS_READ: &str = "bots/read";
pub const METHOD_BOTS_LIST: &str = "bots/list";
pub const METHOD_BOTS_CLOSE: &str = "bots/close";
pub const METHOD_BOTS_DELETE: &str = "bots/delete";
pub const METHOD_BOTS_STATE_READ: &str = "bots/state/read";
pub const METHOD_BOTS_SESSIONS_ROTATE: &str = "bots/sessions/rotate";
pub const METHOD_BOTS_TRIGGERS_PUT: &str = "bots/triggers/put";
pub const METHOD_BOTS_TRIGGERS_READ: &str = "bots/triggers/read";
pub const METHOD_BOTS_TRIGGERS_LIST: &str = "bots/triggers/list";
pub const METHOD_BOTS_TRIGGERS_DELETE: &str = "bots/triggers/delete";
pub const METHOD_BOTS_EVENTS_ADMIT: &str = "bots/events/admit";
pub const METHOD_BOTS_EVENTS_REPLAY: &str = "bots/events/replay";
pub const METHOD_BOTS_EVENTS_LIST: &str = "bots/events/list";
pub const METHOD_BOTS_EVENTS_READ: &str = "bots/events/read";
pub const METHOD_BOTS_FILTERS_TEST: &str = "bots/filters/test";

// ── Universe: channels ──────────────────────────────────────────────────────

pub const METHOD_CHANNELS_ACCOUNTS_CREATE: &str = "channels/accounts/create";
pub const METHOD_CHANNELS_ACCOUNTS_PUT: &str = "channels/accounts/put";
pub const METHOD_CHANNELS_ACCOUNTS_READ: &str = "channels/accounts/read";
pub const METHOD_CHANNELS_ACCOUNTS_LIST: &str = "channels/accounts/list";
pub const METHOD_CHANNELS_ACCOUNTS_DELETE: &str = "channels/accounts/delete";
pub const METHOD_CHANNELS_INBOUND_ADMIT: &str = "channels/inbound/admit";
pub const METHOD_CHANNELS_PAIRINGS_LIST: &str = "channels/pairings/list";
pub const METHOD_CHANNELS_PAIRINGS_DELETE: &str = "channels/pairings/delete";
pub const METHOD_CHANNELS_CONVERSATIONS_READ: &str = "channels/conversations/read";

// ── Notifications ───────────────────────────────────────────────────────────

pub const NOTIFY_SESSION_STARTED: &str = "session/started";
pub const NOTIFY_SESSION_STATUS_CHANGED: &str = "session/status/changed";
pub const NOTIFY_SESSION_EVENT: &str = "session/event";
pub const NOTIFY_SESSION_RUNS_STARTED: &str = "session/runs/started";
pub const NOTIFY_SESSION_RUNS_COMPLETED: &str = "session/runs/completed";
pub const NOTIFY_ERROR: &str = "error";

//! Stable operation IDs.
//!
//! These opaque identifiers remain stable when HTTP methods or resource paths
//! change. The operation catalog associates each ID with its route, input and
//! response types, scope, and policy metadata; clients must use that metadata
//! instead of inferring addressing rules from an ID's spelling.

// ── Connection discovery ──────────────────────────────────────────────────────

pub const METHOD_INITIALIZE: &str = "initialize";

// ── Sessions: lifecycle ─────────────────────────────────────────────────────

pub const METHOD_SESSION_START: &str = "session/start";
pub const METHOD_SESSION_READ: &str = "session/read";
pub const METHOD_SESSION_LIST: &str = "session/list";
pub const METHOD_SESSION_CONFIG_PUT: &str = "session/config/put";
pub const METHOD_SESSION_RENAME: &str = "session/rename";
pub const METHOD_SESSION_METADATA_PUT: &str = "session/metadata/put";
pub const METHOD_SESSION_RETENTION_PUT: &str = "session/retention/put";
pub const METHOD_SESSION_RESTART: &str = "session/restart";
pub const METHOD_SESSION_CLOSE: &str = "session/close";
pub const METHOD_SESSION_DELETE: &str = "session/delete";
pub const METHOD_SESSION_SHARE: &str = "session/share";

// ── Sessions: facets (event log, tools, context, runs) ─────────────────────

pub const METHOD_SESSION_EVENTS_READ: &str = "session/events/read";
pub const METHOD_SESSION_CONTEXT_APPEND: &str = "session/context/append";
pub const METHOD_SESSION_CONTEXT_REMOVE: &str = "session/context/remove";
pub const METHOD_SESSION_CONTEXT_REPLACE: &str = "session/context/replace";
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

pub const METHOD_MODELS_DEFAULTS_READ: &str = "models/defaults/read";
pub const METHOD_MODELS_DEFAULTS_PUT: &str = "models/defaults/put";
pub const METHOD_MODELS_LIST: &str = "models/list";

pub const METHOD_PROFILES_CREATE: &str = "profiles/create";
pub const METHOD_PROFILES_READ: &str = "profiles/read";
pub const METHOD_PROFILES_LIST: &str = "profiles/list";
pub const METHOD_PROFILES_PUT: &str = "profiles/put";
pub const METHOD_PROFILES_DELETE: &str = "profiles/delete";

// ── Universe: content-addressed storage and VFS ─────────────────────────────

pub const METHOD_BLOBS_UPLOAD: &str = "blobs/upload";
pub const METHOD_BLOBS_PUT: &str = "blobs/put";
pub const METHOD_BLOBS_READ: &str = "blobs/read";
pub const METHOD_BLOBS_HAS: &str = "blobs/has";
pub const METHOD_BLOBS_COMPOSE: &str = "blobs/compose";
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
pub const METHOD_ENVIRONMENTS_UPDATE: &str = "environments/update";
pub const METHOD_ENVIRONMENTS_LIST: &str = "environments/list";
pub const METHOD_ENVIRONMENTS_CLOSE: &str = "environments/close";
pub const METHOD_ENVIRONMENTS_EXTERNAL_CREATE: &str = "environments/external/create";
pub const METHOD_ENVIRONMENTS_REGISTERED_PUT: &str = "environments/registered/put";
pub const METHOD_ENVIRONMENTS_REGISTERED_RELEASE: &str = "environments/registered/release";
pub const METHOD_ENVIRONMENTS_INGRESS_PUT: &str = "environments/ingress/put";
pub const METHOD_ENVIRONMENTS_POWER_PUT: &str = "environments/power/put";
pub const METHOD_ENVIRONMENTS_PROVIDERS_LIST: &str = "environments/providers/list";
pub const METHOD_ENVIRONMENTS_PROVIDERS_READ: &str = "environments/providers/read";
pub const METHOD_ENVIRONMENTS_TEMPLATES_LIST: &str = "environments/templates/list";
pub const METHOD_ENVIRONMENTS_TEMPLATES_READ: &str = "environments/templates/read";
pub const METHOD_ENVIRONMENTS_JOBS_CREATE: &str = "environments/jobs/create";
pub const METHOD_ENVIRONMENTS_JOBS_READ: &str = "environments/jobs/read";
pub const METHOD_ENVIRONMENTS_JOBS_CANCEL: &str = "environments/jobs/cancel";
pub const METHOD_ENVIRONMENTS_REGISTRATION_KEYS_CREATE: &str =
    "environments/registration-keys/create";
pub const METHOD_ENVIRONMENTS_REGISTRATION_KEYS_PUT: &str = "environments/registration-keys/put";
pub const METHOD_ENVIRONMENTS_REGISTRATION_KEYS_READ: &str = "environments/registration-keys/read";
pub const METHOD_ENVIRONMENTS_REGISTRATION_KEYS_LIST: &str = "environments/registration-keys/list";
pub const METHOD_ENVIRONMENTS_REGISTRATION_KEYS_UPDATE: &str =
    "environments/registration-keys/update";
pub const METHOD_ENVIRONMENTS_REGISTRATION_KEYS_ROTATE: &str =
    "environments/registration-keys/rotate";
pub const METHOD_ENVIRONMENTS_REGISTRATION_KEYS_REVOKE: &str =
    "environments/registration-keys/revoke";

// ── Universe: auth ──────────────────────────────────────────────────────────

pub const METHOD_CREDENTIALS_GRANTS_IMPORT: &str = "credentials/grants/import";
pub const METHOD_CREDENTIALS_GRANTS_LEASE: &str = "credentials/grants/lease";
pub const METHOD_CREDENTIALS_GRANTS_READ: &str = "credentials/grants/read";
pub const METHOD_CREDENTIALS_GRANTS_LIST: &str = "credentials/grants/list";
pub const METHOD_CREDENTIALS_GRANTS_PUT: &str = "credentials/grants/put";
pub const METHOD_CREDENTIALS_GRANTS_REVOKE: &str = "credentials/grants/revoke";
pub const METHOD_CREDENTIALS_CLIENTS_CREATE: &str = "credentials/clients/create";
pub const METHOD_CREDENTIALS_CLIENTS_READ: &str = "credentials/clients/read";
pub const METHOD_CREDENTIALS_CLIENTS_LIST: &str = "credentials/clients/list";
pub const METHOD_CREDENTIALS_CLIENTS_DELETE: &str = "credentials/clients/delete";
pub const METHOD_CREDENTIALS_FLOWS_START: &str = "credentials/flows/start";
pub const METHOD_CREDENTIALS_FLOWS_READ: &str = "credentials/flows/read";
pub const METHOD_CREDENTIALS_PROVIDERS_CREATE: &str = "credentials/providers/create";
pub const METHOD_CREDENTIALS_PROVIDERS_PUT: &str = "credentials/providers/put";
pub const METHOD_CREDENTIALS_PROVIDERS_READ: &str = "credentials/providers/read";
pub const METHOD_CREDENTIALS_PROVIDERS_LIST: &str = "credentials/providers/list";
pub const METHOD_CREDENTIALS_PROVIDERS_DELETE: &str = "credentials/providers/delete";
pub const METHOD_CREDENTIALS_GITHUB_INSTALLATIONS_LIST: &str =
    "credentials/github/installations/list";
pub const METHOD_CREDENTIALS_GITHUB_INSTALLATIONS_GRANT: &str =
    "credentials/github/installations/grant";

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
pub const METHOD_BOTS_TRIGGERS_SECRETS_READ: &str = "bots/triggers/secrets/read";
pub const METHOD_BOTS_TRIGGERS_DELETE: &str = "bots/triggers/delete";
pub const METHOD_BOTS_EVENTS_ADMIT: &str = "bots/events/admit";
pub const METHOD_BOTS_EVENTS_REPLAY: &str = "bots/events/replay";
pub const METHOD_BOTS_EVENTS_LIST: &str = "bots/events/list";
pub const METHOD_BOTS_EVENTS_READ: &str = "bots/events/read";
pub const METHOD_BOTS_FILTERS_TEST: &str = "bots/filters/test";

// ── Notifications ───────────────────────────────────────────────────────────

// ── Transcriptions ───────────────────────────────────────────────────────────

pub const METHOD_TRANSCRIPTIONS_START: &str = "transcriptions/start";
pub const METHOD_TRANSCRIPTIONS_READ: &str = "transcriptions/read";
pub const METHOD_TRANSCRIPTIONS_CANCEL: &str = "transcriptions/cancel";

/// Prefix of every environment registration key secret.
pub const REGISTRATION_KEY_SECRET_PREFIX: &str = "lsrk_";
/// Random bytes in a key secret. After its prefix a secret is these bytes
/// encoded as base64url without padding, whether the runtime generated it or
/// a caller supplied it.
pub const KEY_SECRET_RANDOM_BYTES: usize = 32;

pub const METHOD_UNIVERSE_WATCH: &str = "universe/watch";

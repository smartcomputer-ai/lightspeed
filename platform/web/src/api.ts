import type {
  ContextEntryView,
  EnvironmentCredentialSourceView,
  EnvironmentCredentialView,
  EnvironmentProviderBindingView,
  EnvironmentTemplateView,
  EnvironmentView,
  SessionEventView,
  SessionEventsReadResponse,
  ToolCallDisplayView,
} from "@lightspeed/agent-client";

/// Thin fetch wrapper for /api/v1 (cookie-authenticated, same origin).

export class ApiError extends Error {
  constructor(
    public status: number,
    public body: unknown,
  ) {
    super(extractMessage(body) ?? `API error ${status}`);
  }
}

function extractMessage(body: unknown): string | null {
  if (body && typeof body === "object" && "error" in body) {
    const error = (body as { error: unknown }).error;
    return typeof error === "string" ? error : null;
  }
  return null;
}

export async function api<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    headers: body !== undefined ? { "content-type": "application/json" } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
    credentials: "same-origin",
  });
  const text = await res.text();
  const json: unknown = text ? JSON.parse(text) : null;
  if (!res.ok) {
    throw new ApiError(res.status, json);
  }
  return json as T;
}

export interface Universe {
  id: string;
  organizationId: string;
  lightspeedUniverseId: string;
  name: string;
  /// Immutable URL segment (the better-auth org slug).
  slug: string;
  gatewayUrl: string | null;
  status: "active" | "archived";
  createdAt: string;
  /// Own membership role; null for platform admins browsing a universe
  /// they are not a member of.
  role?: string | null;
}

/// Engine-side universe inventory entry (operator/universes/list view).
export interface EngineUniverse {
  universeId: string;
  sessions: number;
  workspaces: number;
  profiles: number;
  blobBytes: number;
  createdAtMs: number;
  lastActivityAtMs?: number | null;
}

/// Admin reconciliation: each platform row's engine status, plus engine
/// universes no platform row links to (orphans).
export interface UniverseReconcile {
  platform: {
    id: string;
    lightspeedUniverseId: string;
    engine: "ok" | "missing" | "unchecked";
  }[];
  orphans: EngineUniverse[];
}

export interface Member {
  id: string;
  userId: string;
  role: string;
  email: string;
  name: string;
  createdAt: string;
}

export interface UniverseApiKey {
  keyPrefix: string;
  displayName?: string | null;
  createdAtMs: number;
  revokedAtMs?: number | null;
  lastUsedAtMs?: number | null;
}

export interface UniverseApiKeyCreated {
  apiKey: UniverseApiKey;
  /// Returned once at creation and never recoverable from Lightspeed.
  secret: string;
}

export interface UniverseSetup {
  id: string;
  name: string;
  description: string;
  version: number;
  available: boolean;
  status: "available" | "installing" | "ready" | "failed" | "unavailable";
  installedVersion?: number;
  error?: string;
  resources?: {
    keyPrefix?: string;
    grantId?: string;
    serverId?: string;
    profileId?: string;
  };
}

/// Gateway passthrough shapes (the engine owns the full documents).
export interface ProfileSummary {
  profileId: string;
  displayName?: string | null;
  description?: string | null;
  revision: number;
  updatedAtMs: number;
}

export type ProfileDocument = {
  profileId: string;
  activeEnvironmentId?: string | null;
  revision?: number;
  createdAtMs?: number;
  updatedAtMs?: number;
} & Record<string, unknown>;

export type InlineProfile = {
  config?: Record<string, unknown>;
  instructions?:
    | { type: "text"; text: string }
    | { type: "textRef"; blobRef: string };
  activeEnvironmentId?: string | null;
};

export type ProfileSource =
  | { kind: "named"; profileId: string }
  | { kind: "inline"; profile: InlineProfile };

/// Provider-discovered model route from `models/list`.
export interface ModelOption {
  providerId: string;
  apiKind: string;
  model: string;
  displayName: string;
  capabilities: {
    maxInputTokens?: number | null;
    maxOutputTokens?: number | null;
    parallelToolUse?: boolean | null;
    reasoningEfforts?: string[] | null;
  };
  source: "provider";
  fetchedAtMs: number;
}

export interface ModelListResponse {
  models?: ModelOption[];
  providers?: {
    providerId: string;
    apiKinds: string[];
    fetchedAtMs?: number | null;
    error?: string | null;
  }[];
}

export interface AuthGrantOption {
  grantId: string;
  providerId: string;
  providerKind: string;
  displayName?: string | null;
  subjectHint?: string | null;
  status: "active" | "needsReauth" | "revoked" | "failed";
}

export interface SecretGrant extends AuthGrantOption {
  principal: {
    kind?: "user" | "serviceAccount" | "universeDefault" | string;
    id?: string | null;
  };
  scopes?: string[];
  audience?: string | null;
  hasAccessToken: boolean;
  hasRefreshToken: boolean;
  expiresAtMs?: number | null;
  createdAtMs: number;
  updatedAtMs: number;
}

export interface SecretProvider {
  /// Friendly ModelSelection.providerId, such as `openai`.
  providerId: string;
  /// Namespaced auth-provider row id, such as `model:openai`.
  credentialId: string;
  /// False only for legacy/malformed rows that the model runtime will ignore.
  usableForModels: boolean;
  providerKind:
    | "staticBearer"
    | "mcpOAuth"
    | "gitHubApp"
    | "gitHubAppUser"
    | "gitHubOAuthApp"
    | "customOAuth"
    | "modelApiKey"
    | "modelOAuth";
  displayName?: string | null;
  config:
    | { type: "modelApiKey" }
    | { type: "modelOAuth"; grantId: string; audience?: string | null }
    | { type: "githubApp"; appId: string; apiBaseUrl: string };
  hasCredential: boolean;
  status: "active" | "needsConfiguration" | "disabled";
  createdAtMs: number;
  updatedAtMs: number;
}

export interface SecretsInventory {
  providers: SecretProvider[];
  grants: SecretGrant[];
}

/// Engine MCP server record (mcp/servers/list view). The optional credential
/// is a non-secret universe-owned grant reference; token material is never returned.
export interface McpServer {
  serverId: string;
  displayName?: string | null;
  serverUrl: string;
  transport: "streamableHttp" | "sse" | "auto";
  defaultServerLabel: string;
  description?: string | null;
  allowedTools?: string[] | null;
  approvalDefault: "providerDefault" | "always" | "never";
  deferLoadingDefault?: boolean | null;
  authPolicy: { type: string } & Record<string, unknown>;
  credential?: { type: "authGrant"; grantId: string } | null;
  status: "active" | "needsAuthConfig" | "unverified" | "disabled";
  revision: number;
  createdAtMs: number;
  updatedAtMs: number;
}

/// Exact projections of the generated Lightspeed environment contract.
export type Environment = EnvironmentView;
export type EnvironmentProviderBinding = EnvironmentProviderBindingView;
export type EnvironmentTemplate = EnvironmentTemplateView;
export type EnvironmentCredentialSource = EnvironmentCredentialSourceView;
export type EnvironmentCredential = EnvironmentCredentialView;

export interface SessionSummary {
  id: string;
  displayName?: string | null;
  createdAtMs: number;
  updatedAtMs: number;
  lifecycleStatus: "new" | "open" | "closed";
  managed: boolean;
}

export interface SessionManagement {
  version: number;
  lifecycleController?: WorkflowEndpoint | null;
  tools?: ManagedWorkflowTool[];
}

export interface WorkflowEndpoint {
  workflowId: string;
  workflowKind: string;
}

export interface ManagedWorkflowTool {
  toolId: string;
  name: string;
  semanticType: string;
  target: "bound" | "start";
  completion: "accepted" | "promises";
}

export interface SessionView {
  id: string;
  displayName?: string | null;
  createdAtMs: number;
  updatedAtMs: number;
  status: "notLoaded" | "idle" | "active" | "closed" | "error";
  managed: boolean;
  activeEnvironmentId?: string | null;
  config?: Record<string, unknown> | null;
  configRevision: number;
  management?: SessionManagement | null;
}

export type WorkspaceLinkTarget =
  | { type: "workspace"; workspaceId: string }
  | { type: "snapshot"; snapshotRef: string };

export interface WorkspaceLink {
  path: string;
  access: "readOnly" | "readWrite";
  target: WorkspaceLinkTarget;
}

export type WorkspaceLinkDraft = {
  path?: string;
  access?: string;
  target?: {
    type?: string;
    workspaceId?: string;
    snapshotRef?: string;
  } & Record<string, unknown>;
} & Record<string, unknown>;

export interface SessionInstructionState {
  text: string | null;
  contextRevision: number;
  active: Array<{
    key: string | null;
    contentRef: string;
    preview: string | null;
  }>;
}

export interface SessionListPage {
  sessions: SessionSummary[];
  nextCursor: string | null;
}

/// Transcript wire shapes come directly from Lightspeed so new event fields
/// and statuses cannot silently drift from the browser reducer.
export type SessionItem = ContextEntryView;
export type ToolCallDisplay = ToolCallDisplayView;
export type SessionEvent = SessionEventView;
export type SessionEventsPage = SessionEventsReadResponse;

/// POST …/sessions/:id/messages — the run was accepted; the reply
/// arrives through the event tail.
export interface SessionRunAccepted {
  run: { id: string; status: string };
}

/// Engine workspace view, straight from `vfs/workspaces/list`.
export interface WorkspaceRow {
  workspaceId: string;
  displayName?: string | null;
  headSnapshotRef: string;
  revision: number;
  files: number;
  bytes: number;
  createdAtMs: number;
  updatedAtMs: number;
}

/// VFS manifest tree (engine contract: snake_case, kind-tagged).
export interface VfsFileEntry {
  kind: "file";
  blob_ref: string;
  size_bytes: number;
  media_type?: string;
  executable: boolean;
}
export interface VfsDirEntry {
  kind: "directory";
  entries: Record<string, VfsTreeEntry>;
}
export type VfsTreeEntry = VfsFileEntry | VfsDirEntry;

export interface WorkspaceTree {
  workspace: WorkspaceRow;
  manifest: {
    schema_version: string;
    root: { entries: Record<string, VfsTreeEntry> };
    totals: { files: number; bytes: number };
  };
}

export interface BlobContent {
  blobRef: string;
  bytes: number;
  bytesBase64: string;
}

export interface Binding {
  id: string;
  universeId: string;
  channelAccountId: string;
  name: string;
  matchScope: "direct" | "group" | null;
  profileId: string | null;
  sessionKey: string;
  pairingCode: string | null;
  priority: number;
  enabled: boolean;
  createdAt: string;
  channelAccount: Pick<
    ChannelAccount,
    "id" | "provider" | "accountId" | "displayName" | "enabled"
  >;
}

export interface ChannelAccount {
  id: string;
  provider: "telegram" | "whatsapp";
  accountId: string;
  displayName: string;
  settings: { printQr?: boolean };
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface ChannelConnectorHealth {
  version: 1;
  provider: "telegram" | "whatsapp";
  accountId: string;
  state: "starting" | "ready" | "disconnected" | "stopping" | "stopped";
  ingressConnected: boolean;
  activityWorkerReady: boolean;
  reconnectAttempts: number;
  detail?: string;
  lastError?: string;
  lastErrorAtMs?: number;
  changedAtMs: number;
}

export interface ChannelConnectorStatus {
  url: string;
  reachable: boolean;
  httpStatus: number | null;
  health?: ChannelConnectorHealth;
  error?: string;
}

export interface ChannelsStatus {
  connectors: ChannelConnectorStatus[];
}

/// Foundry: persistent software-system managers, event state, and releases.
export interface FoundryPack {
  id: string;
  universeId: string;
  name: string;
  kind: "workflow";
  repoUrl: string;
  managerProfileId: string;
  environmentId: string | null;
  runtimeTarget: FoundryRuntimeTarget | null;
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface FoundryRuntimeTarget {
  kind: "docker" | "kubernetes" | "ssh" | "external";
  name: string;
  metadata?: Record<string, string>;
}

export interface FoundryEvent {
  version: 1;
  id: string;
  ref: string;
}

export interface FoundryRecentEvent {
  id: string;
  ref: string;
  status: "handled" | "deferred" | "ignored" | "blocked" | "unresolved" | "run_failed";
  runId?: string;
  summary?: string;
  failure?: string;
}

export interface FoundryPackState {
  sessionId: string;
  controllerStatus: "initializing" | "idle" | "manager_busy" | "delivering_event" | "degraded";
  activeEvent: FoundryEvent | null;
  activeRunId: string | null;
  sessionReady: boolean;
  pendingEvents: FoundryEvent[];
  pendingEventCount: number;
  recentEvents: FoundryRecentEvent[];
  eventsProcessed: number;
  duplicateEventCount: number;
  duplicateEmissionCount: number;
  managerProfileId: string;
  appliedProfileRevision: number | null;
  environmentId: string | null;
  lastError: string | null;
}

export interface FoundryRelease {
  id: string;
  packId: string;
  invocationId: string;
  sourceCommit: string;
  artifactDigest: string;
  target: string;
  outcome: "succeeded" | "failed" | "rolled_back";
  initiatedBy: string | null;
  smokePassed: boolean | null;
  detailsRef: string | null;
  createdAt: string;
}

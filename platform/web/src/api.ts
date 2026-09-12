import type {
  ContextEntryView,
  RunSummaryView,
  RunStatus,
  EnvironmentCredentialSourceView,
  EnvironmentCredentialView,
  EnvironmentProviderBindingView,
  EnvironmentRegistrationKeyCreateResponse,
  EnvironmentRegistrationKeyView,
  EnvironmentTemplateView,
  EnvironmentView,
  ProfileEnvironment as ProfileEnvironmentView,
  ProfileSessionRetention as ProfileSessionRetentionView,
  SessionEnvironmentOverride as SessionEnvironmentOverrideView,
  SessionEventView,
  SessionEventsReadResponse,
  ToolCallDisplayView,
} from "@lightspeed-ai/agent-client";

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
  if (!body || typeof body !== "object" || !("error" in body)) return null;
  const { error, issues, failure } = body as {
    error: unknown;
    issues?: unknown;
    failure?: unknown;
  };
  if (typeof error !== "string") return null;
  if (typeof failure === "string" && failure) return `${error}: ${failure}`;
  if (Array.isArray(issues)) {
    const details = issues
      .slice(0, 3)
      .map((issue) => {
        if (!issue || typeof issue !== "object") return null;
        const { path, message } = issue as { path?: unknown; message?: unknown };
        if (typeof message !== "string") return null;
        const at = Array.isArray(path) && path.length > 0 ? `${path.join(".")}: ` : "";
        return `${at}${message}`;
      })
      .filter((detail): detail is string => detail !== null);
    if (details.length > 0) return `${error} — ${details.join("; ")}`;
  }
  return error;
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

export type ProfileEnvironment = ProfileEnvironmentView;
export type ProfileSessionRetention = ProfileSessionRetentionView;
export type SessionEnvironmentOverride = SessionEnvironmentOverrideView;

export type ProfileDocument = {
  profileId: string;
  metadata?: Record<string, string>;
  retention?: ProfileSessionRetention | null;
  environment?: ProfileEnvironment | null;
  revision?: number;
  createdAtMs?: number;
  updatedAtMs?: number;
} & Record<string, unknown>;

export type InlineProfile = {
  metadata?: Record<string, string>;
  retention?: ProfileSessionRetention | null;
  config?: Record<string, unknown>;
  instructions?:
    | { type: "text"; text: string }
    | { type: "textRef"; blobRef: string };
  environment?: ProfileEnvironment | null;
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
  createdAtMs?: number | null;
  capabilities: {
    maxInputTokens?: number | null;
    maxOutputTokens?: number | null;
    parallelToolUse?: boolean | null;
    reasoningEfforts?: string[] | null;
  };
  source: "provider";
  fetchedAtMs: number;
}

export interface ModelProviderDiscovery {
  providerId: string;
  apiKinds: string[];
  fetchedAtMs?: number | null;
  error?: string | null;
  /// Stable credential signal from the engine (no string matching on `error`).
  credential: "configured" | "missing" | "invalid" | "notRequired";
  credentialSource: "universe" | "deployment" | "none";
}

export interface ModelListResponse {
  models?: ModelOption[];
  providers?: ModelProviderDiscovery[];
}

export interface AuthGrantOption {
  grantId: string;
  providerId: string;
  providerKind: string;
  displayName?: string | null;
  subjectHint?: string | null;
  status: "active" | "needsReauth" | "revoked" | "failed";
  exposure: "brokered" | "retrievable";
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
  lastLeasedAtMs?: number | null;
  leaseCount: number;
  /// Non-secret provider metadata (GitHub App grants: installation id,
  /// account login, permissions, repository selection).
  metadata?: Record<string, unknown>;
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
    | "customOAuth"
    | "modelApiKey"
    | "modelOAuth"
    | "modelEndpoint";
  displayName?: string | null;
  config:
    | { type: "modelApiKey"; endpoint?: ModelEndpointConfig | null }
    | {
        type: "modelOAuth";
        grantId: string;
        audience?: string | null;
        endpoint?: ModelEndpointConfig | null;
      }
    | { type: "modelEndpoint"; endpoint: ModelEndpointConfig }
    | { type: "githubApp"; appId: string; apiBaseUrl: string };
  hasCredential: boolean;
  status: "active" | "needsConfiguration" | "disabled";
  createdAtMs: number;
  updatedAtMs: number;
}

export interface ModelEndpointConfig {
  baseUrl: string;
  headers?: Record<string, string>;
  apiKinds: Array<"openai:responses" | "openai:completions">;
}

export interface SecretsInventory {
  providers: SecretProvider[];
  grants: SecretGrant[];
}

/// Universe-owned GitHub App (BYO provider). The private key is stored by the
/// engine and never returned; `hasCredential` is the only trace of it.
export interface GitHubApp {
  providerId: string;
  providerKind: SecretProvider["providerKind"];
  displayName?: string | null;
  config: { type: "githubApp"; appId: string; apiBaseUrl: string };
  hasCredential: boolean;
  status: "active" | "needsConfiguration" | "disabled";
  createdAtMs: number;
  updatedAtMs: number;
}

/// One installation of a GitHub App, live from GitHub.
export interface GitHubInstallation {
  installationId: number;
  accountLogin?: string | null;
  repositorySelection?: string | null;
  permissions?: Record<string, unknown>;
}

/// Result of the Platform subscription import: the grant plus how to bind it.
export interface SubscriptionImportResult {
  grant: SecretGrant;
  shape: "token" | "codexTokenSet";
}

export interface GitHubIntegration {
  apps: GitHubApp[];
  /// Installation grants (`gitHubApp` kind); `metadata.installation_id` links
  /// each grant to its installation.
  grants: SecretGrant[];
}

/// Engine MCP server record (mcp/servers/list view). The optional credential
/// is a non-secret universe-owned grant reference; token material is never returned.
export interface McpServer {
  serverId: string;
  displayName?: string | null;
  serverUrl: string;
  defaultServerLabel: string;
  description?: string | null;
  allowedTools?: string[] | null;
  execution: "provider" | "native";
  exposure: "inject" | "search";
  approvalDefault: "always" | "never";
  deferLoadingDefault?: boolean | null;
  allowPrivateNetwork: boolean;
  authPolicy: { type: string } & Record<string, unknown>;
  credential?: { type: "authGrant"; grantId: string } | null;
  status: "active" | "needsAuthConfig" | "unverified" | "disabled";
  revision: number;
  createdAtMs: number;
  updatedAtMs: number;
}

export interface McpAdvertisedTool {
  name: string;
  title?: string | null;
  description?: string | null;
  annotations?: {
    readOnlyHint?: boolean | null;
    destructiveHint?: boolean | null;
    idempotentHint?: boolean | null;
    openWorldHint?: boolean | null;
  } | null;
}

export type McpToolDiscovery =
  | { status: "success"; tools: McpAdvertisedTool[] }
  | {
      status: "failure";
      code:
        | "credentialAbsent"
        | "grantNeedsReauth"
        | "grantAudienceMismatch"
        | "unauthorized"
        | "forbidden"
        | "additionalConsentRequired"
        | "remoteRateLimited"
        | "remoteFailure"
        | "unreachable"
        | "invalidResponse"
        | "unsupportedProtocol"
        | "paginationLimit"
        | "responseTooLarge";
      message: string;
      requiredScopes?: string[];
    };

export interface McpOAuthFlowStart {
  flowId: string;
  authorizeUrl: string;
  expiresAtMs: number;
  serverRevision: number;
}

export interface McpOAuthFlow {
  flowId: string;
  clientId: string;
  providerId: string;
  status: "pending" | "completed" | "failed" | "expired";
  grantId?: string | null;
  error?: string | null;
  expiresAtMs: number;
  createdAtMs: number;
  updatedAtMs: number;
}

export interface McpServerAuthDiscovery {
  oauth?: {
    resource: string;
    authorizationServers: string[];
    scopesSupported: string[];
  } | null;
}

/// Exact projections of the generated Lightspeed environment contract.
export type Environment = EnvironmentView;
export type EnvironmentProviderBinding = EnvironmentProviderBindingView;
export type EnvironmentTemplate = EnvironmentTemplateView;
export type EnvironmentCredentialSource = EnvironmentCredentialSourceView;
export type EnvironmentCredential = EnvironmentCredentialView;
export type EnvironmentRegistrationKey = EnvironmentRegistrationKeyView;
export type EnvironmentRegistrationKeyCreated = EnvironmentRegistrationKeyCreateResponse;

/// Sub-agent lineage: who delegated the session, under which root,
/// at what depth, from which pinned profile revision. Provenance only.
export interface SessionOrigin {
  kind: "subagent";
  parentSessionId: string;
  parentRunId: string;
  rootSessionId: string;
  depth: number;
  invocationId: string;
  agent: { profileId: string; revision: number };
  limits: { maxDepth: number; maxDescendants: number; maxConcurrent: number; deadlineMs: number };
}

export interface SessionSummary {
  id: string;
  displayName?: string | null;
  /// Descriptive key/value metadata; absent or empty when none was set.
  metadata?: Record<string, string>;
  createdAtMs: number;
  updatedAtMs: number;
  closedAtMs?: number | null;
  lifecycleStatus: "new" | "open" | "closed";
  retention: SessionRetention;
  managed: boolean;
  origin?: SessionOrigin | null;
}

export interface SessionRetention {
  rootSessionId: string;
  deleteAfterCloseMs?: number | null;
  deleteAtMs?: number | null;
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
  metadata?: Record<string, string>;
  createdAtMs: number;
  updatedAtMs: number;
  closedAtMs?: number | null;
  status: "notLoaded" | "idle" | "active" | "closed" | "error";
  retention: SessionRetention;
  managed: boolean;
  activeEnvironmentId?: string | null;
  config?: Record<string, unknown> | null;
  configRevision: number;
  management?: SessionManagement | null;
  origin?: SessionOrigin | null;
  /// Bounded newest-first run summary page. Authoritative for recent run
  /// state; the event tail is the live, incremental transcript view.
  runs?: SessionRunView[];
  activeRun?: SessionRunView | null;
}

export type SessionRunView = RunSummaryView;
export type SessionRunStatus = RunStatus;

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

/// POST …/sessions/:id/messages — the run was accepted (`running`, or
/// `queued` behind an active run); the reply arrives through the event tail.
export interface SessionRunAccepted {
  run: { id: string; status: SessionRunStatus };
}

/// POST …/runs/:runId/steer — the steering was admitted into the active run
/// and reaches the model at its next turn boundary.
export interface SessionRunSteered {
  steeringId: string;
  run: { id: string; status: SessionRunStatus };
}

/// POST …/runs/:runId/cancel — the cancel was admitted; `cancelling` for an
/// active run (terminal `cancelled` follows on the tail), `cancelled` for a
/// queued one.
export interface SessionRunCancelled {
  run: { id: string; status: SessionRunStatus };
}

export interface SessionRunApprovalsDecided {
  results: Array<{
    approvalId: string;
    status: "decided" | "failed";
    failure?: { kind: string; message: string };
  }>;
  run: SessionRunView;
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

/// Bot and channel wire types come from the generated core API client: the
/// platform routes are passthroughs, so the browser reads the
/// core response shapes verbatim. Only the connector health report — a
/// platform-owned surface — keeps a local shape.
export type {
  BotActiveDeliverySnapshot,
  BotBreaker,
  BotBufferSnapshot,
  BotCloseResponse,
  BotCoalescePolicy,
  BotControllerSnapshot,
  BotControllerStatus,
  BotCreateResponse,
  BotDeleteResponse,
  BotDeliverPolicy,
  BotEventDocument,
  BotEventInput,
  BotEventListResponse,
  BotEventMedia,
  BotEventOutcome,
  BotEventReadResponse,
  BotEventReplyRef,
  BotEventSender,
  BotEventView,
  BotFilterTestResponse,
  BotId,
  BotInput,
  BotListItem,
  BotListResponse,
  BotReadResponse,
  BotRecentDeliverySnapshot,
  BotRoutedSessionView,
  BotSessionKind,
  BotSessionSnapshot,
  BotSetupStatus,
  BotStateReadResponse,
  BotStateView,
  BotTriggerDisabledReason,
  BotTriggerId,
  BotTriggerInput,
  BotTriggerListResponse,
  BotTriggerReadResponse,
  BotTriggerRoute,
  BotTriggerView,
  BotView,
  BotWhenBusy,
  ChannelAccountInput,
  ChannelAccountListResponse,
  ChannelAccountReadResponse,
  ChannelAccountSettings,
  ChannelAccountView,
  ChannelPairedVia,
  ChannelPairingListResponse,
  ChannelPairingView,
  ChannelProvider,
  ChatAccess,
  ChatActivation,
  ChatGroupActivation,
  ChatPairing,
  ChatScope,
  ChatTurnAccess,
  LlmUsageView,
  OperatorChannelAccountListResponse,
  OperatorChannelAccountView,
  PollCursorSpec,
  PollCursorState,
  PollHttpAuth,
  PollSource,
  SessionSummaryView,
  WebhookPreset,
  WebhookVerification,
} from "@lightspeed-ai/agent-client";

import type { BotView } from "@lightspeed-ai/agent-client";

export function botLabel(bot: Pick<BotView, "botId" | "displayName">): string {
  return bot.displayName ?? bot.botId;
}

/// Connector host health, aggregated by the platform server from each
/// connector's /healthz (`GET /api/v1/status/channels`). Platform-owned;
/// unrelated to the core channel account records.
export interface ChannelConnectorHealth {
  version: 1;
  universeId?: string;
  provider: string;
  accountId: string;
  state: "starting" | "ready" | "disconnected" | "failed" | "stopping" | "stopped";
  ingressConnected: boolean;
  activityWorkerReady: boolean;
  reconnectAttempts: number;
  detail?: string;
  lastError?: string;
  lastErrorAtMs?: number;
  changedAtMs: number;
}

export interface ChannelConnectorHostHealth {
  version: 1;
  state: "starting" | "ready" | "degraded" | "stopping" | "stopped";
  startedAtMs: number;
  discovery: {
    passes: number;
    lastSuccessAtMs?: number;
    lastError?: string;
    lastErrorAtMs?: number;
  };
  accounts: ChannelConnectorHealth[];
}

export interface ChannelConnectorStatus {
  url: string;
  reachable: boolean;
  httpStatus: number | null;
  health?: ChannelConnectorHostHealth | ChannelConnectorHealth;
  error?: string;
}

export interface ChannelsStatus {
  connectors: ChannelConnectorStatus[];
}

export interface UniverseChannelStatus {
  accounts: ChannelConnectorHealth[];
}

export function connectorAccountHealth(
  status: ChannelConnectorStatus,
): ChannelConnectorHealth[] {
  const health = status.health;
  if (!health) return [];
  return "accounts" in health ? health.accounts : [health];
}

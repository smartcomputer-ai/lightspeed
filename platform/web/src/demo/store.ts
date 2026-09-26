/// In-memory state behind the browser demo. Fixtures fill it at boot, the
/// stub routes read and mutate it, and nothing survives a reload.
import type { UniverseRole } from "@lightspeed/platform-shared";
import type {
  BlobContent,
  ChannelsStatus,
  EngineUniverse,
  Environment,
  EnvironmentCredential,
  EnvironmentProviderBinding,
  EnvironmentRegistrationKey,
  EnvironmentTemplate,
  GitHubApp,
  McpOAuthFlow,
  McpServer,
  Member,
  ModelListResponse,
  ProfileDocument,
  SecretsInventory,
  SessionSummary,
  SessionView,
  Universe,
  UniverseApiKey,
  UniverseSetup,
  WorkspaceRow,
  WorkspaceTree,
} from "@/api";
import type {
  BotControllerSnapshot,
  BotEventView,
  BotTriggerView,
  BotView,
  ChannelAccountView,
  ChannelPairingView,
  ContextEntryView,
  DeploymentApiKeyView,
  DeploymentEnvironmentProviderView,
  MethodGroup,
  RunView,
  SessionEventView,
  SessionSummaryView,
  ToolCallDisplayView,
} from "@lightspeed-ai/agent-client";

/// better-auth user shape as the client sees it (`admin` plugin fields
/// included); everything the demo user is.
export interface DemoUser {
  id: string;
  name: string;
  email: string;
  role: string;
  emailVerified: boolean;
  image: string | null;
  banned: boolean;
  createdAt: string;
  updatedAt: string;
}

/// One tool call inside a scripted assistant turn.
export interface DemoToolCall {
  toolId: string;
  name: string;
  arguments: Record<string, unknown>;
  display: ToolCallDisplayView;
  output: string;
  isError?: boolean;
}

/// A scripted assistant turn: optional thinking, optional tool batch, then
/// the reply. The engine simulation turns this into engine-shaped events.
export interface DemoTurn {
  thinking?: string;
  tools?: DemoToolCall[];
  text: string;
}

export interface ResponderContext {
  store: DemoStore;
  universe: UniverseState;
  session: SessionRecord;
  /// 1-based count of user turns in this session, for scripted sequences.
  turn: number;
}

/// Produces the assistant's turn for a user message.
export type DemoResponder = (input: string, context: ResponderContext) => DemoTurn;

export interface SessionRecord {
  view: SessionView;
  events: SessionEventView[];
  activeContext: { revision: number; entries: ContextEntryView[] };
  /// Session-level custom instructions; null when only defaults apply.
  instructions: string | null;
  /// submissionId → run, so client retries dedupe like the engine.
  submissions: Map<string, RunView>;
  /// All detailed runs; `view.runs` carries the bounded summary shape.
  runs: Map<string, RunView>;
  /// Runs queued behind the active one.
  queue: Array<{ runId: string; begin: () => void }>;
  /// Steering admitted while a run is in flight; consumed at the run's
  /// next turn boundary, where the entry carries its steering source.
  steering: Array<{ text: string; steeringId: string; origin?: string }>;
  timers: Set<ReturnType<typeof setTimeout>>;
  /// Long-poll wakers, notified on every appended event.
  waiters: Set<() => void>;
  /// User turns so far (drives scripted responders).
  turns: number;
  responder?: DemoResponder;
}

export interface WorkspaceRecord {
  row: WorkspaceRow;
  manifest: WorkspaceTree["manifest"];
}

export interface BotRecord {
  /// Core wire shape; `eventSeq` is recomputed from `events` when served.
  bot: BotView;
  /// Keyed by `triggerId`.
  triggers: Map<string, BotTriggerView>;
  /// Newest last; `seq` is the bot's #N.
  events: BotEventView[];
  /// The controller's live snapshot (core wire shape).
  state: BotControllerSnapshot;
  /// Sub-agent sessions delegated under the bot's sessions.
  descendants: SessionSummaryView[];
}

export interface UniverseState {
  universe: Universe;
  members: Member[];
  apiKeys: UniverseApiKey[];
  profiles: Map<string, ProfileDocument>;
  sessions: Map<string, SessionRecord>;
  workspaces: Map<string, WorkspaceRecord>;
  environments: Map<string, Environment>;
  /// Registration keys (core wire shape) with the secret the demo minted,
  /// so a revoke or a re-mint can behave like the real gateway.
  registrationKeys: EnvironmentRegistrationKey[];
  providerBindings: EnvironmentProviderBinding[];
  environmentTemplates: EnvironmentTemplate[];
  environmentCredentials: EnvironmentCredential[];
  mcpServers: Map<string, McpServer>;
  oauthFlows: Map<string, McpOAuthFlow>;
  secrets: SecretsInventory;
  githubApps: GitHubApp[];
  models: ModelListResponse;
  setups: UniverseSetup[];
  bots: Map<string, BotRecord>;
  /// Universe channel accounts (core wire shape), keyed by `accountId`.
  channelAccounts: Map<string, ChannelAccountView>;
  /// Conversation → bot pairing rows (core wire shape).
  channelPairings: ChannelPairingView[];
  /// Fallback responder for sessions without their own script.
  responder: DemoResponder;
}

export interface UniverseInit {
  id?: string;
  slug: string;
  name: string;
  lightspeedUniverseId?: string;
  /// Membership role of the demo user; null = platform admin browsing.
  role?: UniverseRole | null;
  createdAt?: string;
  responder?: DemoResponder;
}

export const DEFAULT_INSTRUCTIONS = "You are a helpful assistant.";

const fallbackResponder: DemoResponder = (input) => ({
  text: `This is the Lightspeed demo — replies are scripted, not generated. You said:\n\n> ${input.slice(0, 300)}`,
});

export class DemoStore {
  readonly users = new Map<string, DemoUser>();
  currentUser: DemoUser;
  readonly universes = new Map<string, UniverseState>();
  /// Engine universes no platform row links to (admin reconcile view).
  readonly orphanEngineUniverses: EngineUniverse[] = [];
  readonly environmentProviders = new Map<string, DeploymentEnvironmentProviderView>();
  /// Deployment keys, and what each universe key may call beyond the
  /// universe page's default of every universe group (admin API keys page).
  readonly deploymentKeys: DeploymentApiKeyView[] = [];
  readonly keyGrants = new Map<string, { groups: MethodGroup[]; assertActor: boolean }>();
  channelsStatus: ChannelsStatus = { connectors: [] };
  readonly blobs = new Map<string, BlobContent>();
  readonly defaultInstructionsRef: string;
  private readonly counters = new Map<string, number>();

  constructor(currentUser: DemoUser) {
    this.currentUser = currentUser;
    this.users.set(currentUser.id, currentUser);
    this.defaultInstructionsRef = this.putText(DEFAULT_INSTRUCTIONS);
  }

  /// Counter ids (`prefix-<n>`): readable and stable within one page load.
  nextId(prefix: string): string {
    const next = (this.counters.get(prefix) ?? 0) + 1;
    this.counters.set(prefix, next);
    return `${prefix}-${next}`;
  }

  putText(text: string): string {
    return this.putBytes(new TextEncoder().encode(text));
  }

  putBytes(bytes: Uint8Array): string {
    const blobRef = this.nextId("blob");
    this.blobs.set(blobRef, { blobRef, bytes: bytes.length, bytesBase64: bytesToBase64(bytes) });
    return blobRef;
  }

  readText(blobRef: string): string | null {
    const blob = this.blobs.get(blobRef);
    return blob ? base64ToText(blob.bytesBase64) : null;
  }

  universe(id: string | undefined): UniverseState | null {
    return id ? (this.universes.get(id) ?? null) : null;
  }

  universeBySlug(slug: string): UniverseState | null {
    for (const state of this.universes.values()) {
      if (state.universe.slug === slug) return state;
    }
    return null;
  }

  addUniverse(init: UniverseInit): UniverseState {
    const id = init.id ?? crypto.randomUUID();
    const createdAt = init.createdAt ?? new Date().toISOString();
    const role = init.role === undefined ? "admin" : init.role;
    const state: UniverseState = {
      universe: {
        id,
        lightspeedUniverseId: init.lightspeedUniverseId ?? crypto.randomUUID(),
        name: init.name,
        slug: init.slug,
        gatewayUrl: null,
        status: "active",
        createdAt,
        role,
      },
      members: [],
      apiKeys: [],
      profiles: new Map(),
      sessions: new Map(),
      workspaces: new Map(),
      environments: new Map(),
      registrationKeys: [],
      providerBindings: [],
      environmentTemplates: [],
      environmentCredentials: [],
      mcpServers: new Map(),
      oauthFlows: new Map(),
      secrets: { providers: [], grants: [] },
      githubApps: [],
      models: { models: [], providers: [] },
      setups: [],
      bots: new Map(),
      channelAccounts: new Map(),
      channelPairings: [],
      responder: init.responder ?? fallbackResponder,
    };
    if (role) {
      state.members.push({
        id: this.nextId("member"),
        userId: this.currentUser.id,
        role,
        email: this.currentUser.email,
        name: this.currentUser.name,
        createdAt,
      });
    }
    this.universes.set(id, state);
    return state;
  }

  /// Engine-side inventory for every platform universe (admin reconcile).
  engineView(state: UniverseState): EngineUniverse {
    let lastActivityAtMs: number | null = null;
    let blobBytes = 0;
    for (const session of state.sessions.values()) {
      lastActivityAtMs = Math.max(lastActivityAtMs ?? 0, session.view.updatedAtMs);
      blobBytes += session.events.length * 640;
    }
    for (const workspace of state.workspaces.values()) blobBytes += workspace.row.bytes;
    return {
      universeId: state.universe.lightspeedUniverseId,
      sessions: state.sessions.size,
      workspaces: state.workspaces.size,
      profiles: state.profiles.size,
      blobBytes,
      createdAtMs: Date.parse(state.universe.createdAt),
      lastActivityAtMs,
    };
  }
}

export function sessionSummary(record: SessionRecord): SessionSummary {
  const view = record.view;
  return {
    id: view.id,
    displayName: view.displayName ?? null,
    metadata: view.metadata ?? {},
    createdAtMs: view.createdAtMs,
    updatedAtMs: view.updatedAtMs,
    closedAtMs: view.closedAtMs ?? null,
    lifecycleStatus: view.status === "closed" ? "closed" : "open",
    retention: view.retention,
    managed: view.managed,
    origin: view.origin ?? null,
    access: view.access,
  };
}

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

export function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

export function textToBase64(text: string): string {
  return bytesToBase64(new TextEncoder().encode(text));
}

export function base64ToText(base64: string): string {
  return new TextDecoder().decode(base64ToBytes(base64));
}

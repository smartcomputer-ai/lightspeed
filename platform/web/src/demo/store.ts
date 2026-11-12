/// In-memory state behind the browser demo. Fixtures fill it at boot, the
/// stub routes read and mutate it, and nothing survives a reload.
import { applyEvents, emptyTranscript } from "@/lib/sessions/transcript";
import { effectiveFeatures, type FeatureOverrides, type MessageAttachment, type UniverseRole, type UniverseIconName, type UniverseIconColor } from "@lightspeed-ai/platform-shared";
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
  ModelDefaults,
  ProfileDocument,
  SecretsInventory,
  SessionSummary,
  SessionView,
  Universe,
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
  RunFailureKindView,
  RunView,
  SessionEventView,
  SessionSummaryView,
  ToolCallDisplayView,
} from "@lightspeed-ai/sdk";

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
  /// How long the model generates before the turn lands (its tool calls,
  /// or its reply when it calls none); by default a second or so.
  thinkMs?: number;
  /// Runs once the turn's reply has landed (debug commands change state here).
  landed?: (at: number) => void;
  /// Parks the run on an MCP tool call that needs approval; the decision
  /// picks the turn that follows.
  approval?: {
    serverId: string;
    serverLabel: string;
    toolName: string;
    arguments: Record<string, unknown>;
    then: (approved: boolean) => DemoTurn;
  };
  /// Fails the run after this turn instead of completing it.
  fail?: { kind: RunFailureKindView; message: string };
  /// Runs once the run has finished, whichever way.
  finished?: (run: RunView) => void;
}

/// What a debug message (`debug: …`) runs instead of the scripted reply:
/// a turn, or null for an ordinary message. Installed by the demo entry in
/// debug mode only.
export type DemoDebugHandler = (input: string, context: ResponderContext & { run: RunView }) => DemoTurn | null;

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
  steering: Array<{ text: string; steeringId: string; origin?: string; media?: MessageAttachment[] }>;
  timers: Set<ReturnType<typeof setTimeout>>;
  /// Long-poll wakers, notified on every appended event.
  waiters: Set<() => void>;
  /// User turns so far (drives scripted responders).
  turns: number;
  responder?: DemoResponder;
  /// Runs parked on an approval, each with what deciding it resumes.
  parked?: Map<string, (approved: boolean) => void>;
  /// The active run's next timed step: when it lands and how to move it
  /// sooner (see hurryRun).
  pendingStep?: { timer: ReturnType<typeof setTimeout>; dueAtMs: number; rearm: (delayMs: number) => void };
  /// Every step of the active run lands by then (see hurryRun).
  hurryUntilMs?: number;
  /// Called when someone opens the session (reads it); scripted sessions
  /// use it to react to being looked at.
  onOpen?: () => void;
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
  /// The feature switches set away from their default, as the Platform
  /// stores them; `universe.features` is what they add up to.
  featureOverrides: FeatureOverrides;
  members: Member[];
  /// Universe keys as core lists them.
  apiKeys: DeploymentApiKeyView[];
  profiles: Map<string, ProfileDocument>;
  sessions: Map<string, SessionRecord>;
  /// Called once, the first time someone lists the universe's sessions.
  onSessionsViewed?: () => void;
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
  modelDefaults: ModelDefaults;
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
  icon?: UniverseIconName;
  iconColor?: UniverseIconColor;
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
  readonly auditEvents: Array<{ id: string; createdAt: string; actorId: string; targetId: string; universeId: string; action: string; outcome: string; details: Record<string, unknown> }> = [];
  readonly deletedSessions = new Map<string, { record: SessionRecord; deletedAtMs: number }>();
  readonly users = new Map<string, DemoUser>();
  currentUser: DemoUser;
  readonly universes = new Map<string, UniverseState>();
  /// Engine universes no platform row links to (admin reconcile view).
  readonly orphanEngineUniverses: EngineUniverse[] = [];
  readonly environmentProviders = new Map<string, DeploymentEnvironmentProviderView>();
  /// Deployment keys; universe keys live on their universe.
  readonly deploymentKeys: DeploymentApiKeyView[] = [];
  channelsStatus: ChannelsStatus = { connectors: [] };
  readonly blobs = new Map<string, BlobContent>();
  readonly defaultInstructionsRef: string;
  /// Debug commands (`debug: …` messages); see debug-commands.ts.
  debug?: DemoDebugHandler;
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

  /// Seeded text is stored under a digest-shaped reference, so blob pages
  /// address it the way they address real content. Uploads compute a real
  /// SHA-256 instead; a counter padded to 64 hex digits never collides with one.
  putBytes(bytes: Uint8Array): string {
    const blobRef = `sha256:${this.nextId("blob").slice("blob-".length).padStart(64, "0")}`;
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
        icon: init.icon ?? "orbit",
        iconColor: init.iconColor ?? "default",
        slug: init.slug,
        gatewayUrl: null,
        status: "active",
        createdAt,
        role,
        features: effectiveFeatures({}),
      },
      featureOverrides: {},
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
      modelDefaults: { revision: 0, agentRun: null, speechToText: null },
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
      slug: state.universe.slug,
      sessions: state.sessions.size,
      workspaces: state.workspaces.size,
      profiles: state.profiles.size,
      blobBytes,
      createdAtMs: Date.parse(state.universe.createdAt),
      lastActivityAtMs,
    };
  }
}

/// A key scoped to `state`'s universe, as core lists it.
export function universeApiKey(
  state: UniverseState,
  key: { keyPrefix: string; displayName: string; groups: MethodGroup[]; createdAtMs: number; createdBy: string; lastUsedAtMs?: number | null; revokedAtMs?: number | null },
): DeploymentApiKeyView {
  return {
    keyPrefix: key.keyPrefix,
    displayName: key.displayName,
    scope: { kind: "universe", universeId: state.universe.lightspeedUniverseId },
    groups: key.groups,
    assertActor: false,
    createdBy: { kind: "actor", id: key.createdBy },
    createdAtMs: key.createdAtMs,
    lastUsedAtMs: key.lastUsedAtMs ?? null,
    revokedAtMs: key.revokedAtMs ?? null,
  };
}

export function sessionSummary(record: SessionRecord): SessionSummary {
  const view = record.view;
  const lastMessagePreview = lastMessagePreviewOf(record.events);
  return {
    id: view.id,
    displayName: view.displayName ?? null,
    metadata: view.metadata ?? {},
    createdAtMs: view.createdAtMs,
    updatedAtMs: view.updatedAtMs,
    closedAtMs: view.closedAtMs ?? null,
    lifecycleStatus: view.lifecycleStatus === "closed" ? "closed" : "open",
    retention: view.retention,
    lifecycleControlled: view.lifecycleControlled,
    origin: view.origin ?? null,
    access: view.access,
    activity: view.activity,
    ...(lastMessagePreview ? { lastMessagePreview } : {}),
    ...(view.activeEnvironmentId ? { activeEnvironmentId: view.activeEnvironmentId } : {}),
  };
}

const PREVIEW_LIMIT = 200;

/// The latest user or agent message of a session as one line of plain text:
/// what the session list shows under each title. Read from the same
/// transcript the session page draws, so it matches the last bubble.
export function lastMessagePreviewOf(events: SessionRecord["events"]): string | undefined {
  const entries = applyEvents(emptyTranscript(), events).entries;
  for (let index = entries.length - 1; index >= 0; index--) {
    const entry = entries[index]!;
    if (entry.kind !== "message") continue;
    const text = plainLine(entry.text);
    if (text) return text.length > PREVIEW_LIMIT ? `${text.slice(0, PREVIEW_LIMIT - 1).trimEnd()}…` : text;
  }
  return undefined;
}

/// Markdown reduced to the words: code blocks dropped, links to their text,
/// emphasis, headings, quotes and list markers removed, lines joined.
/// Underscores stay, so snake_case names survive.
function plainLine(markdown: string): string {
  return markdown
    .replace(/```[\s\S]*?(```|$)/g, " ")
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/^\s{0,3}(#{1,6}\s+|>\s?|[-*+]\s+|\d+[.)]\s+)/gm, "")
    .replace(/(\*\*|~~|\*|`)/g, "")
    .replace(/\s+/g, " ")
    .trim();
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

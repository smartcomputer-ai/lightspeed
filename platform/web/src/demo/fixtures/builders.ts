/// Building blocks shared by the use-case fixtures. A new use-case is one
/// fixture module built from these helpers plus one `seed*` call in
/// `./index.ts`: the module holds the data (names, instructions, files,
/// transcripts, event logs), and everything a universe needs beyond data —
/// clocks, ids, tool calls as transcripts show them, records with their
/// defaults, bot event logs, bot state — lives here.
import type {
  EnvironmentProviderBinding,
  EnvironmentTemplate,
  ManagedWorkflowTool,
  McpServer,
  Member,
  ModelOption,
  ModelProviderDiscovery,
  ProfileDocument,
  SecretProvider,
  SessionOrigin,
  VfsDirEntry,
  VfsTreeEntry,
  WorkspaceTree,
} from "@/api";
import type {
  BotBreaker,
  BotCoalescePolicy,
  BotControllerSnapshot,
  BotDeliverPolicy,
  BotEventDocument,
  BotEventOutcome,
  BotEventReplyRef,
  BotEventView,
  BotRecentDeliverySnapshot,
  BotSessionSnapshot,
  BotTriggerDisabledReason,
  BotTriggerRoute,
  BotTriggerView,
  BotView,
  ChannelAccountView,
  ChannelPairingView,
  ChatActivation,
  ChatAccess,
  ChatScope,
  ModelConfig,
  PollCursorSpec,
  PollCursorState,
  PollSource,
  SessionSummaryView,
  ToolCallDisplayView,
  WebhookPreset,
  WebhookVerification,
} from "@lightspeed-ai/agent-client";
import { DEFAULT_MODEL, newSession } from "../engine";
import type { DemoStore, DemoToolCall, SessionRecord, UniverseState } from "../store";
import { INCUS_PROVIDER_ID } from "./platform";

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

export const MINUTE_MS = 60_000;
export const HOUR_MS = 60 * MINUTE_MS;
export const DAY_MS = 24 * HOUR_MS;

/// Boot time; every fixture timestamp hangs off it so the demo always looks
/// lived-in today.
export const NOW = Date.now();
export const ago = (ms: number): number => NOW - ms;
export const atIso = (ms: number): string => new Date(ms).toISOString();
export const agoIso = (ms: number): string => atIso(NOW - ms);

/// A moment `daysAgo` days back at `hh:mm` local time, so transcripts read
/// like a working day. Today's moments use `ago` so they never land in the
/// future.
export function at(daysAgo: number, hh: number, mm: number): number {
  const date = new Date(NOW - daysAgo * DAY_MS);
  date.setHours(hh, mm, 0, 0);
  return date.getTime();
}

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

/// `HH:MM` local time.
export function clockLabel(ms: number): string {
  const date = new Date(ms);
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/// `HH:MM` today, `Mon D HH:MM` otherwise.
function compactTime(ms: number): string {
  const date = new Date(ms);
  const today = date.toDateString() === new Date(NOW).toDateString();
  const time = clockLabel(ms);
  return today ? time : `${date.toLocaleDateString("en-US", { month: "short", day: "numeric" })} ${time}`;
}

// ---------------------------------------------------------------------------
// Ids
// ---------------------------------------------------------------------------

/// Deterministic pseudo-digest so refs and ids look real and stay stable
/// across reloads (FNV-1a, folded; nothing cryptographic).
export function hex(seed: string, length = 16): string {
  let out = "";
  for (let round = 0; out.length < length; round++) {
    let h = (0x811c9dc5 ^ round) >>> 0;
    for (let i = 0; i < seed.length; i++) {
      h ^= seed.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    out += h.toString(16).padStart(8, "0");
  }
  return out.slice(0, length);
}

/// GitHub-delivery-shaped id.
export function uuidLike(seed: string): string {
  const h = hex(seed, 32);
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-4${h.slice(13, 16)}-8${h.slice(17, 20)}-${h.slice(20, 32)}`;
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

export const OPUS: ModelConfig = DEFAULT_MODEL;
export const SONNET: ModelConfig = { providerId: "anthropic", apiKind: "anthropic:messages", model: "claude-sonnet-5" };
export const GPT: ModelConfig = { providerId: "openai", apiKind: "openai:responses", model: "gpt-5.4" };

// ---------------------------------------------------------------------------
// Tool calls as the transcript shows them
// ---------------------------------------------------------------------------

/// The first line of a brief or message, cut like the projection does.
export function firstLine(text: string, max = 80): string {
  const line = text.split("\n").map((line) => line.trim()).find((line) => line.length > 0) ?? "";
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

export function tool(
  toolId: string,
  name: string,
  args: Record<string, unknown>,
  display: ToolCallDisplayView,
  output: string,
  isError = false,
): DemoToolCall {
  return { toolId, name, arguments: args, display, output, ...(isError ? { isError: true } : {}) };
}

export interface RunCommandOptions {
  detail?: string;
  cwd?: string;
  timeoutMs?: number;
  isError?: boolean;
}

/// `exec_command` against the active environment.
export function runCommand(argv: string[], output: string, options: RunCommandOptions = {}): DemoToolCall {
  const { detail, cwd, timeoutMs, isError = false } = options;
  return tool(
    "env.run_process",
    "exec_command",
    { argv, ...(cwd === undefined ? {} : { cwd }), ...(timeoutMs === undefined ? {} : { timeoutMs }) },
    { group: "execute", verb: "Run", target: argv.join(" "), ...(detail === undefined ? {} : { detail }) },
    output,
    isError,
  );
}

/// `read_file` against the active environment.
export function readFile(path: string, output: string): DemoToolCall {
  return tool("env.read_file", "read_file", { path }, { group: "explore", verb: "Read", target: path }, output);
}

/// `vfs_read_file` against a linked workspace.
export function vfsReadFile(path: string, output: string): DemoToolCall {
  return tool("vfs.read_file", "vfs_read_file", { path }, { group: "explore", verb: "Read", target: path }, output);
}

export function writeFile(path: string, content: string, detail: string): DemoToolCall {
  return tool(
    "env.write_file",
    "write_file",
    { path, content },
    { group: "edit", verb: "Write", target: path, detail },
    `wrote ${new TextEncoder().encode(content).length} bytes to ${path}`,
  );
}

/// `vfs_write_file` into a writable workspace link.
export function vfsWriteFile(path: string, content: string, detail: string): DemoToolCall {
  return tool(
    "vfs.write_file",
    "vfs_write_file",
    { path, content },
    { group: "edit", verb: "Write", target: path, detail },
    `wrote ${new TextEncoder().encode(content).length} bytes to ${path}`,
  );
}

export function webFetch(url: string, detail: string, output: string, isError = false): DemoToolCall {
  return tool("web.fetch", "web_fetch", { url }, { group: "explore", verb: "Fetch", target: url, detail }, output, isError);
}

/// A remote MCP tool named `server.tool`; the server is the verb, the tool
/// the target, as the projection renders both MCP paths. Object outputs are
/// shown pretty-printed.
export function mcpCall(name: string, args: Record<string, unknown>, output: unknown, isError = false): DemoToolCall {
  return tool(
    name,
    name,
    args,
    mcpDisplay(name, args),
    typeof output === "string" ? output : JSON.stringify(output, null, 2),
    isError,
  );
}

export function mcpDisplay(name: string, args: Record<string, unknown>, detail?: string): ToolCallDisplayView {
  const dot = name.indexOf(".");
  const server = dot === -1 ? "MCP" : name.slice(0, dot);
  const toolName = dot === -1 ? name : name.slice(dot + 1);
  const hint = detail ?? Object.values(args).find((value): value is string | number =>
    (typeof value === "string" && value.trim().length > 0) || typeof value === "number");
  return { group: "mcp", verb: server, target: toolName, ...(hint === undefined ? {} : { detail: String(hint) }) };
}

/// A GitHub MCP tool.
export function github(name: string, args: Record<string, unknown>, detail: string, output: string): DemoToolCall {
  return tool(`github.${name}`, `github.${name}`, args, mcpDisplay(`github.${name}`, args, detail), output);
}

/// `agent_run`: a joined sub-agent delegation.
export function agentRun(profileId: string, task: string, output: string): DemoToolCall {
  return tool(
    "subagent.run",
    "agent_run",
    { agent: profileId, input: task },
    { group: "agent", verb: "Delegate", target: profileId, detail: firstLine(task) },
    output,
  );
}

/// `agent_spawn`: a sub-agent started for a promise the run joins later.
export function agentSpawn(profileId: string, task: string, promiseId: string): DemoToolCall {
  return tool(
    "subagent.spawn",
    "agent_spawn",
    { agent: profileId, input: task },
    { group: "agent", verb: "Spawn", target: profileId, detail: firstLine(task) },
    JSON.stringify({ promise: promiseId, agent: profileId }),
  );
}

/// `await`: parks the run on promises; the output is each child's result.
export function awaitPromises(
  promises: string[],
  results: Array<{ agent: string; sessionId: string; output: string }>,
): DemoToolCall {
  return tool(
    "concurrency.await",
    "await",
    { promises, mode: "all" },
    { group: "agent", verb: "Await", target: promises.join(", "), ...(promises.length > 1 ? { detail: `${promises.length} promises` } : {}) },
    JSON.stringify(Object.fromEntries(promises.map((id, i) => [id, { status: "completed", ...results[i] }])), null, 2),
  );
}

export type BotEmitArgs = {
  to: string;
  kind: string;
  summary: string;
  data?: unknown;
  reply?: boolean;
};

/// `bot_emit` to another bot's inbox; `seq` is the receiver's #N for it.
export function botEmit(args: BotEmitArgs, seq: number | null): DemoToolCall {
  return tool(
    "bot_emit",
    "bot_emit",
    { ...args },
    { group: "bot", verb: "Emit", target: `to ${args.to}`, detail: args.reply ? `${args.kind} · reply requested` : args.kind },
    JSON.stringify({ to: args.to, seq }),
  );
}

export function briefPut(brief: string): DemoToolCall {
  return tool(
    "bot_brief_put",
    "bot_brief_put",
    { brief },
    { group: "bot", verb: "Update brief", detail: firstLine(brief) },
    JSON.stringify({ brief, appliesAt: "next idle boundary" }),
  );
}

/// `message_send` in a chat conversation; `sent` is the bot's #N for the
/// archived send. A push (a brief, a reminder) replies to nothing.
export function messageSend(
  conversation: { label: string },
  text: string,
  replyTo: number | null,
  sent: number,
): DemoToolCall {
  return tool(
    "message_send",
    "message_send",
    { text, ...(replyTo === null ? {} : { replyTo }) },
    { group: "message", verb: "Send", target: firstLine(text), detail: replyTo === null ? conversation.label : `${conversation.label} · reply to #${replyTo}` },
    JSON.stringify({ sent }),
  );
}

export function messageNoop(conversation: { label: string }, reason: string): DemoToolCall {
  return tool(
    "message_noop",
    "message_noop",
    { reason },
    { group: "message", verb: "Skip", target: conversation.label, detail: reason },
    JSON.stringify({ accepted: true }),
  );
}

// ---------------------------------------------------------------------------
// Universe records
// ---------------------------------------------------------------------------

export interface ProfileInit {
  profileId: string;
  displayName: string;
  description: string;
  instructions: string;
  config: Record<string, unknown>;
  metadata?: Record<string, string>;
  environment?: ProfileDocument["environment"];
  revision: number;
  createdAtMs: number;
  updatedAtMs: number;
}

export function profile(init: ProfileInit): ProfileDocument {
  return {
    profileId: init.profileId,
    displayName: init.displayName,
    description: init.description,
    instructions: { type: "text", text: init.instructions },
    config: structuredClone(init.config),
    ...(init.metadata === undefined ? {} : { metadata: structuredClone(init.metadata) }),
    ...(init.environment === undefined ? {} : { environment: init.environment }),
    revision: init.revision,
    createdAtMs: init.createdAtMs,
    updatedAtMs: init.updatedAtMs,
  };
}

/// Membership of a platform user; the demo admin joins through `addUniverse`.
export function member(
  store: DemoStore,
  universe: UniverseState,
  userId: string,
  role: string,
  joinedAtMs: number,
): Member {
  const user = store.users.get(userId);
  return {
    id: store.nextId("member"),
    userId,
    role,
    email: user?.email ?? `${userId}@${universe.universe.slug}.example`,
    name: user?.name ?? userId,
    createdAt: atIso(joinedAtMs),
  };
}

const MEDIA_TYPES: Record<string, string> = {
  md: "text/markdown",
  json: "application/json",
  ts: "text/typescript",
};

export interface WorkspaceInit {
  id: string;
  displayName: string;
  /// Path → text; nested directories come from the slashes.
  files: Record<string, string>;
  revision: number;
  createdAtMs: number;
  updatedAtMs: number;
}

/// Builds a nested manifest from flat paths; file bytes go to the blob store
/// so the workspace browser can open them.
export function workspace(store: DemoStore, universe: UniverseState, init: WorkspaceInit): void {
  const root: Record<string, VfsTreeEntry> = {};
  let files = 0;
  let bytes = 0;
  for (const [path, text] of Object.entries(init.files)) {
    const size = new TextEncoder().encode(text).length;
    const parts = path.split("/");
    const name = parts.pop() ?? path;
    let dir = root;
    for (const part of parts) {
      const existing = dir[part];
      if (existing?.kind === "directory") {
        dir = existing.entries;
        continue;
      }
      const created: VfsDirEntry = { kind: "directory", entries: {} };
      dir[part] = created;
      dir = created.entries;
    }
    dir[name] = {
      kind: "file",
      blob_ref: store.putText(text),
      size_bytes: size,
      media_type: MEDIA_TYPES[name.split(".").pop() ?? ""] ?? "text/plain",
      executable: false,
    };
    files += 1;
    bytes += size;
  }
  const manifest: WorkspaceTree["manifest"] = {
    schema_version: "lightspeed.vfs.snapshot.v1",
    root: { entries: root },
    totals: { files, bytes },
  };
  universe.workspaces.set(init.id, {
    row: {
      workspaceId: init.id,
      displayName: init.displayName,
      headSnapshotRef: `snap-${hex(`${init.id}:${init.revision}`, 12)}`,
      revision: init.revision,
      files,
      bytes,
      createdAtMs: init.createdAtMs,
      updatedAtMs: init.updatedAtMs,
    },
    manifest,
  });
}

export interface ProviderBindingInit {
  /// Defaults to the platform's Incus provider.
  providerId?: string;
  revision: number;
  metadata?: Record<string, string>;
  createdAtMs: number;
  updatedAtMs: number;
}

/// The universe's enabled binding to an operator-registered provider.
export function providerBinding(init: ProviderBindingInit): EnvironmentProviderBinding {
  const providerId = init.providerId ?? INCUS_PROVIDER_ID;
  return {
    bindingId: providerId,
    providerId,
    status: "enabled",
    revision: init.revision,
    ...(init.metadata === undefined ? {} : { metadata: init.metadata }),
    createdAtMs: init.createdAtMs,
    updatedAtMs: init.updatedAtMs,
  };
}

export interface TemplateInit {
  /// Defaults to the platform's Incus provider.
  providerId?: string;
  templateId: string;
  displayName: string;
  description: string;
  publicIngress: boolean;
  deprecated: boolean;
  metadata: Record<string, string>;
}

export function template(init: TemplateInit): EnvironmentTemplate {
  const providerId = init.providerId ?? INCUS_PROVIDER_ID;
  return {
    bindingId: providerId,
    providerId,
    templateId: init.templateId,
    displayName: init.displayName,
    description: init.description,
    publicIngress: init.publicIngress,
    deprecated: init.deprecated,
    metadata: init.metadata,
  };
}

export type McpServerInit = Partial<McpServer> &
  Pick<McpServer, "serverId" | "serverUrl" | "authPolicy" | "status" | "createdAtMs" | "updatedAtMs">;

export function mcpServer(init: McpServerInit): McpServer {
  return {
    displayName: null,
    defaultServerLabel: init.serverId,
    description: null,
    allowedTools: null,
    execution: "provider",
    exposure: "inject",
    approvalDefault: "never",
    deferLoadingDefault: null,
    allowPrivateNetwork: false,
    credential: null,
    revision: 1,
    ...init,
  };
}

/// A `model:<providerId>` credential row.
export function modelProvider(
  providerId: string,
  displayName: string,
  config: SecretProvider["config"],
  hasCredential: boolean,
  createdAtMs: number,
  updatedAtMs: number,
): SecretProvider {
  const providerKind: SecretProvider["providerKind"] =
    config.type === "modelApiKey" ? "modelApiKey" : config.type === "modelEndpoint" ? "modelEndpoint" : "modelOAuth";
  return {
    providerId,
    credentialId: `model:${providerId}`,
    usableForModels: true,
    providerKind,
    displayName,
    config,
    hasCredential,
    status: "active",
    createdAtMs,
    updatedAtMs,
  };
}

/// One discovered model, as `models/list` reports it.
export function modelOption(
  config: ModelConfig,
  displayName: string,
  capabilities: ModelOption["capabilities"],
  fetchedAtMs: number,
): ModelOption {
  return { ...config, displayName, capabilities, source: "provider", fetchedAtMs };
}

export function modelDiscovery(
  providerId: string,
  apiKinds: string[],
  credential: ModelProviderDiscovery["credential"],
  credentialSource: ModelProviderDiscovery["credentialSource"],
  fetchedAtMs: number,
): ModelProviderDiscovery {
  return { providerId, apiKinds, fetchedAtMs, error: null, credential, credentialSource };
}

// ---------------------------------------------------------------------------
// Bots
// ---------------------------------------------------------------------------

/// The workflow tools every bot session carries.
export const BOT_TOOLS: ManagedWorkflowTool[] = [
  { toolId: "bots.event.read", name: "bot_event_read", semanticType: "bots.event.read.v1", target: "bound", completion: "accepted" },
  { toolId: "bots.event.list", name: "bot_event_list", semanticType: "bots.event.list.v1", target: "bound", completion: "accepted" },
  { toolId: "bots.brief.put", name: "bot_brief_put", semanticType: "bots.brief.put.v1", target: "bound", completion: "accepted" },
];
/// Added when the bot may emit.
export const EMIT_TOOL: ManagedWorkflowTool = {
  toolId: "bots.emit",
  name: "bot_emit",
  semanticType: "bots.emit.v1",
  target: "bound",
  completion: "accepted",
};
/// Added when the bot may change its own triggers (`selfConfig`).
export const SELF_CONFIG_TOOLS: ManagedWorkflowTool[] = [
  { toolId: "bots.trigger.put", name: "bot_trigger_put", semanticType: "bots.trigger.put.v1", target: "bound", completion: "accepted" },
  { toolId: "bots.trigger.delete", name: "bot_trigger_delete", semanticType: "bots.trigger.delete.v1", target: "bound", completion: "accepted" },
];
/// Carried by chat-trigger events into their conversation sessions.
export const MESSAGE_TOOLS: ManagedWorkflowTool[] = [
  { toolId: "channels.message.send", name: "message_send", semanticType: "channels.message.send.v1", target: "bound", completion: "accepted" },
  { toolId: "channels.message.edit", name: "message_edit", semanticType: "channels.message.edit.v1", target: "bound", completion: "accepted" },
  { toolId: "channels.message.react", name: "message_react", semanticType: "channels.message.react.v1", target: "bound", completion: "accepted" },
  { toolId: "channels.message.noop", name: "message_noop", semanticType: "channels.message.noop.v1", target: "bound", completion: "accepted" },
];

/// The first thing a freshly created bot is asked.
export const INTRODUCTION_PROMPT =
  "You were just created. Introduce yourself in two sentences and confirm your setup: the triggers that wake you, the tools and environment you can use. Ask about anything that is unclear or missing.";

export interface BotInit {
  botId: string;
  displayName: string;
  description: string;
  profileId: string;
  brief: string;
  runsPerDay: number | null;
  breaker: BotBreaker | null;
  routedSessionCloseAfterMs?: number | null;
  selfConfig?: boolean;
  emit: boolean;
  revision?: number;
  createdAtMs: number;
  updatedAtMs: number;
}

/// A bot record in the core wire shape. `eventSeq` is recomputed from the
/// event log by the routes, so the seed value never has to be maintained.
export function bot(universe: UniverseState, init: BotInit): BotView {
  void universe;
  return {
    botId: init.botId,
    displayName: init.displayName,
    description: init.description,
    profileId: init.profileId,
    brief: init.brief,
    runsPerDay: init.runsPerDay,
    breaker: init.breaker,
    routedSessionCloseAfterMs: init.routedSessionCloseAfterMs ?? null,
    selfConfig: init.selfConfig ?? false,
    emit: init.emit,
    enabled: true,
    eventSeq: 0,
    revision: init.revision ?? 1,
    createdAtMs: init.createdAtMs,
    updatedAtMs: init.updatedAtMs,
  };
}

/// Everything a trigger may carry beyond what its kind fixes.
export interface TriggerRest {
  filter?: string | null;
  route?: BotTriggerRoute | null;
  coalesce?: BotCoalescePolicy | null;
  deliver?: BotDeliverPolicy | null;
  sessionCloseAfterMs?: number | null;
  enabled?: boolean;
  disabledReason?: BotTriggerDisabledReason | null;
  disabledAtMs?: number | null;
  /// Poll triggers: the advancing runtime cursor state (`cursorState`).
  cursorState?: Partial<PollCursorState>;
  revision?: number;
  createdAtMs: number;
  updatedAtMs?: number;
}

function baseTrigger(botId: string, triggerId: string, rest: TriggerRest) {
  return {
    botId,
    triggerId,
    revision: rest.revision ?? 1,
    filter: rest.filter ?? null,
    route: rest.route ?? null,
    coalesce: rest.coalesce ?? null,
    deliver: rest.deliver ?? null,
    sessionCloseAfterMs: rest.sessionCloseAfterMs ?? null,
    enabled: rest.enabled ?? true,
    disabledReason: rest.disabledReason ?? null,
    disabledAtMs: rest.disabledAtMs ?? null,
    lastFilterError: null,
    lastFilterErrorAtMs: null,
    createdAtMs: rest.createdAtMs,
    updatedAtMs: rest.updatedAtMs ?? rest.createdAtMs,
  };
}

export interface WebhookSpecInit {
  token: string;
  verification?: WebhookVerification;
  preset?: WebhookPreset | null;
}

/// A webhook trigger with its capability-URL ingest path in the core shape:
/// `/hooks/bots/{universeId}/{botId}/{triggerId}/{token}`.
export function webhookTrigger(
  universe: UniverseState,
  botId: string,
  triggerId: string,
  spec: WebhookSpecInit,
  rest: TriggerRest,
): BotTriggerView {
  return {
    ...baseTrigger(botId, triggerId, rest),
    kind: "webhook",
    verification: spec.verification ?? { scheme: "token" },
    preset: spec.preset ?? null,
    ingestPath: `/hooks/bots/${universe.universe.lightspeedUniverseId}/${botId}/${triggerId}/${spec.token}`,
  } as BotTriggerView;
}

export function scheduleTrigger(
  botId: string,
  triggerId: string,
  spec: { cron: string; summary: string; timezone?: string },
  rest: TriggerRest,
): BotTriggerView {
  return {
    ...baseTrigger(botId, triggerId, rest),
    kind: "schedule",
    cron: spec.cron,
    atMs: null,
    timezone: spec.timezone ?? "Europe/Berlin",
    summary: spec.summary,
  } as BotTriggerView;
}

export interface PollSpecInit {
  source: PollSource;
  intervalMs: number;
  items?: string | null;
  cursor: PollCursorSpec;
}

export function pollTrigger(botId: string, triggerId: string, spec: PollSpecInit, rest: TriggerRest): BotTriggerView {
  return {
    ...baseTrigger(botId, triggerId, rest),
    kind: "poll",
    source: spec.source,
    intervalMs: spec.intervalMs,
    items: spec.items ?? null,
    cursor: spec.cursor,
    cursorState: rest.cursorState ? { consecutiveFailures: 0, ...rest.cursorState } : null,
  } as BotTriggerView;
}

/// The bot's single inbox; `from` undefined accepts every bot in the universe.
export function inboxTrigger(botId: string, from: string[] | undefined, rest: TriggerRest): BotTriggerView {
  return {
    ...baseTrigger(botId, "inbox", rest),
    kind: "bot",
    from: from ?? null,
  } as BotTriggerView;
}

export interface ChatSpecInit {
  /// The universe's channel account this connection serves.
  accountId: string;
  matchScope?: ChatScope | null;
  activation?: ChatActivation | null;
  access?: ChatAccess | null;
  /// A code makes the connection `pairing: "code"`; null opens it.
  pairingCode?: string | null;
  priority?: number;
}

/// A chat connection: one session per conversation, kept forever.
export function chatTrigger(botId: string, triggerId: string, spec: ChatSpecInit, rest: TriggerRest): BotTriggerView {
  return {
    ...baseTrigger(botId, triggerId, { route: { policy: "perKey", key: null }, sessionCloseAfterMs: 0, ...rest }),
    kind: "chat",
    accountId: spec.accountId,
    matchScope: spec.matchScope ?? null,
    ...(spec.activation ? { activation: spec.activation } : {}),
    ...(spec.access ? { access: spec.access } : {}),
    pairing: spec.pairingCode ? "code" : "open",
    priority: spec.priority ?? 100,
    pairingCode: spec.pairingCode ?? null,
  } as BotTriggerView;
}

export interface ManagedInit {
  id: string;
  botId: string;
  displayName: string;
  profile: Pick<ProfileInit, "config" | "instructions" | "metadata">;
  tools: ManagedWorkflowTool[];
  createdAtMs: number;
  environmentId?: string;
}

/// A bot-owned session: the controller is its lifecycle owner and `tools`
/// are its caller-declared workflow tools.
export function managedSession(store: DemoStore, universe: UniverseState, init: ManagedInit): SessionRecord {
  return newSession(store, universe, {
    id: init.id,
    displayName: init.displayName,
    managed: true,
    management: {
      version: 1,
      lifecycleController: { workflowId: `bot:v1:${init.botId}`, workflowKind: "bot_controller_v1" },
      tools: init.tools,
    },
    config: structuredClone(init.profile.config),
    metadata: structuredClone(init.profile.metadata ?? {}),
    instructions: init.profile.instructions,
    activeEnvironmentId: init.environmentId ?? null,
    createdAtMs: init.createdAtMs,
  });
}

export interface RenderedEvent {
  seq: number;
  kind: string;
  source: string;
  at: number;
  summary: string;
  /// Extra lines after the summary (a payload projection).
  body?: string[];
  inReplyTo?: { bot: string; seq: number } | null;
}

/// The model-facing text of one delivered event, as the bot's session sees
/// it: a header the model can quote back by #N, then the summary.
export function renderEvent(input: RenderedEvent): string {
  const lines = [
    `── event #${input.seq} · ${input.kind} · ${input.source} · ${compactTime(input.at)}`,
    input.summary,
    ...(input.body ?? []),
  ];
  if (input.inReplyTo) lines.push(`reply to your #${input.inReplyTo.seq} at ${input.inReplyTo.bot}`);
  return lines.join("\n");
}

export interface EventInit {
  kind: string;
  /// Document source string (`webhook:github`, `bot:planner`, `manual`, …).
  source: string;
  /// Admitting trigger; derived from `source` when absent.
  triggerId?: string | null;
  /// When it happened; received 900 ms later.
  at: number;
  summary: string;
  body?: string[];
  data?: unknown;
  eventId?: string;
  session?: { sessionId: string; label: string } | null;
  sender?: string;
  hops?: number;
  inReplyTo?: BotEventReplyRef;
  outcome: BotEventOutcome | null;
  detail?: string;
  resolvedAfterMs?: number;
}

export interface ScriptedEvent {
  envelope: BotEventView;
  /// The event as the session read it.
  prompt: string;
}

export interface EventLog {
  botId: string;
  /// Newest last; `seq` is the position in the log.
  events: BotEventView[];
  add(init: EventInit): ScriptedEvent;
}

/// The bot each seeded event belongs to, so `recent` can name the main
/// session without carrying non-wire fields on the view itself.
const eventOwners = new WeakMap<BotEventView, string>();

/// The admitting trigger, read off the document source the fixtures name:
/// `webhook:x`/`poll:x`/`schedule:x` → `x`, a bot sender → the inbox, a
/// chat provider source → the provider-named chat trigger.
function triggerIdFromSource(source: string): string | null {
  const prefixed = /^(?:webhook|poll|schedule|chat):(.+)$/.exec(source);
  if (prefixed?.[1]) return prefixed[1];
  if (source.startsWith("bot:")) return "inbox";
  const provider = /^(telegram|whatsapp):/.exec(source);
  if (provider?.[1]) return provider[1];
  return null;
}

/// A bot's numbered event log. `add` stores the prompt and the event
/// document as blobs, like admission does; `runId` is set by whoever
/// appends the run that handled it.
export function eventLog(store: DemoStore, botId: string): EventLog {
  const events: BotEventView[] = [];
  const add = (init: EventInit): ScriptedEvent => {
    const seq = events.length + 1;
    const inReplyTo = init.inReplyTo ?? null;
    const prompt = renderEvent({
      seq,
      kind: init.kind,
      source: init.source,
      at: init.at,
      summary: init.summary,
      ...(init.body === undefined ? {} : { body: init.body }),
      inReplyTo,
    });
    const document: BotEventDocument = {
      version: 1,
      kind: init.kind,
      source: init.source,
      occurredAtMs: init.at,
      summary: init.summary,
      ...(init.data === undefined ? {} : { data: init.data }),
      ...(init.sender === undefined ? {} : { sender: { bot: init.sender } }),
      ...(init.hops ? { hops: init.hops } : {}),
      ...(inReplyTo ? { inReplyTo } : {}),
    };
    const triggerId = init.triggerId !== undefined ? init.triggerId : triggerIdFromSource(init.source);
    const envelope: BotEventView = {
      seq,
      eventId: init.eventId ?? `${init.kind}:${botId}:${seq}`,
      ...(triggerId === null ? {} : { triggerId }),
      kind: init.kind,
      summary: init.summary,
      occurredAtMs: init.at,
      receivedAtMs: init.at + 900,
      promptRef: store.putText(prompt),
      documentRef: store.putText(JSON.stringify(document, null, 2)),
      session: init.session ? { sessionId: init.session.sessionId, label: init.session.label } : null,
      senderBotId: init.sender ?? null,
      hops: init.hops ?? 0,
      inReplyTo,
      outcome: init.outcome,
      outcomeDetail: init.detail ?? null,
      runId: null,
      resolvedAtMs: init.outcome === null ? null : init.at + (init.resolvedAfterMs ?? 40_000),
    };
    events.push(envelope);
    eventOwners.set(envelope, botId);
    return { envelope, prompt };
  };
  return { botId, events, add };
}

/// One chat conversation a chat trigger routes into its own session.
export interface Conversation {
  sessionId: string;
  label: string;
  provider: "telegram" | "whatsapp";
  source: string;
  chatId: string;
  scope: "direct" | "group";
}

/// An inbound chat message as the chat trigger admits it.
export function chatMessage(
  log: EventLog,
  conversation: Conversation,
  trigger: string,
  sender: string,
  text: string,
  at: number,
  outcome: BotEventOutcome | null,
  detail?: string,
): ScriptedEvent {
  const messageId = String(1_000 + log.events.length * 7 + conversation.chatId.length);
  return log.add({
    kind: "chat.message",
    source: conversation.source,
    triggerId: trigger,
    at,
    summary: `${sender} (${clockLabel(at)}): ${text}`,
    eventId: `chat:${trigger}:${conversation.chatId}:${messageId}`,
    session: conversation,
    outcome,
    ...(detail === undefined ? {} : { detail }),
    data: {
      conversation: {
        key: conversation.sessionId.slice(`bot:v1:${log.botId}:`.length),
        label: conversation.label,
        scope: conversation.scope,
        provider: conversation.provider,
        chatId: conversation.chatId,
      },
      sender: { name: sender, memberRole: "member" },
      messageId,
      text,
      isDirect: conversation.scope === "direct",
      mentionedBot: conversation.scope === "group",
    },
  });
}

/// A send the bot made, archived so the model can refer to it by #N.
export function chatSent(
  log: EventLog,
  conversation: Conversation,
  text: string,
  at: number,
  replyTo: number | null,
): ScriptedEvent {
  const line = `sent: ${text}`;
  return log.add({
    kind: "chat.sent",
    source: conversation.source,
    triggerId: conversation.provider,
    at,
    summary: line,
    eventId: `chat:${conversation.provider}:${conversation.chatId}:sent:${log.events.length + 1}`,
    session: conversation,
    outcome: "archived",
    detail: line.length > 120 ? `${line.slice(0, 119)}…` : line,
    resolvedAfterMs: 0,
    data: {
      conversation: { label: conversation.label, provider: conversation.provider, chatId: conversation.chatId },
      text,
      fromMe: true,
      replyTo,
    },
  });
}

export interface ReceiptInit {
  /// The bot that handled our event.
  from: string;
  /// Our event's #N at that bot.
  askedSeq: number;
  status: BotEventOutcome;
  /// The answering delivery's one-line summary.
  summary: string;
  at: number;
  hops: number;
  session: { sessionId: string; label: string };
  outcome: BotEventOutcome;
  detail: string;
  resolvedAfterMs?: number;
}

/// The deterministic `bot.reply` receipt a receiver's controller sends when
/// a delivery finishes: the outcome, never a model-authored message.
export function receipt(log: EventLog, init: ReceiptInit): ScriptedEvent {
  return log.add({
    kind: "bot.reply",
    source: `bot:${init.from}`,
    at: init.at,
    summary: `#${init.askedSeq} at ${init.from} finished ${init.status}: ${init.summary}`,
    eventId: `reply:${init.from}:${hex(`${log.botId}:${init.from}:${init.askedSeq}`, 12)}`,
    session: init.session,
    sender: init.from,
    hops: init.hops,
    inReplyTo: { bot: init.from, seq: init.askedSeq },
    outcome: init.outcome,
    detail: init.detail,
    resolvedAfterMs: init.resolvedAfterMs ?? 20_000,
    data: { status: init.status },
  });
}

export interface SubagentInit {
  id: string;
  displayName: string;
  /// The pinned profile: its config, instructions, and revision.
  profile: Pick<ProfileInit, "profileId" | "config" | "instructions" | "revision">;
  parent: SessionRecord;
  parentRunId: string;
  root: string;
  depth: number;
  limits: SessionOrigin["limits"];
  environmentId?: string;
  createdAtMs: number;
}

/// A sub-agent session: `origin` records who delegated it, under which
/// root, at what depth, from which pinned profile revision.
export function subagentSession(store: DemoStore, universe: UniverseState, init: SubagentInit): SessionRecord {
  const origin: SessionOrigin = {
    kind: "subagent",
    parentSessionId: init.parent.view.id,
    parentRunId: init.parentRunId,
    rootSessionId: init.root,
    depth: init.depth,
    invocationId: `inv-${hex(init.id, 10)}`,
    agent: { profileId: init.profile.profileId, revision: init.profile.revision },
    limits: init.limits,
  };
  return newSession(store, universe, {
    id: init.id,
    displayName: init.displayName,
    config: structuredClone(init.profile.config),
    instructions: init.profile.instructions,
    origin,
    activeEnvironmentId: init.environmentId ?? null,
    createdAtMs: init.createdAtMs,
  });
}

/// A session summary in the core wire shape (bot-state descendants).
export function sessionSummaryOf(session: SessionRecord): SessionSummaryView {
  const view = session.view;
  return {
    id: view.id,
    displayName: view.displayName ?? null,
    createdAtMs: view.createdAtMs,
    updatedAtMs: view.updatedAtMs,
    lifecycleStatus: view.status === "closed" ? "closed" : "open",
    retention: view.retention,
    managed: view.managed,
    origin: view.origin ?? null,
  };
}

/// One descendant as the bot state's flat list carries it; profile and
/// depth already travel on the session's own `origin`.
export function lineageChild(session: SessionRecord, profileId: string, depth: number): SessionSummaryView {
  void profileId;
  void depth;
  return sessionSummaryOf(session);
}

/// A session as the controller lists it; the label defaults to "Main" for
/// the main session and to the display name for a keyed one.
export function botSession(
  session: SessionRecord,
  kind: "main" | "keyed" | "event",
  label?: string,
): BotSessionSnapshot {
  return {
    sessionId: session.view.id,
    label: label ?? (kind === "main" ? "Main" : (session.view.displayName ?? session.view.id)),
    kind: kind === "main" ? "main" : kind === "keyed" ? "perKey" : "perEvent",
    busy: false,
    generation: 1,
    lastActiveAtMs: session.view.updatedAtMs,
  };
}

/// The delivery as the controller remembers it, so the activity page can
/// fill in cache figures beside the stored outcome.
export function recent(
  envelope: BotEventView,
  usage?: { inputTokens: number; cachedInputTokens: number },
): BotRecentDeliverySnapshot {
  const botId = eventOwners.get(envelope);
  return {
    deliveryId: `dlv-${botId ?? "bot"}-${envelope.seq}`,
    seqs: [envelope.seq],
    sessionId: envelope.session?.sessionId ?? `bot:v1:${botId ?? "bot"}`,
    finishedAtMs: envelope.resolvedAtMs ?? envelope.receivedAtMs,
    outcome: envelope.outcome ?? "unresolved",
    ...(envelope.runId ? { runId: envelope.runId } : {}),
    ...(envelope.outcomeDetail === null || envelope.outcomeDetail === undefined
      ? {}
      : { summary: envelope.outcomeDetail }),
    ...(usage === undefined ? {} : { usage }),
  };
}

export interface StateInit {
  bot: BotView;
  /// The main session first.
  sessions: BotSessionSnapshot[];
  recentEvents: BotRecentDeliverySnapshot[];
  eventsProcessed: number;
  duplicateEventCount?: number;
  appliedProfileRevision: number;
  runsToday: number;
  descendantsToday?: number;
}

/// The controller's live snapshot for an idle bot.
export function botState(init: StateInit): BotControllerSnapshot {
  return {
    mainSessionId:
      init.sessions.find((session) => session.kind === "main")?.sessionId ?? `bot:v1:${init.bot.botId}`,
    sessions: init.sessions,
    controllerStatus: "idle",
    setupStatus: "ready",
    enabled: init.bot.enabled ?? true,
    closed: false,
    activeDeliveries: [],
    buffers: [],
    pendingDeliveries: 0,
    recentDeliveries: init.recentEvents,
    eventsProcessed: init.eventsProcessed,
    duplicateEvents: init.duplicateEventCount ?? 0,
    appliedProfileRevision: init.appliedProfileRevision,
    runDay: new Date(NOW).toISOString().slice(0, 10),
    runsToday: init.runsToday,
    descendantsToday: init.descendantsToday ?? 0,
    lastError: null,
  };
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

export interface ChannelAccountInit {
  accountId: string;
  provider: string;
  /// Provider-native identity: the Telegram bot username, the WhatsApp number.
  providerAccountId: string;
  displayName: string;
  credentialGrantId?: string | null;
  settings?: ChannelAccountView["settings"];
  enabled?: boolean;
  createdAtMs: number;
  updatedAtMs: number;
}

/// A universe channel account in the core wire shape, stored on the universe.
export function channelAccount(universe: UniverseState, init: ChannelAccountInit): ChannelAccountView {
  const account: ChannelAccountView = {
    accountId: init.accountId,
    provider: init.provider,
    providerAccountId: init.providerAccountId,
    displayName: init.displayName,
    credentialGrantId: init.credentialGrantId ?? null,
    settings: init.settings ?? {},
    enabled: init.enabled ?? true,
    revision: 1,
    createdAtMs: init.createdAtMs,
    updatedAtMs: init.updatedAtMs,
  };
  universe.channelAccounts.set(account.accountId, account);
  return account;
}

/// A pairing row: the routing authority binding one conversation to a bot.
export function channelPairing(universe: UniverseState, pairing: ChannelPairingView): ChannelPairingView {
  universe.channelPairings.push(pairing);
  return pairing;
}

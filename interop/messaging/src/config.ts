import { readFile } from "node:fs/promises";
import type {
  RemoteMcpApprovalPolicy,
  SessionConfigInput,
  VfsMountAccess,
} from "@lightspeed/agent-client";
import type { GroupActivation, ReplyToMode } from "./policy.js";

export interface LightspeedBridgeConfig {
  endpoint: string;
  cwd?: string | null;
  waitMs: number;
  eventLimit: number;
  sessionPrefix: string;
}

export interface BridgeRuntimeConfig {
  /// Quiet window that batches rapid consecutive messages into one turn.
  debounceMs: number;
  turnMaxBatch: number;
  turnMaxWaitMs: number;
  /// Room-event batching toward context/append.
  roomFlushMs: number;
  roomFlushMax: number;
  /// Max room events buffered per chat between flushes; overflow is dropped.
  roomBudget: number;
}

/// One workspace/snapshot to mount into a recipe's sessions. The core
/// discovers `.lightspeed/prompts/` instructions and the skill catalog under
/// workspace mounts, so a recipe configures prompts and skills purely by what
/// it mounts; the bridge never activates skills itself.
export interface RecipeMount {
  mountPath: string;
  source: { workspaceId: string } | { snapshotRef: string };
  access: VfsMountAccess;
}

/// One MCP server to link into a recipe's sessions. The server must already be
/// created and authenticated (`mcp/servers/create`); the recipe references it
/// by id and passes the link surface through unchanged.
export interface RecipeMcpLink {
  serverId: string;
  serverLabel?: string;
  toolId?: string;
  allowedTools?: string[];
  approval?: RemoteMcpApprovalPolicy;
  authGrantId?: string;
  deferLoading?: boolean;
}

/// One execution environment to attach into a recipe's sessions. The provider
/// must already be online; the bridge binds an existing provider target.
export interface RecipeEnvironment {
  envId: string;
  providerId: string;
  targetId: string;
  activate: boolean;
}

/// A named provisioning recipe applied once when a bound session is first
/// created: start with `config` (model + tools), mount workspaces/snapshots,
/// link MCP servers, then attach/activate execution environments.
export interface SessionRecipe {
  config?: SessionConfigInput;
  mounts: RecipeMount[];
  mcp: RecipeMcpLink[];
  environments?: RecipeEnvironment[];
}

export type BindingScope = "direct" | "group";
export type BindingHandleMatch = string | string[];

export interface BindingMatch {
  /// Channel to match, or `*` for any channel.
  channel: "telegram" | "whatsapp" | "*";
  /// Sender handle(s) (telegram id/@username, whatsapp jid). Omit to match any.
  handle?: BindingHandleMatch;
  /// Channel chat id (telegram chat id, whatsapp jid). Omit to match any.
  chatId?: string;
  /// Restrict to direct or group conversations. Omit to match either.
  scope?: BindingScope;
}

/// A rule mapping matched conversations to a recipe and a session key.
/// Bindings are evaluated top-to-bottom, first match wins.
export interface BindingRule {
  match: BindingMatch;
  /// Recipe name from `recipes`. Omitted/unknown means the default recipe
  /// (no mounts, no mcp, messaging tool only).
  recipe?: string;
  /// Stable key used to derive the bound session id. Conversations sharing a
  /// key share a session. Omitted means the bridge derives a per-conversation
  /// key, so each conversation gets its own session.
  sessionKey?: string;
}

export interface TelegramBridgeConfig {
  enabled: boolean;
  botToken: string;
  accountId: string;
  allowedChatIds: string[];
  /// Sender handles allowed to run a turn at all. Empty means anyone may chat.
  allowFrom: string[];
  /// Sender handles allowed to run control commands (/activation, /status).
  controlAllowFrom: string[];
  triggerPrefixes: string[];
  mentionNames: string[];
  groupActivation: GroupActivation;
  /// Reply-quote behavior in groups (`off` | `first` | `all`). Direct chats
  /// never quote.
  replyToMode: ReplyToMode;
}

export interface WhatsAppBridgeConfig {
  enabled: boolean;
  accountId: string;
  authDir: string;
  allowedJids: string[];
  allowFrom: string[];
  controlAllowFrom: string[];
  triggerPrefixes: string[];
  mentionNames: string[];
  groupActivation: GroupActivation;
  replyToMode: ReplyToMode;
  printQr: boolean;
}

export interface MessagingBridgeConfig {
  lightspeed: LightspeedBridgeConfig;
  runtime: BridgeRuntimeConfig;
  store: {
    path: string;
  };
  recipes: Record<string, SessionRecipe>;
  bindings: BindingRule[];
  telegram?: TelegramBridgeConfig;
  whatsapp?: WhatsAppBridgeConfig;
}

/// Defaults applied to a recipe mount when `mountPath`/`access` are omitted.
const DEFAULT_MOUNT_PATH = "/workspace";
const DEFAULT_MOUNT_ACCESS: VfsMountAccess = "readWrite";

type PartialConfig = Partial<{
  lightspeed: Partial<LightspeedBridgeConfig>;
  runtime: Partial<BridgeRuntimeConfig>;
  store: Partial<{ path: string }>;
  recipes: Record<string, unknown>;
  bindings: unknown[];
  telegram: Partial<TelegramBridgeConfig>;
  whatsapp: Partial<WhatsAppBridgeConfig>;
}>;

export async function loadBridgeConfig(env: NodeJS.ProcessEnv = process.env): Promise<MessagingBridgeConfig> {
  const fileConfig = env.BRIDGE_CONFIG ? await readJsonConfig(env.BRIDGE_CONFIG) : {};
  const lightspeed = {
    endpoint:
      env.LIGHTSPEED_API_URL ??
      fileConfig.lightspeed?.endpoint ??
      "http://127.0.0.1:18080/rpc",
    cwd: env.LIGHTSPEED_CWD ?? fileConfig.lightspeed?.cwd ?? null,
    waitMs: parsePositiveInt(env.LIGHTSPEED_WAIT_MS, fileConfig.lightspeed?.waitMs ?? 30_000),
    eventLimit: parsePositiveInt(env.LIGHTSPEED_EVENT_LIMIT, fileConfig.lightspeed?.eventLimit ?? 128),
    sessionPrefix: env.BRIDGE_SESSION_PREFIX ?? fileConfig.lightspeed?.sessionPrefix ?? "bridge",
  };
  const runtime: BridgeRuntimeConfig = {
    debounceMs: parsePositiveInt(env.BRIDGE_DEBOUNCE_MS, fileConfig.runtime?.debounceMs ?? 500),
    turnMaxBatch: parsePositiveInt(
      env.BRIDGE_TURN_MAX_BATCH,
      fileConfig.runtime?.turnMaxBatch ?? 10,
    ),
    turnMaxWaitMs: parsePositiveInt(
      env.BRIDGE_TURN_MAX_WAIT_MS,
      fileConfig.runtime?.turnMaxWaitMs ?? 2_500,
    ),
    roomFlushMs: parsePositiveInt(env.BRIDGE_ROOM_FLUSH_MS, fileConfig.runtime?.roomFlushMs ?? 30_000),
    roomFlushMax: parsePositiveInt(
      env.BRIDGE_ROOM_FLUSH_MAX,
      fileConfig.runtime?.roomFlushMax ?? 20,
    ),
    roomBudget: parsePositiveInt(env.BRIDGE_ROOM_BUDGET, fileConfig.runtime?.roomBudget ?? 50),
  };
  const recipes = parseRecipes(fileConfig.recipes);
  const bindings = parseBindings(fileConfig.bindings, recipes);
  const telegramToken = env.TELEGRAM_BOT_TOKEN ?? fileConfig.telegram?.botToken ?? "";
  const telegramEnabled =
    parseBoolean(env.TELEGRAM_ENABLED, fileConfig.telegram?.enabled ?? Boolean(telegramToken));
  const whatsappEnabled = parseBoolean(env.WHATSAPP_ENABLED, fileConfig.whatsapp?.enabled ?? false);

  const config: MessagingBridgeConfig = {
    lightspeed,
    runtime,
    store: {
      path: env.BRIDGE_STATE_PATH ?? fileConfig.store?.path ?? ".bridge-state.json",
    },
    recipes,
    bindings,
  };
  if (telegramEnabled) {
    config.telegram = {
      enabled: true,
      botToken: requireValue("TELEGRAM_BOT_TOKEN", telegramToken),
      accountId: env.TELEGRAM_ACCOUNT_ID ?? fileConfig.telegram?.accountId ?? "default",
      allowedChatIds: csv(env.TELEGRAM_ALLOWED_CHAT_IDS, fileConfig.telegram?.allowedChatIds),
      allowFrom: csv(env.TELEGRAM_ALLOW_FROM, fileConfig.telegram?.allowFrom),
      controlAllowFrom: csv(
        env.TELEGRAM_CONTROL_ALLOW_FROM,
        fileConfig.telegram?.controlAllowFrom,
      ),
      triggerPrefixes: csv(
        env.TELEGRAM_TRIGGER_PREFIXES,
        fileConfig.telegram?.triggerPrefixes ?? ["/ask", "/lightspeed"],
      ),
      mentionNames: csv(env.TELEGRAM_MENTION_NAMES, fileConfig.telegram?.mentionNames),
      groupActivation: parseGroupActivation(
        "TELEGRAM_GROUP_ACTIVATION",
        env.TELEGRAM_GROUP_ACTIVATION,
        fileConfig.telegram?.groupActivation,
      ),
      replyToMode: parseReplyToMode(
        "TELEGRAM_REPLY_TO_MODE",
        env.TELEGRAM_REPLY_TO_MODE,
        fileConfig.telegram?.replyToMode,
      ),
    };
  }
  if (whatsappEnabled) {
    config.whatsapp = {
      enabled: true,
      accountId: env.WHATSAPP_ACCOUNT_ID ?? fileConfig.whatsapp?.accountId ?? "default",
      authDir: env.WHATSAPP_AUTH_DIR ?? fileConfig.whatsapp?.authDir ?? ".whatsapp-auth",
      allowedJids: csv(env.WHATSAPP_ALLOWED_JIDS, fileConfig.whatsapp?.allowedJids),
      allowFrom: csv(env.WHATSAPP_ALLOW_FROM, fileConfig.whatsapp?.allowFrom),
      controlAllowFrom: csv(
        env.WHATSAPP_CONTROL_ALLOW_FROM,
        fileConfig.whatsapp?.controlAllowFrom,
      ),
      triggerPrefixes: csv(
        env.WHATSAPP_TRIGGER_PREFIXES,
        fileConfig.whatsapp?.triggerPrefixes ?? ["/ask", "/lightspeed"],
      ),
      mentionNames: csv(env.WHATSAPP_MENTION_NAMES, fileConfig.whatsapp?.mentionNames),
      groupActivation: parseGroupActivation(
        "WHATSAPP_GROUP_ACTIVATION",
        env.WHATSAPP_GROUP_ACTIVATION,
        fileConfig.whatsapp?.groupActivation,
      ),
      replyToMode: parseReplyToMode(
        "WHATSAPP_REPLY_TO_MODE",
        env.WHATSAPP_REPLY_TO_MODE,
        fileConfig.whatsapp?.replyToMode,
      ),
      printQr: parseBoolean(env.WHATSAPP_PRINT_QR, fileConfig.whatsapp?.printQr ?? true),
    };
  }
  return config;
}

async function readJsonConfig(path: string): Promise<PartialConfig> {
  const raw = await readFile(path, "utf8");
  return JSON.parse(raw) as PartialConfig;
}

export interface BindingQuery {
  channel: "telegram" | "whatsapp";
  /// Every identity for the sender (e.g. a telegram numeric id and @username),
  /// any of which may match a configured handle.
  handles: readonly string[];
  chatId: string;
  scope: BindingScope;
}

export interface ChannelAccessConfig {
  allowFrom: readonly string[];
  controlAllowFrom: readonly string[];
}

/// Everything the runtime needs from config for one inbound message: whether
/// the sender may take a turn or run control commands, and which recipe and
/// session key the conversation binds to.
export interface InboundAccess {
  turnAllowed: boolean;
  controlAllowed: boolean;
  recipeName: string | null;
  recipe: SessionRecipe | null;
  sessionKey: string | null;
}

export function resolveInboundAccess(
  query: BindingQuery,
  access: ChannelAccessConfig,
  bindings: readonly BindingRule[],
  recipes: Record<string, SessionRecipe>,
): InboundAccess {
  const binding = resolveBinding(query, bindings);
  return {
    turnAllowed: !handleDenied(access.allowFrom, query.handles),
    // With no explicit control allowlist, direct chats are trusted for control
    // and group members are not (matches the prior default).
    controlAllowed:
      access.controlAllowFrom.length > 0
        ? !handleDenied(access.controlAllowFrom, query.handles)
        : query.scope === "direct",
    recipeName: binding.recipe,
    recipe: binding.recipe != null ? recipes[binding.recipe] ?? null : null,
    sessionKey: binding.sessionKey,
  };
}

export interface ResolvedBinding {
  recipe: string | null;
  sessionKey: string | null;
}

/// Finds the first binding rule matching the conversation. Returns the recipe
/// name (or null for the default recipe) and the configured session key (or
/// null, meaning the bridge derives a per-conversation key). When no rule
/// matches, returns the default recipe with a derived key.
export function resolveBinding(query: BindingQuery, bindings: readonly BindingRule[]): ResolvedBinding {
  for (const rule of bindings) {
    if (bindingMatches(rule.match, query)) {
      return { recipe: rule.recipe ?? null, sessionKey: rule.sessionKey ?? null };
    }
  }
  return { recipe: null, sessionKey: null };
}

function bindingMatches(match: BindingMatch, query: BindingQuery): boolean {
  if (match.channel !== "*" && match.channel !== query.channel) {
    return false;
  }
  if (match.scope !== undefined && match.scope !== query.scope) {
    return false;
  }
  if (match.chatId !== undefined && match.chatId !== query.chatId) {
    return false;
  }
  if (match.handle !== undefined && !bindingHandleMatches(match.handle, query.handles)) {
    return false;
  }
  return true;
}

/// Case-insensitive handle comparison that ignores a leading `@`, so a config
/// `@lukas` matches a telegram username `lukas` and vice versa.
export function handleMatches(configured: string, actual: string): boolean {
  return normalizeHandle(configured) === normalizeHandle(actual);
}

function handleMatchesAny(configured: string, actuals: readonly string[]): boolean {
  return actuals.some((actual) => handleMatches(configured, actual));
}

function bindingHandleMatches(
  configured: BindingHandleMatch,
  actuals: readonly string[],
): boolean {
  const handles = Array.isArray(configured) ? configured : [configured];
  return handles.some((handle) => handleMatchesAny(handle, actuals));
}

/// True when none of the sender's handles appears in `allowFrom`, given
/// `allowFrom`'s empty-means-anyone semantics. Used both for the turn gate and
/// the control-command gate.
export function handleDenied(allowFrom: readonly string[], handles: readonly string[]): boolean {
  if (allowFrom.length === 0) {
    return false;
  }
  return !allowFrom.some((entry) => handleMatchesAny(entry, handles));
}

function normalizeHandle(handle: string): string {
  return handle.trim().toLowerCase().replace(/^@/, "");
}

export function parseRecipes(raw: unknown): Record<string, SessionRecipe> {
  if (raw === undefined || raw === null) {
    return {};
  }
  if (typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error("recipes must be an object keyed by recipe name");
  }
  const recipes: Record<string, SessionRecipe> = {};
  for (const [name, value] of Object.entries(raw as Record<string, unknown>)) {
    recipes[name] = parseRecipe(name, value);
  }
  return recipes;
}

function parseRecipe(name: string, raw: unknown): SessionRecipe {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new Error(`recipe "${name}" must be an object`);
  }
  const record = raw as Record<string, unknown>;
  const recipe: SessionRecipe = {
    mounts: parseMounts(name, record.mounts),
    mcp: parseMcpLinks(name, record.mcp),
    environments: parseEnvironments(name, record),
  };
  if (record.config !== undefined && record.config !== null) {
    if (typeof record.config !== "object" || Array.isArray(record.config)) {
      throw new Error(`recipe "${name}".config must be an object`);
    }
    recipe.config = record.config as SessionConfigInput;
  }
  return recipe;
}

function parseEnvironments(recipe: string, record: Record<string, unknown>): RecipeEnvironment[] {
  if (record.environments !== undefined && record.envs !== undefined) {
    throw new Error(`recipe "${recipe}" must use environments or envs, not both`);
  }
  const raw = record.environments ?? record.envs;
  if (raw === undefined || raw === null) {
    return [];
  }
  if (!Array.isArray(raw)) {
    throw new Error(`recipe "${recipe}".environments must be an array`);
  }
  const environments = raw.map((entry, index) => parseEnvironment(recipe, index, entry));
  const activeCount = environments.filter((environment) => environment.activate).length;
  if (activeCount > 1) {
    throw new Error(`recipe "${recipe}".environments may activate at most one environment`);
  }
  return environments;
}

function parseEnvironment(recipe: string, index: number, raw: unknown): RecipeEnvironment {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new Error(`recipe "${recipe}".environments[${index}] must be an object`);
  }
  const record = raw as Record<string, unknown>;
  const envId = optionalString(record.envId);
  if (envId === undefined) {
    throw new Error(`recipe "${recipe}".environments[${index}] needs an envId`);
  }
  const providerId = optionalString(record.providerId);
  if (providerId === undefined) {
    throw new Error(`recipe "${recipe}".environments[${index}] needs a providerId`);
  }
  if (record.activate !== undefined && typeof record.activate !== "boolean") {
    throw new Error(`recipe "${recipe}".environments[${index}].activate must be a boolean`);
  }
  return {
    envId,
    providerId,
    targetId: optionalString(record.targetId) ?? "local",
    activate: typeof record.activate === "boolean" ? record.activate : true,
  };
}

function parseMounts(recipe: string, raw: unknown): RecipeMount[] {
  if (raw === undefined || raw === null) {
    return [];
  }
  if (!Array.isArray(raw)) {
    throw new Error(`recipe "${recipe}".mounts must be an array`);
  }
  return raw.map((entry, index) => parseMount(recipe, index, entry));
}

function parseMount(recipe: string, index: number, raw: unknown): RecipeMount {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new Error(`recipe "${recipe}".mounts[${index}] must be an object`);
  }
  const record = raw as Record<string, unknown>;
  const workspaceId = optionalString(record.workspaceId);
  const snapshotRef = optionalString(record.snapshotRef);
  const nestedSource = record.source as Record<string, unknown> | undefined;
  const source =
    workspaceId !== undefined
      ? { workspaceId }
      : snapshotRef !== undefined
        ? { snapshotRef }
        : nestedSource && optionalString(nestedSource.workspaceId) !== undefined
          ? { workspaceId: optionalString(nestedSource.workspaceId) as string }
          : nestedSource && optionalString(nestedSource.snapshotRef) !== undefined
            ? { snapshotRef: optionalString(nestedSource.snapshotRef) as string }
            : null;
  if (!source) {
    throw new Error(
      `recipe "${recipe}".mounts[${index}] needs a workspaceId or snapshotRef`,
    );
  }
  const access = optionalString(record.access);
  if (access !== undefined && access !== "readOnly" && access !== "readWrite") {
    throw new Error(
      `recipe "${recipe}".mounts[${index}].access must be readOnly or readWrite`,
    );
  }
  return {
    mountPath: optionalString(record.mountPath) ?? DEFAULT_MOUNT_PATH,
    source,
    access: (access as VfsMountAccess | undefined) ?? DEFAULT_MOUNT_ACCESS,
  };
}

function parseMcpLinks(recipe: string, raw: unknown): RecipeMcpLink[] {
  if (raw === undefined || raw === null) {
    return [];
  }
  if (!Array.isArray(raw)) {
    throw new Error(`recipe "${recipe}".mcp must be an array`);
  }
  return raw.map((entry, index) => parseMcpLink(recipe, index, entry));
}

function parseMcpLink(recipe: string, index: number, raw: unknown): RecipeMcpLink {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new Error(`recipe "${recipe}".mcp[${index}] must be an object`);
  }
  const record = raw as Record<string, unknown>;
  const serverId = optionalString(record.serverId);
  if (serverId === undefined) {
    throw new Error(`recipe "${recipe}".mcp[${index}] needs a serverId`);
  }
  const approval = optionalString(record.approval);
  if (
    approval !== undefined &&
    approval !== "providerDefault" &&
    approval !== "always" &&
    approval !== "never"
  ) {
    throw new Error(
      `recipe "${recipe}".mcp[${index}].approval must be providerDefault, always, or never`,
    );
  }
  const allowedTools = record.allowedTools;
  const link: RecipeMcpLink = { serverId };
  const serverLabel = optionalString(record.serverLabel);
  if (serverLabel !== undefined) link.serverLabel = serverLabel;
  const toolId = optionalString(record.toolId);
  if (toolId !== undefined) link.toolId = toolId;
  if (Array.isArray(allowedTools)) {
    link.allowedTools = allowedTools.map((tool) => String(tool));
  }
  if (approval !== undefined) link.approval = approval as RemoteMcpApprovalPolicy;
  const authGrantId = optionalString(record.authGrantId);
  if (authGrantId !== undefined) link.authGrantId = authGrantId;
  if (typeof record.deferLoading === "boolean") link.deferLoading = record.deferLoading;
  return link;
}

export function parseBindings(
  raw: unknown,
  recipes: Record<string, SessionRecipe>,
): BindingRule[] {
  if (raw === undefined || raw === null) {
    return [];
  }
  if (!Array.isArray(raw)) {
    throw new Error("bindings must be an array");
  }
  return raw.map((entry, index) => parseBinding(index, entry, recipes));
}

function parseBinding(
  index: number,
  raw: unknown,
  recipes: Record<string, SessionRecipe>,
): BindingRule {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new Error(`bindings[${index}] must be an object`);
  }
  const record = raw as Record<string, unknown>;
  const match = record.match as Record<string, unknown> | undefined;
  if (typeof match !== "object" || match === null || Array.isArray(match)) {
    throw new Error(`bindings[${index}].match must be an object`);
  }
  const channel = optionalString(match.channel) ?? "*";
  if (channel !== "telegram" && channel !== "whatsapp" && channel !== "*") {
    throw new Error(`bindings[${index}].match.channel must be telegram, whatsapp, or *`);
  }
  const scope = optionalString(match.scope);
  if (scope !== undefined && scope !== "direct" && scope !== "group") {
    throw new Error(`bindings[${index}].match.scope must be direct or group`);
  }
  const recipe = optionalString(record.recipe);
  if (recipe !== undefined && !(recipe in recipes)) {
    throw new Error(`bindings[${index}].recipe "${recipe}" is not defined in recipes`);
  }
  const matchRule: BindingMatch = { channel };
  const handle = optionalStringOrStringArray(match.handle);
  if (handle !== undefined) matchRule.handle = handle;
  const chatId = optionalString(match.chatId);
  if (chatId !== undefined) matchRule.chatId = chatId;
  if (scope !== undefined) matchRule.scope = scope as BindingScope;
  const rule: BindingRule = { match: matchRule };
  if (recipe !== undefined) rule.recipe = recipe;
  const sessionKey = optionalString(record.sessionKey);
  if (sessionKey !== undefined) rule.sessionKey = sessionKey;
  return rule;
}

function optionalString(value: unknown): string | undefined {
  if (value === undefined || value === null) {
    return undefined;
  }
  const text = String(value).trim();
  return text === "" ? undefined : text;
}

function optionalStringOrStringArray(value: unknown): string | string[] | undefined {
  if (Array.isArray(value)) {
    const items = value
      .map((entry) => optionalString(entry))
      .filter((entry): entry is string => entry !== undefined);
    return items.length > 0 ? items : undefined;
  }
  return optionalString(value);
}

function csv(value: string | undefined, fallback: readonly string[] | undefined): string[] {
  const source = value === undefined ? fallback ?? [] : value.split(",");
  return source.map((entry) => entry.trim()).filter(Boolean);
}

function parsePositiveInt(value: string | undefined, fallback: number): number {
  if (value === undefined || value.trim() === "") {
    return fallback;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function parseBoolean(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined || value.trim() === "") {
    return fallback;
  }
  return ["1", "true", "yes", "on"].includes(value.trim().toLowerCase());
}

function parseGroupActivation(
  name: string,
  value: string | undefined,
  fallback: GroupActivation | undefined,
): GroupActivation {
  const candidate = (value ?? fallback ?? "mention").toString().trim().toLowerCase();
  if (candidate === "mention" || candidate === "always" || candidate === "silent") {
    return candidate;
  }
  throw new Error(`${name} must be one of mention, always, silent`);
}

function parseReplyToMode(
  name: string,
  value: string | undefined,
  fallback: ReplyToMode | undefined,
): ReplyToMode {
  const candidate = (value ?? fallback ?? "first").toString().trim().toLowerCase();
  if (candidate === "off" || candidate === "first" || candidate === "all") {
    return candidate;
  }
  throw new Error(`${name} must be one of off, first, all`);
}

function requireValue(name: string, value: string): string {
  if (!value.trim()) {
    throw new Error(`${name} is required`);
  }
  return value;
}

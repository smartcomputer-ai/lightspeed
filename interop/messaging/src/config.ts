import { readFile } from "node:fs/promises";
import type { InlineAgentProfile, ProfileSource } from "@lightspeed/agent-client";
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

/// A rule mapping matched conversations to a profile and a session key.
/// Bindings are evaluated top-to-bottom, first match wins.
export interface BindingRule {
  match: BindingMatch;
  /// Agent profile source for matched conversations. A string is treated as a
  /// named profile id; an object is passed through as a ProfileSource.
  profile?: ProfileSource;
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
  bindings: BindingRule[];
  telegram?: TelegramBridgeConfig;
  whatsapp?: WhatsAppBridgeConfig;
}

type PartialConfig = Partial<{
  lightspeed: Partial<LightspeedBridgeConfig>;
  runtime: Partial<BridgeRuntimeConfig>;
  store: Partial<{ path: string }>;
  recipes: unknown;
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
  if (fileConfig.recipes !== undefined) {
    throw new Error("recipes are no longer supported; use bindings[].profile");
  }
  const bindings = parseBindings(fileConfig.bindings);
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
/// the sender may take a turn or run control commands, and which profile and
/// session key the conversation binds to.
export interface InboundAccess {
  turnAllowed: boolean;
  controlAllowed: boolean;
  profileLabel: string | null;
  profile: ProfileSource | null;
  sessionKey: string | null;
}

export function resolveInboundAccess(
  query: BindingQuery,
  access: ChannelAccessConfig,
  bindings: readonly BindingRule[],
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
    profileLabel: binding.profileLabel,
    profile: binding.profile,
    sessionKey: binding.sessionKey,
  };
}

export interface ResolvedBinding {
  profile: ProfileSource | null;
  profileLabel: string | null;
  sessionKey: string | null;
}

/// Finds the first binding rule matching the conversation. Returns the profile
/// source (or null for the default profile) and the configured session key (or
/// null, meaning the bridge derives a per-conversation key). When no rule
/// matches, returns the default profile with a derived key.
export function resolveBinding(query: BindingQuery, bindings: readonly BindingRule[]): ResolvedBinding {
  for (const rule of bindings) {
    if (bindingMatches(rule.match, query)) {
      return {
        profile: rule.profile ?? null,
        profileLabel: labelForProfile(rule.profile),
        sessionKey: rule.sessionKey ?? null,
      };
    }
  }
  return { profile: null, profileLabel: null, sessionKey: null };
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

export function parseBindings(raw: unknown): BindingRule[] {
  if (raw === undefined || raw === null) {
    return [];
  }
  if (!Array.isArray(raw)) {
    throw new Error("bindings must be an array");
  }
  return raw.map((entry, index) => parseBinding(index, entry));
}

function parseBinding(index: number, raw: unknown): BindingRule {
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
  if (record.recipe !== undefined) {
    throw new Error(`bindings[${index}].recipe is no longer supported; use bindings[${index}].profile`);
  }
  const profile = parseOptionalProfileSource(record.profile, `bindings[${index}].profile`);
  const matchRule: BindingMatch = { channel };
  const handle = optionalStringOrStringArray(match.handle);
  if (handle !== undefined) matchRule.handle = handle;
  const chatId = optionalString(match.chatId);
  if (chatId !== undefined) matchRule.chatId = chatId;
  if (scope !== undefined) matchRule.scope = scope as BindingScope;
  const rule: BindingRule = { match: matchRule };
  if (profile !== undefined) rule.profile = profile;
  const sessionKey = optionalString(record.sessionKey);
  if (sessionKey !== undefined) rule.sessionKey = sessionKey;
  return rule;
}

function parseOptionalProfileSource(raw: unknown, path: string): ProfileSource | undefined {
  if (raw === undefined || raw === null) {
    return undefined;
  }
  const named = optionalString(raw);
  if (named !== undefined && typeof raw !== "object") {
    return { kind: "named", profileId: named };
  }
  if (typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error(`${path} must be a profile id string or ProfileSource object`);
  }
  const record = raw as Record<string, unknown>;
  const kind = optionalString(record.kind);
  if (kind === "named") {
    const profileId = optionalString(record.profileId ?? record.profile_id);
    if (profileId === undefined) {
      throw new Error(`${path}.profileId is required for named profiles`);
    }
    return { kind: "named", profileId };
  }
  if (kind === "inline") {
    if (typeof record.profile !== "object" || record.profile === null || Array.isArray(record.profile)) {
      throw new Error(`${path}.profile must be an object for inline profiles`);
    }
    return { kind: "inline", profile: record.profile as InlineAgentProfile };
  }
  throw new Error(`${path}.kind must be named or inline`);
}

function labelForProfile(profile: ProfileSource | undefined): string | null {
  if (!profile) {
    return null;
  }
  return profile.kind === "named" ? profile.profileId : "inline";
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

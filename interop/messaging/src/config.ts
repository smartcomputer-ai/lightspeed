import { readFile } from "node:fs/promises";

export interface ForgeBridgeConfig {
  endpoint: string;
  cwd?: string | null;
  waitMs: number;
  eventLimit: number;
  sessionPrefix: string;
}

export interface TelegramBridgeConfig {
  enabled: boolean;
  botToken: string;
  accountId: string;
  allowedChatIds: string[];
  triggerPrefixes: string[];
  requireTrigger: boolean;
  mentionNames: string[];
}

export interface WhatsAppBridgeConfig {
  enabled: boolean;
  accountId: string;
  authDir: string;
  allowedJids: string[];
  triggerPrefixes: string[];
  requireTrigger: boolean;
  mentionNames: string[];
  printQr: boolean;
}

export interface MessagingBridgeConfig {
  forge: ForgeBridgeConfig;
  store: {
    path: string;
  };
  telegram?: TelegramBridgeConfig;
  whatsapp?: WhatsAppBridgeConfig;
}

type PartialConfig = Partial<{
  forge: Partial<ForgeBridgeConfig>;
  store: Partial<{ path: string }>;
  telegram: Partial<TelegramBridgeConfig>;
  whatsapp: Partial<WhatsAppBridgeConfig>;
}>;

export async function loadBridgeConfig(env: NodeJS.ProcessEnv = process.env): Promise<MessagingBridgeConfig> {
  const fileConfig = env.BRIDGE_CONFIG ? await readJsonConfig(env.BRIDGE_CONFIG) : {};
  const forge = {
    endpoint:
      env.FORGE_API_URL ??
      fileConfig.forge?.endpoint ??
      "http://127.0.0.1:18080/rpc",
    cwd: env.FORGE_CWD ?? fileConfig.forge?.cwd ?? null,
    waitMs: parsePositiveInt(env.FORGE_WAIT_MS, fileConfig.forge?.waitMs ?? 30_000),
    eventLimit: parsePositiveInt(env.FORGE_EVENT_LIMIT, fileConfig.forge?.eventLimit ?? 128),
    sessionPrefix: env.BRIDGE_SESSION_PREFIX ?? fileConfig.forge?.sessionPrefix ?? "bridge",
  };
  const telegramToken = env.TELEGRAM_BOT_TOKEN ?? fileConfig.telegram?.botToken ?? "";
  const telegramEnabled =
    parseBoolean(env.TELEGRAM_ENABLED, fileConfig.telegram?.enabled ?? Boolean(telegramToken));
  const whatsappEnabled = parseBoolean(env.WHATSAPP_ENABLED, fileConfig.whatsapp?.enabled ?? false);

  const config: MessagingBridgeConfig = {
    forge,
    store: {
      path: env.BRIDGE_STATE_PATH ?? fileConfig.store?.path ?? ".bridge-state.json",
    },
  };
  if (telegramEnabled) {
    config.telegram = {
      enabled: true,
      botToken: requireValue("TELEGRAM_BOT_TOKEN", telegramToken),
      accountId: env.TELEGRAM_ACCOUNT_ID ?? fileConfig.telegram?.accountId ?? "default",
      allowedChatIds: csv(env.TELEGRAM_ALLOWED_CHAT_IDS, fileConfig.telegram?.allowedChatIds),
      triggerPrefixes: csv(
        env.TELEGRAM_TRIGGER_PREFIXES,
        fileConfig.telegram?.triggerPrefixes ?? ["/ask", "/forge"],
      ),
      requireTrigger: parseBoolean(
        env.TELEGRAM_REQUIRE_TRIGGER,
        fileConfig.telegram?.requireTrigger ?? true,
      ),
      mentionNames: csv(env.TELEGRAM_MENTION_NAMES, fileConfig.telegram?.mentionNames),
    };
  }
  if (whatsappEnabled) {
    config.whatsapp = {
      enabled: true,
      accountId: env.WHATSAPP_ACCOUNT_ID ?? fileConfig.whatsapp?.accountId ?? "default",
      authDir: env.WHATSAPP_AUTH_DIR ?? fileConfig.whatsapp?.authDir ?? ".whatsapp-auth",
      allowedJids: csv(env.WHATSAPP_ALLOWED_JIDS, fileConfig.whatsapp?.allowedJids),
      triggerPrefixes: csv(
        env.WHATSAPP_TRIGGER_PREFIXES,
        fileConfig.whatsapp?.triggerPrefixes ?? ["/ask", "/forge"],
      ),
      requireTrigger: parseBoolean(
        env.WHATSAPP_REQUIRE_TRIGGER,
        fileConfig.whatsapp?.requireTrigger ?? true,
      ),
      mentionNames: csv(env.WHATSAPP_MENTION_NAMES, fileConfig.whatsapp?.mentionNames),
      printQr: parseBoolean(env.WHATSAPP_PRINT_QR, fileConfig.whatsapp?.printQr ?? true),
    };
  }
  return config;
}

async function readJsonConfig(path: string): Promise<PartialConfig> {
  const raw = await readFile(path, "utf8");
  return JSON.parse(raw) as PartialConfig;
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

function requireValue(name: string, value: string): string {
  if (!value.trim()) {
    throw new Error(`${name} is required`);
  }
  return value;
}

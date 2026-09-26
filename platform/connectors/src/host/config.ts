import type { ChannelProvider } from "@lightspeed-ai/agent-client";
import { parseAccountSelector, type AccountSelector } from "../core/identity.js";
import { parseWhatsAppMediaLocatorKey } from "../providers/whatsapp/media.js";
import { parsePositiveInteger } from "./rate-limit.js";

export const CHANNEL_PROVIDERS: readonly ChannelProvider[] = ["telegram", "whatsapp"];

export interface ConnectorHostConfig {
  /** Core JSON-RPC endpoint. */
  apiUrl: string;
  apiKey: string;
  /** Providers this host serves. */
  providers: ChannelProvider[];
  /** Accounts to serve, or every discovered account when null. */
  accounts: AccountSelector[] | null;
  discoveryIntervalMs: number;
  temporal: { address: string; namespace: string };
  /** WhatsApp session state and media-locator key; required when WhatsApp is served. */
  whatsapp: { authDir: string; mediaLocatorKey: Uint8Array } | null;
  ingressMaxPerMinute: number;
  health: { host: string; port: number };
  metrics: { host: string; port: number };
}

export function parseHostConfig(env: NodeJS.ProcessEnv): ConnectorHostConfig {
  const apiUrl = required(env, "LIGHTSPEED_API_URL");
  const providers = parseProviders(env.LIGHTSPEED_CONNECTOR_PROVIDERS);
  const whatsapp = providers.includes("whatsapp")
    ? {
        authDir: required(env, "LIGHTSPEED_CONNECTOR_WHATSAPP_AUTH_DIR"),
        mediaLocatorKey: parseWhatsAppMediaLocatorKey(
          required(env, "LIGHTSPEED_CONNECTOR_WHATSAPP_MEDIA_LOCATOR_KEY"),
        ),
      }
    : null;
  return {
    apiUrl,
    apiKey: required(env, "LIGHTSPEED_CONNECTOR_API_KEY"),
    providers,
    accounts: parseAccountSelectors(env.LIGHTSPEED_CONNECTOR_ACCOUNTS),
    discoveryIntervalMs: parsePositiveInteger(
      env.LIGHTSPEED_CONNECTOR_DISCOVERY_INTERVAL_MS,
      30_000,
    ),
    temporal: {
      address: env.TEMPORAL_ADDRESS ?? "localhost:7233",
      namespace: env.TEMPORAL_NAMESPACE ?? "default",
    },
    whatsapp,
    ingressMaxPerMinute: parsePositiveInteger(env.LIGHTSPEED_CONNECTOR_INGRESS_MAX_PER_MINUTE, 120),
    health: {
      host: env.LIGHTSPEED_CONNECTOR_HEALTH_HOST ?? "0.0.0.0",
      port: parsePort(env.LIGHTSPEED_CONNECTOR_HEALTH_PORT, 8_090, "LIGHTSPEED_CONNECTOR_HEALTH_PORT"),
    },
    metrics: {
      host: env.LIGHTSPEED_CONNECTOR_METRICS_HOST ?? "0.0.0.0",
      port: parsePort(env.LIGHTSPEED_CONNECTOR_METRICS_PORT, 9_090, "LIGHTSPEED_CONNECTOR_METRICS_PORT"),
    },
  };
}

/** `LIGHTSPEED_CONNECTOR_PROVIDERS`: a comma list; unset serves every provider. */
export function parseProviders(value: string | undefined): ChannelProvider[] {
  if (value === undefined || value.trim().length === 0) return [...CHANNEL_PROVIDERS];
  const providers: ChannelProvider[] = [];
  for (const entry of value.split(",")) {
    const provider = entry.trim();
    if (provider.length === 0) continue;
    if (!isChannelProvider(provider)) {
      throw new TypeError(
        `invalid LIGHTSPEED_CONNECTOR_PROVIDERS entry ${JSON.stringify(provider)}; expected telegram or whatsapp`,
      );
    }
    if (!providers.includes(provider)) providers.push(provider);
  }
  if (providers.length === 0) {
    throw new TypeError("LIGHTSPEED_CONNECTOR_PROVIDERS must name at least one provider");
  }
  return providers;
}

/** `LIGHTSPEED_CONNECTOR_ACCOUNTS`: a comma list of `<universeId>/<accountId>`; unset serves all. */
export function parseAccountSelectors(value: string | undefined): AccountSelector[] | null {
  if (value === undefined || value.trim().length === 0) return null;
  const selectors: AccountSelector[] = [];
  for (const entry of value.split(",")) {
    const trimmed = entry.trim();
    if (trimmed.length === 0) continue;
    const selector = parseAccountSelector(trimmed);
    if (
      !selectors.some(
        (existing) =>
          existing.universeId === selector.universeId && existing.accountId === selector.accountId,
      )
    ) {
      selectors.push(selector);
    }
  }
  return selectors.length === 0 ? null : selectors;
}

export function isChannelProvider(value: string): value is ChannelProvider {
  return (CHANNEL_PROVIDERS as readonly string[]).includes(value);
}

export function parsePort(value: string | undefined, fallback: number, name: string): number {
  const port = value === undefined || value.length === 0 ? fallback : Number(value);
  if (!Number.isSafeInteger(port) || port < 0 || port > 65_535) {
    throw new TypeError(`${name} must be an integer between 0 and 65535`);
  }
  return port;
}

function required(env: NodeJS.ProcessEnv, name: string): string {
  const value = env[name];
  if (value === undefined || value.length === 0) {
    throw new TypeError(`${name} is required`);
  }
  return value;
}

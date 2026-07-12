import type { ConfiguratorAuthMode } from "./request-auth.js";

export interface ConfiguratorConfig {
  bindHost: string;
  bindPort: number;
  allowedHosts: string[];
  allowedOrigins: string[];
  rpcEndpoint: string;
  authMode: ConfiguratorAuthMode;
  maxBodyBytes: number;
  upstreamTimeoutMs: number;
  shutdownTimeoutMs: number;
}

export function configFromEnv(env: NodeJS.ProcessEnv = process.env): ConfiguratorConfig {
  const bindHost = env.LIGHTSPEED_CONFIGURATOR_MCP_BIND_HOST ?? "127.0.0.1";
  const bindPort = integerEnv(env, "LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT", 18081);
  const rpcEndpoint = env.LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL ?? "http://127.0.0.1:18080/rpc";
  const authMode = parseAuthMode(env.LIGHTSPEED_AUTH_MODE ?? "single");
  const allowedHosts = listEnv(env.LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_HOSTS);
  const allowedOrigins = listEnv(env.LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_ORIGINS);
  const maxBodyBytes = integerEnv(
    env,
    "LIGHTSPEED_CONFIGURATOR_MCP_MAX_BODY_BYTES",
    64 * 1024 * 1024,
  );
  const upstreamTimeoutMs = integerEnv(
    env,
    "LIGHTSPEED_CONFIGURATOR_MCP_UPSTREAM_TIMEOUT_MS",
    60_000,
  );
  const shutdownTimeoutMs = integerEnv(
    env,
    "LIGHTSPEED_CONFIGURATOR_MCP_SHUTDOWN_TIMEOUT_MS",
    10_000,
  );

  if (!isLoopback(bindHost) && allowedHosts.length === 0) {
    throw new Error(
      "LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_HOSTS is required when binding beyond localhost",
    );
  }
  new URL(rpcEndpoint);
  return {
    bindHost,
    bindPort,
    allowedHosts: allowedHosts.length > 0 ? allowedHosts : loopbackHosts(bindHost),
    allowedOrigins,
    rpcEndpoint,
    authMode,
    maxBodyBytes,
    upstreamTimeoutMs,
    shutdownTimeoutMs,
  };
}

function parseAuthMode(value: string): ConfiguratorAuthMode {
  if (value === "single" || value === "trusted-header" || value === "api-key") {
    return value;
  }
  throw new Error(
    `invalid LIGHTSPEED_AUTH_MODE=${JSON.stringify(value)}; expected single, trusted-header, or api-key`,
  );
}

function integerEnv(env: NodeJS.ProcessEnv, name: string, fallback: number): number {
  const raw = env[name];
  if (raw === undefined || raw === "") {
    return fallback;
  }
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return value;
}

function listEnv(value: string | undefined): string[] {
  return value
    ? value
        .split(",")
        .map((entry) => entry.trim())
        .filter(Boolean)
    : [];
}

function isLoopback(host: string): boolean {
  return host === "127.0.0.1" || host === "localhost" || host === "::1";
}

function loopbackHosts(host: string): string[] {
  if (host === "::1") {
    return ["[::1]", "localhost"];
  }
  return ["127.0.0.1", "localhost"];
}

export interface ServerEnv {
  databaseUrl: string;
  authSecret: string;
  /// Public origin the platform is served from.
  baseUrl: string;
  /// Additional browser origins accepted by Better Auth.
  trustedOrigins: string[];
  port: number;
  /// Bootstrap admin, applied only when the users table is empty.
  adminEmail: string | null;
  adminPassword: string | null;
  adminPrincipalId?: string | null;
  /// Explicit local launcher fixtures; disabled in ordinary server startup.
  devSeed?: boolean;
  github: { clientId: string; clientSecret: string } | null;
  /// Authenticated Lightspeed gateway RPC endpoint.
  lightspeedApiUrl: string | null;
  /// Authenticated Platform service credential; requires deployment assert_user and manage_identity capabilities.
  lightspeedApiKey?: string | null;
  /// Public Streamable HTTP endpoint installed by the Configurator setup.
  configuratorMcpUrl: string | null;
  /// Permit the installed Configurator MCP record to reach a private network.
  /// This is intended for explicit local/internal deployments only.
  configuratorMcpAllowPrivateNetwork: boolean;
  /// Retired configuration field, always false; true is rejected at startup.
  configuratorMcpInternalTrustedHeader: boolean;
  /// Internal connector health endpoints aggregated for platform admins.
  channelsHealthUrls: string[];
  /// Development convenience: a directly attached `lightspeed-envd` endpoint
  /// (started by ./dev.sh) offered as the default when registering an
  /// external environment. Never set in deployed configuration.
  devEnvdEndpoint: string | null;
}

function required(name: string): string {
  const value = process.env[name] || undefined;
  if (!value) {
    throw new Error(`Missing required environment variable ${name}`);
  }
  return value;
}

export function loadEnv(): ServerEnv {
  if (booleanEnv("LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER", false)) {
    throw new Error("Configurator trusted-header authentication is retired; use a scoped service key");
  }
  const lightspeedApiKey = process.env.LIGHTSPEED_PLATFORM_API_KEY ?? null;
  if (lightspeedApiKey !== null && !/^lsk_[A-Za-z0-9_-]+$/.test(lightspeedApiKey)) {
    throw new Error("LIGHTSPEED_PLATFORM_API_KEY must be a Lightspeed bearer key");
  }
  const github =
    process.env.LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID &&
    process.env.LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET
      ? {
          clientId: process.env.LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID,
          clientSecret: process.env.LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET,
        }
      : null;
  return {
    databaseUrl: required("LIGHTSPEED_PLATFORM_DATABASE_URL"),
    authSecret: required("LIGHTSPEED_PLATFORM_AUTH_SECRET"),
    baseUrl: process.env.LIGHTSPEED_PLATFORM_BASE_URL ?? "http://localhost:3000",
    trustedOrigins: csv(process.env.LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS),
    port: Number(process.env.PORT ?? 3000),
    adminEmail: process.env.LIGHTSPEED_PLATFORM_ADMIN_EMAIL ?? null,
    adminPassword: process.env.LIGHTSPEED_PLATFORM_ADMIN_PASSWORD ?? null,
    adminPrincipalId: process.env.LIGHTSPEED_PLATFORM_ADMIN_PRINCIPAL_ID ?? null,
    devSeed: booleanEnv("LIGHTSPEED_PLATFORM_DEV_SEED", false),
    github,
    lightspeedApiUrl: process.env.LIGHTSPEED_API_URL ?? null,
    lightspeedApiKey,
    configuratorMcpUrl: process.env.LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL ?? null,
    configuratorMcpAllowPrivateNetwork: booleanEnv(
      "LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_ALLOW_PRIVATE_NETWORK",
      false,
    ),
    configuratorMcpInternalTrustedHeader: booleanEnv(
      "LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER",
      false,
    ),
    channelsHealthUrls: csv(process.env.LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS),
    devEnvdEndpoint: process.env.LIGHTSPEED_PLATFORM_DEV_ENVD_ENDPOINT ?? null,
  };
}

function booleanEnv(name: string, fallback: boolean): boolean {
  const value = process.env[name]?.trim().toLowerCase();
  if (!value) return fallback;
  if (value === "true") return true;
  if (value === "false") return false;
  throw new Error(`${name} must be true or false`);
}

function csv(value: string | undefined): string[] {
  return value?.split(",").map((entry) => entry.trim()).filter(Boolean) ?? [];
}

export interface ServerEnv {
  databaseUrl: string;
  authSecret: string;
  /// Public origin the platform is served from.
  baseUrl: string;
  port: number;
  /// Bootstrap admin, applied only when the users table is empty.
  adminEmail: string | null;
  adminPassword: string | null;
  github: { clientId: string; clientSecret: string } | null;
  /// Lightspeed gateway RPC endpoint (trusted-header mode). Unused since the
  /// provisioner was dropped; serves the universe editing passthrough (U3).
  lightspeedApiUrl: string | null;
  /// Public Streamable HTTP endpoint installed by the Configurator setup.
  configuratorMcpUrl: string | null;
  /// Internal connector health endpoints aggregated for platform admins.
  channelsHealthUrls: string[];
}

function required(name: string): string {
  const value = process.env[name] || undefined;
  if (!value) {
    throw new Error(`Missing required environment variable ${name}`);
  }
  return value;
}

export function loadEnv(): ServerEnv {
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
    port: Number(process.env.PORT ?? 3000),
    adminEmail: process.env.LIGHTSPEED_PLATFORM_ADMIN_EMAIL ?? null,
    adminPassword: process.env.LIGHTSPEED_PLATFORM_ADMIN_PASSWORD ?? null,
    github,
    lightspeedApiUrl: process.env.LIGHTSPEED_API_URL ?? null,
    configuratorMcpUrl: process.env.LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL ?? null,
    channelsHealthUrls: csv(process.env.LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS),
  };
}

function csv(value: string | undefined): string[] {
  return value?.split(",").map((entry) => entry.trim()).filter(Boolean) ?? [];
}

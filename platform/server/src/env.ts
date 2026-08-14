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

function compatible(name: string, legacyName: string): string | undefined {
  const value = process.env[name];
  const legacyValue = process.env[legacyName];
  if (value && legacyValue && value !== legacyValue) {
    throw new Error(`Conflicting environment variables ${name} and ${legacyName}`);
  }
  return value ?? legacyValue;
}

function required(name: string, legacyName: string): string {
  const value = compatible(name, legacyName);
  if (!value) {
    throw new Error(`Missing required environment variable ${name}`);
  }
  return value;
}

export function loadEnv(): ServerEnv {
  const github =
    compatible("LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID", "LSBOT_GITHUB_CLIENT_ID") &&
    compatible("LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET", "LSBOT_GITHUB_CLIENT_SECRET")
      ? {
          clientId: compatible(
            "LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID",
            "LSBOT_GITHUB_CLIENT_ID",
          )!,
          clientSecret: compatible(
            "LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET",
            "LSBOT_GITHUB_CLIENT_SECRET",
          )!,
        }
      : null;
  return {
    databaseUrl: required("LIGHTSPEED_PLATFORM_DATABASE_URL", "LSBOT_DATABASE_URL"),
    authSecret: required("LIGHTSPEED_PLATFORM_AUTH_SECRET", "LSBOT_AUTH_SECRET"),
    baseUrl:
      compatible("LIGHTSPEED_PLATFORM_BASE_URL", "LSBOT_BASE_URL") ??
      "http://localhost:3000",
    port: Number(process.env.PORT ?? 3000),
    adminEmail:
      compatible("LIGHTSPEED_PLATFORM_ADMIN_EMAIL", "LSBOT_ADMIN_EMAIL") ?? null,
    adminPassword:
      compatible("LIGHTSPEED_PLATFORM_ADMIN_PASSWORD", "LSBOT_ADMIN_PASSWORD") ?? null,
    github,
    lightspeedApiUrl: process.env.LIGHTSPEED_API_URL ?? null,
    configuratorMcpUrl:
      compatible(
        "LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL",
        "LSBOT_CONFIGURATOR_MCP_URL",
      ) ?? null,
    channelsHealthUrls: csv(
      compatible(
        "LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS",
        "LSBOT_CHANNELS_HEALTH_URLS",
      ),
    ),
  };
}

function csv(value: string | undefined): string[] {
  return value?.split(",").map((entry) => entry.trim()).filter(Boolean) ?? [];
}

export interface OidcConfig {
  issuer: string;
  discoveryUrl: string;
  clientId: string;
  clientSecret: string;
  scopes: string[];
  groupsClaim: string;
  userGroup: string;
  adminGroup: string;
  claimsToken: "id" | "access";
  resource?: string;
  audience?: string;
  sessionMaxAgeSeconds: number;
  autoSignIn: boolean;
}

export interface ServerEnv {
  databaseUrl: string;
  authSecret: string;
  /// Public origin the platform is served from.
  baseUrl: string;
  /// Additional browser origins accepted by Better Auth.
  trustedOrigins: string[];
  port: number;
  /// Seeds the first admin, or designates an existing local password admin for emergency access.
  adminEmail: string | null;
  adminPassword: string | null;
  /// Explicit local launcher fixtures; disabled in ordinary server startup.
  devSeed?: boolean;
  github: { clientId: string; clientSecret: string } | null;
  oidc?: OidcConfig | null;
  passwordSignIn?: "local" | "break-glass" | "off";
  /// Authenticated Lightspeed gateway RPC endpoint.
  lightspeedApiUrl: string | null;
  /// The Platform's deployment key: every group, and allowed to assert the
  /// signed-in user as the actor (`server api-key bootstrap`).
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
  const oidc = loadOidc();
  const passwordSignIn = process.env.LIGHTSPEED_PLATFORM_PASSWORD_SIGN_IN || (oidc ? "break-glass" : "local");
  if (!["break-glass", "off", ...(oidc ? [] : ["local"])].includes(passwordSignIn)) {
    throw new Error("LIGHTSPEED_PLATFORM_PASSWORD_SIGN_IN must be break-glass or off with OIDC, or local without OIDC");
  }
  return {
    databaseUrl: required("LIGHTSPEED_PLATFORM_DATABASE_URL"),
    authSecret: required("LIGHTSPEED_PLATFORM_AUTH_SECRET"),
    baseUrl: process.env.LIGHTSPEED_PLATFORM_BASE_URL ?? "http://localhost:3000",
    trustedOrigins: csv(process.env.LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS),
    port: Number(process.env.PORT ?? 3000),
    adminEmail: process.env.LIGHTSPEED_PLATFORM_ADMIN_EMAIL ?? null,
    adminPassword: process.env.LIGHTSPEED_PLATFORM_ADMIN_PASSWORD ?? null,
    devSeed: booleanEnv("LIGHTSPEED_PLATFORM_DEV_SEED", false),
    github,
    oidc,
    passwordSignIn: passwordSignIn as ServerEnv["passwordSignIn"],
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

function loadOidc(): OidcConfig | null {
  const prefix = "LIGHTSPEED_PLATFORM_OIDC_";
  if (!Object.keys(process.env).some((key) => key.startsWith(prefix) && process.env[key])) return null;
  const issuer = required(`${prefix}ISSUER`);
  const issuerUrl = new URL(issuer);
  if (!["http:", "https:"].includes(issuerUrl.protocol) || issuerUrl.username || issuerUrl.password || issuerUrl.hash || issuerUrl.search) {
    throw new Error("OIDC issuer must be an absolute HTTP(S) identifier without credentials, query or fragment");
  }
  if (issuerUrl.protocol === "http:" && !["localhost", "127.0.0.1", "[::1]"].includes(issuerUrl.hostname)) {
    throw new Error("OIDC issuer must use HTTPS (HTTP is allowed on loopback for development)");
  }
  const discoveryUrl = process.env[`${prefix}DISCOVERY_URL`] || `${issuer.replace(/\/$/, "")}/.well-known/openid-configuration`;
  const discovery = new URL(discoveryUrl);
  if ((discovery.protocol !== "https:" && !(discovery.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(discovery.hostname))) || discovery.username || discovery.password || discovery.hash) {
    throw new Error("OIDC discovery must use HTTPS (HTTP is allowed on loopback for development)");
  }
  const userGroup = required(`${prefix}USER_GROUP`);
  const adminGroup = required(`${prefix}ADMIN_GROUP`);
  if (userGroup === adminGroup) throw new Error("OIDC user and admin entitlements must be distinct");
  const claimsToken = process.env[`${prefix}CLAIMS_TOKEN`] || "id";
  if (claimsToken !== "id" && claimsToken !== "access") throw new Error("OIDC_CLAIMS_TOKEN must be id or access");
  const sessionMaxAgeSeconds = Number(process.env[`${prefix}SESSION_MAX_AGE_SECONDS`] || 28800);
  if (!Number.isSafeInteger(sessionMaxAgeSeconds) || sessionMaxAgeSeconds < 60 || sessionMaxAgeSeconds > 86400) {
    throw new Error("OIDC_SESSION_MAX_AGE_SECONDS must be an integer from 60 to 86400");
  }
  const scopes = (process.env[`${prefix}SCOPES`] || "openid profile email").split(/\s+/).filter(Boolean);
  if (!scopes.includes("openid") || scopes.includes("offline_access")) throw new Error("OIDC scopes must include openid and must not include offline_access");
  return {
    issuer, discoveryUrl, userGroup, adminGroup, claimsToken, sessionMaxAgeSeconds, scopes,
    autoSignIn: booleanEnv(`${prefix}AUTO_SIGN_IN`, true),
    clientId: required(`${prefix}CLIENT_ID`),
    clientSecret: required(`${prefix}CLIENT_SECRET`),
    groupsClaim: process.env[`${prefix}GROUPS_CLAIM`] || "groups",
    resource: process.env[`${prefix}RESOURCE`] || undefined,
    audience: claimsToken === "access" ? required(`${prefix}AUDIENCE`) : undefined,
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

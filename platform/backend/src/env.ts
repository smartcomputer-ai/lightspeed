import { WorkloadFederation } from "./federation.js";
import { readFederationConfig } from "./federation-config.js";
import { readFileSync } from "node:fs";
import { databaseWorkloadIdentityFromEnv, type DatabaseWorkloadIdentity } from "@lightspeed-ai/platform-db";
import {
  DEFAULT_WORKSPACE_UPLOAD_LIMITS,
  MAX_CONFIGURABLE_WORKSPACE_UPLOAD_BYTES,
  type WorkspaceUploadLimits,
} from "@lightspeed-ai/platform-shared";

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
  /// Set when the database login uses a Microsoft Entra token instead of a password.
  databaseWorkloadIdentity?: DatabaseWorkloadIdentity | null;
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
  browserSessionIdleDays?: number;
  browserSessionMaxDays?: number;
  personalOrganizations?: boolean;
  personalOrganizationUniverseQuota?: number;
  sharedOrganizationUniverseQuota?: number;
  apiKeyDefaultExpiryDays?: number;
  /** Unset: no maximum, and keys may be created without expiry. */
  apiKeyMaxExpiryDays?: number;
  github: { clientId: string; clientSecret: string } | null;
  oidc?: OidcConfig | null;
  federation?: WorkloadFederation;
  passwordSignIn?: "local" | "break-glass" | "off";
  /// Authenticated Lightspeed runtime HTTP API endpoint.
  lightspeedApiUrl: string | null;
  /// Full-authority runtime credential, held only by the Platform backend.
  lightspeedApiToken?: string | null;
  /// Development convenience: a directly attached `lightspeed-envd` endpoint
  /// (started by ./dev.sh) offered as the default when registering an
  /// external environment. Never set in deployed configuration.
  devEnvdEndpoint: string | null;
  /// Workspace upload limits for the web UI and the upload route.
  workspaceUploadLimits?: WorkspaceUploadLimits;
}

function required(name: string): string {
  const value = process.env[name] || undefined;
  if (!value) {
    throw new Error(`Missing required environment variable ${name}`);
  }
  return value;
}

export function loadEnv(): ServerEnv {
  const token = process.env.LIGHTSPEED_PLATFORM_API_TOKEN;
  const tokenFile = process.env.LIGHTSPEED_PLATFORM_API_TOKEN_FILE;
  if (token !== undefined && tokenFile !== undefined) {
    throw new Error("Set only one of LIGHTSPEED_PLATFORM_API_TOKEN and LIGHTSPEED_PLATFORM_API_TOKEN_FILE");
  }
  let lightspeedApiToken: string | null = token ?? null;
  if (tokenFile !== undefined) {
    try { lightspeedApiToken = readFileSync(tokenFile, "utf8").replace(/\r?\n$/, ""); }
    catch { throw new Error("Cannot read LIGHTSPEED_PLATFORM_API_TOKEN_FILE"); }
  }
  if (lightspeedApiToken !== null && (lightspeedApiToken.length < 32 || /[^\x21-\x7e]/u.test(lightspeedApiToken))) {
    throw new Error("LIGHTSPEED_PLATFORM_API_TOKEN must contain at least 32 printable ASCII characters without whitespace");
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
    databaseWorkloadIdentity: databaseWorkloadIdentityFromEnv(process.env),
    authSecret: required("LIGHTSPEED_PLATFORM_AUTH_SECRET"),
    baseUrl: process.env.LIGHTSPEED_PLATFORM_BASE_URL ?? "http://localhost:3000",
    trustedOrigins: csv(process.env.LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS),
    port: Number(process.env.PORT ?? 3000),
    adminEmail: process.env.LIGHTSPEED_PLATFORM_ADMIN_EMAIL ?? null,
    adminPassword: process.env.LIGHTSPEED_PLATFORM_ADMIN_PASSWORD ?? null,
    devSeed: booleanEnv("LIGHTSPEED_PLATFORM_DEV_SEED", false),
    browserSessionIdleDays: integerEnv("LIGHTSPEED_PLATFORM_SESSION_IDLE_DAYS", 30, 1, 365),
    browserSessionMaxDays: integerEnv("LIGHTSPEED_PLATFORM_SESSION_MAX_DAYS", 90, 1, 3650),
    // Platform settings: set here, they win over a Platform Admin's value.
    personalOrganizations: optionalBooleanEnv("LIGHTSPEED_PLATFORM_PERSONAL_ORGANIZATIONS"),
    personalOrganizationUniverseQuota: optionalIntegerEnv("LIGHTSPEED_PLATFORM_PERSONAL_UNIVERSE_QUOTA", 0, 100000),
    sharedOrganizationUniverseQuota: optionalIntegerEnv("LIGHTSPEED_PLATFORM_SHARED_UNIVERSE_QUOTA", 0, 100000),
    apiKeyDefaultExpiryDays: integerEnv("LIGHTSPEED_PLATFORM_API_KEY_DEFAULT_EXPIRY_DAYS", 365, 1, 3650),
    apiKeyMaxExpiryDays: optionalIntegerEnv("LIGHTSPEED_PLATFORM_API_KEY_MAX_EXPIRY_DAYS", 1, 3650),
    github,
    oidc,
    federation: process.env.LIGHTSPEED_PLATFORM_FEDERATION_FILE
      ? new WorkloadFederation(readFederationConfig(process.env.LIGHTSPEED_PLATFORM_FEDERATION_FILE)) : undefined,
    passwordSignIn: passwordSignIn as ServerEnv["passwordSignIn"],
    lightspeedApiUrl: process.env.LIGHTSPEED_API_URL ?? null,
    lightspeedApiToken,
    devEnvdEndpoint: process.env.LIGHTSPEED_PLATFORM_DEV_ENVD_ENDPOINT ?? null,
    workspaceUploadLimits: {
      maxBytes: integerEnv(
        "LIGHTSPEED_PLATFORM_WORKSPACE_UPLOAD_MAX_BYTES",
        DEFAULT_WORKSPACE_UPLOAD_LIMITS.maxBytes,
        1,
        MAX_CONFIGURABLE_WORKSPACE_UPLOAD_BYTES,
      ),
      maxEntries: integerEnv(
        "LIGHTSPEED_PLATFORM_WORKSPACE_UPLOAD_MAX_ENTRIES",
        DEFAULT_WORKSPACE_UPLOAD_LIMITS.maxEntries,
        1,
        1_000_000,
      ),
    },
  };
}

function optionalIntegerEnv(name: string, min: number, max: number): number | undefined {
  return process.env[name]?.trim() ? integerEnv(name, min, min, max) : undefined;
}

function integerEnv(name: string, fallback: number, min: number, max: number): number {
  const raw = process.env[name]?.trim();
  if (!raw) return fallback;
  const value = Number(raw);
  if (!Number.isInteger(value) || value < min || value > max) {
    throw new Error(`${name} must be an integer from ${min} to ${max}`);
  }
  return value;
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

function optionalBooleanEnv(name: string): boolean | undefined {
  return process.env[name]?.trim() ? booleanEnv(name, false) : undefined;
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

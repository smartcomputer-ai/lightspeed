import { access } from "node:fs/promises";
import { isAbsolute } from "node:path";
import { WorkloadIdentityCredential, type TokenCredential } from "@azure/identity";
import { parse, toClientConfig } from "pg-connection-string";
import type pg from "pg";

const DATABASE_URL = "LIGHTSPEED_PLATFORM_DATABASE_URL";
const AUTH_MODE = "LIGHTSPEED_PLATFORM_DATABASE_AUTH_MODE";
const POSTGRES_SCOPE = "https://ossrdbms-aad.database.windows.net/.default";
const PUBLIC_AUTHORITY = "https://login.microsoftonline.com";
/// Anything longer or with other characters is not a code or identifier, so it is withheld.
const DIAGNOSTIC_VALUE = /^[A-Za-z0-9_-]{1,128}$/;

export interface DatabaseWorkloadIdentity {
  clientId: string;
  tenantId: string;
  tokenFile: string;
  authorityHost: string;
}

/// Reads the auth mode and, when it selects workload identity, the standard Azure
/// settings. Those settings are ignored otherwise, because other software in the
/// pod may set them.
export function databaseWorkloadIdentityFromEnv(
  env: Record<string, string | undefined>,
): DatabaseWorkloadIdentity | null {
  const mode = env[AUTH_MODE] || "password";
  if (mode === "password") return null;
  if (mode !== "workload_identity") {
    throw new Error(`${AUTH_MODE} must be password or workload_identity`);
  }
  const required = (name: string) => {
    const value = env[name];
    if (!value?.trim()) throw new Error(`${name} must be set when ${AUTH_MODE}=workload_identity`);
    return value;
  };
  const tenantId = required("AZURE_TENANT_ID");
  // The tenant becomes a path segment of the token URL.
  if (!/^[A-Za-z0-9][A-Za-z0-9.-]*$/.test(tenantId)) {
    throw new Error("AZURE_TENANT_ID must be a tenant ID or domain name");
  }
  const tokenFile = required("AZURE_FEDERATED_TOKEN_FILE");
  if (!isAbsolute(tokenFile)) throw new Error("AZURE_FEDERATED_TOKEN_FILE must be an absolute path");
  return {
    clientId: required("AZURE_CLIENT_ID"),
    tenantId,
    tokenFile,
    authorityHost: normalizedAuthorityHost(env.AZURE_AUTHORITY_HOST),
  };
}

function normalizedAuthorityHost(value: string | undefined): string {
  if (!value) return PUBLIC_AUTHORITY;
  const invalid = new Error(
    "AZURE_AUTHORITY_HOST must be an HTTPS authority with a host and no credentials, path, query, or fragment",
  );
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw invalid;
  }
  if (
    url.protocol !== "https:" ||
    url.username ||
    url.password ||
    url.pathname !== "/" ||
    url.search ||
    url.hash
  ) {
    throw invalid;
  }
  return url.origin;
}

/// Connection settings that present an Entra token as the login password. `pg`
/// discards a password function when it is combined with a `connectionString`, so
/// the URL is expanded into separate fields.
export function workloadIdentityPoolConfig(
  connectionString: string,
  identity: DatabaseWorkloadIdentity,
  credential: TokenCredential = new WorkloadIdentityCredential({
    clientId: identity.clientId,
    tenantId: identity.tenantId,
    tokenFilePath: identity.tokenFile,
    authorityHost: identity.authorityHost,
    // The authority is configured explicitly, so it needs no discovery request.
    disableInstanceDiscovery: true,
  }),
): pg.PoolConfig {
  let url: URL;
  try {
    url = new URL(connectionString);
  } catch {
    throw new Error(`${DATABASE_URL} could not be parsed`);
  }
  if (!url.username) throw new Error(`${DATABASE_URL} must name the managed identity as its user`);
  if (url.password || url.searchParams.has("password")) {
    throw new Error(`${DATABASE_URL} must not contain a password with workload identity`);
  }
  // The token travels as a cleartext password inside TLS, so the server's identity
  // must be verified before it is sent.
  if (url.searchParams.get("sslmode") !== "verify-full") {
    throw new Error(`${DATABASE_URL} must set sslmode=verify-full with workload identity`);
  }
  const config = toClientConfig(parse(connectionString)) as pg.PoolConfig;
  return { ...config, password: entraTokenPassword(credential, identity.tokenFile) };
}

/// The credential caches and renews its token, so this only reaches Entra when a
/// token is close to expiry. `pg` calls it once for each new connection.
function entraTokenPassword(credential: TokenCredential, tokenFile: string): () => Promise<string> {
  return async () => {
    let failure: unknown;
    try {
      const token = await credential.getToken(POSTGRES_SCOPE);
      if (token) return token.token;
    } catch (error) {
      failure = error;
    }
    throw await tokenFailure(failure, tokenFile);
  };
}

/// Builds a new error rather than wrapping the original, whose message and response
/// body would otherwise stay reachable through logs and `cause`.
async function tokenFailure(failure: unknown, tokenFile: string): Promise<Error> {
  const unreadable = await access(tokenFile).then(
    () => null,
    (error: NodeJS.ErrnoException) => error.code ?? "unreadable",
  );
  if (unreadable) {
    return new Error(
      `could not read the projected token file '${tokenFile}' named by AZURE_FEDERATED_TOKEN_FILE: ${unreadable}`,
    );
  }
  const { message, statusCode, code, errorResponse } = (failure ?? {}) as {
    message?: unknown;
    statusCode?: unknown;
    code?: unknown;
    errorResponse?: { error?: unknown; errorCodes?: unknown; traceId?: unknown; correlationId?: unknown };
  };
  const fields = [
    typeof statusCode === "number" ? `HTTP ${statusCode}` : null,
    diagnostic("code", code),
    // The SDK's other failures begin with a reason code, such as a resolution error.
    diagnostic("reason", typeof message === "string" ? message.split(":", 1)[0] : undefined),
    diagnostic("error", errorResponse?.error),
    ...(Array.isArray(errorResponse?.errorCodes) ? errorResponse.errorCodes : [])
      .filter((value) => typeof value === "number")
      .map((value) => `AADSTS${value}`),
    diagnostic("trace_id", errorResponse?.traceId),
    diagnostic("correlation_id", errorResponse?.correlationId),
  ].filter(Boolean);
  return new Error(`Microsoft Entra token exchange failed: ${fields.join(" ") || "details withheld"}`);
}

function diagnostic(name: string, value: unknown): string | null {
  return typeof value === "string" && DIAGNOSTIC_VALUE.test(value) ? `${name}=${value}` : null;
}

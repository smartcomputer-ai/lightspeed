import { mkdtempSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AccessToken, TokenCredential } from "@azure/identity";
import { WorkloadIdentityCredential } from "@azure/identity";
import { createHttpHeaders, type HttpClient, type PipelineRequest } from "@azure/core-rest-pipeline";
import pg from "pg";
import { afterEach, describe, expect, test, vi } from "vitest";
import { createDb, POOL_TUNING } from "./index.js";
import {
  databaseWorkloadIdentityFromEnv,
  workloadIdentityPoolConfig,
  type DatabaseWorkloadIdentity,
} from "./workload-identity.js";

const URL_OK = "postgres://identity-name@db.example:5432/platform?sslmode=verify-full";
const ASSERTION = "secret-assertion.jwt.value";
const ENV = {
  LIGHTSPEED_PLATFORM_DATABASE_AUTH_MODE: "workload_identity",
  AZURE_CLIENT_ID: "client-id",
  AZURE_TENANT_ID: "tenant-id",
  AZURE_FEDERATED_TOKEN_FILE: "/var/run/token",
};

const directories: string[] = [];
function tempDirectory() {
  const directory = mkdtempSync(join(tmpdir(), "platform-db-"));
  directories.push(directory);
  return directory;
}

afterEach(() => {
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
  vi.restoreAllMocks();
});

function identity(tokenFile = "/var/run/token"): DatabaseWorkloadIdentity {
  return {
    clientId: "client-id",
    tenantId: "tenant-id",
    tokenFile,
    authorityHost: "https://login.example",
  };
}

function fixedCredential(getToken: TokenCredential["getToken"]): TokenCredential {
  return { getToken };
}

async function passwordOf(config: pg.PoolConfig): Promise<string> {
  const password = config.password;
  if (typeof password !== "function") throw new Error("expected a password function");
  return password();
}

describe("databaseWorkloadIdentityFromEnv", () => {
  test("password stays the default and Azure variables do not change it", () => {
    expect(databaseWorkloadIdentityFromEnv({})).toBeNull();
    expect(
      databaseWorkloadIdentityFromEnv({ ...ENV, LIGHTSPEED_PLATFORM_DATABASE_AUTH_MODE: undefined }),
    ).toBeNull();
    expect(
      databaseWorkloadIdentityFromEnv({ ...ENV, LIGHTSPEED_PLATFORM_DATABASE_AUTH_MODE: "password" }),
    ).toBeNull();
  });

  test("requires each identity setting and names it", () => {
    for (const name of ["AZURE_CLIENT_ID", "AZURE_TENANT_ID", "AZURE_FEDERATED_TOKEN_FILE"]) {
      expect(() => databaseWorkloadIdentityFromEnv({ ...ENV, [name]: "" })).toThrow(name);
    }
  });

  test("rejects unknown modes without echoing them", () => {
    const load = () =>
      databaseWorkloadIdentityFromEnv({ LIGHTSPEED_PLATFORM_DATABASE_AUTH_MODE: "secret-mode" });
    expect(load).toThrow("LIGHTSPEED_PLATFORM_DATABASE_AUTH_MODE");
    expect(load).not.toThrow("secret-mode");
  });

  test("validates the token path and tenant", () => {
    expect(() =>
      databaseWorkloadIdentityFromEnv({ ...ENV, AZURE_FEDERATED_TOKEN_FILE: "relative/token" }),
    ).toThrow("absolute");
    for (const tenant of ["../other", "tenant/path"]) {
      expect(() => databaseWorkloadIdentityFromEnv({ ...ENV, AZURE_TENANT_ID: tenant })).toThrow(
        "AZURE_TENANT_ID",
      );
    }
  });

  test("normalizes authorities to one form and defaults to the public cloud", () => {
    expect(databaseWorkloadIdentityFromEnv(ENV)?.authorityHost).toBe(
      "https://login.microsoftonline.com",
    );
    for (const authority of [
      "https://login.microsoftonline.us",
      "https://login.microsoftonline.us/",
      "HTTPS://LOGIN.microsoftonline.us:443/",
    ]) {
      expect(
        databaseWorkloadIdentityFromEnv({ ...ENV, AZURE_AUTHORITY_HOST: authority })?.authorityHost,
      ).toBe("https://login.microsoftonline.us");
    }
    for (const invalid of [
      "http://login.example",
      "login.example",
      "https://login.example/tenant",
      "https://login.example/?x=1",
      "https://user:secret@login.example/",
    ]) {
      expect(() =>
        databaseWorkloadIdentityFromEnv({ ...ENV, AZURE_AUTHORITY_HOST: invalid }),
      ).toThrow("AZURE_AUTHORITY_HOST");
    }
  });
});

describe("workloadIdentityPoolConfig", () => {
  const credential = fixedCredential(async () => ({ token: "t", expiresOnTimestamp: 0 }));

  test("requires a named user, verified TLS, and no password", () => {
    for (const [url, expected] of [
      ["not a url", "could not be parsed"],
      ["postgres://db.example/platform?sslmode=verify-full", "managed identity"],
      ["postgres://user:secret-password@db.example/platform?sslmode=verify-full", "password"],
      ["postgres://user@db.example/platform?sslmode=verify-full&password=secret-password", "password"],
      ["postgres://user@db.example/platform", "verify-full"],
      ["postgres://user@db.example/platform?sslmode=require", "verify-full"],
    ] as const) {
      const build = () => workloadIdentityPoolConfig(url, identity(), credential);
      expect(build).toThrow(expected);
      expect(build).not.toThrow("secret-password");
    }
  });

  test("pg keeps the password function, which a connectionString would discard", () => {
    const config = workloadIdentityPoolConfig(URL_OK, identity(), credential);
    const client = new pg.Client(config);
    expect(typeof client.password).toBe("function");
    expect(client.user).toBe("identity-name");
    expect(client.database).toBe("platform");
    expect(client.host).toBe("db.example");
    expect(client.ssl).toBeTruthy();
    expect(client.ssl).not.toHaveProperty("rejectUnauthorized", false);
    expect(config).not.toHaveProperty("connectionString");
  });

  test("asks for the PostgreSQL scope on each new connection", async () => {
    const getToken = vi.fn(async () => ({ token: "access-token", expiresOnTimestamp: 0 }));
    const config = workloadIdentityPoolConfig(URL_OK, identity(), fixedCredential(getToken));
    expect(await passwordOf(config)).toBe("access-token");
    await passwordOf(config);
    expect(getToken).toHaveBeenCalledTimes(2);
    expect(getToken).toHaveBeenCalledWith("https://ossrdbms-aad.database.windows.net/.default");
  });

  test("an unreadable token file names the stage and path", async () => {
    const tokenFile = join(tempDirectory(), "missing");
    const failing = fixedCredential(async () => {
      throw new Error(`failed with ${ASSERTION}`);
    });
    const config = workloadIdentityPoolConfig(URL_OK, identity(tokenFile), failing);
    const error: Error = await passwordOf(config).catch((caught) => caught);
    expect(error.message).toContain(tokenFile);
    expect(error.message).toContain("AZURE_FEDERATED_TOKEN_FILE");
    expect(error.message).toContain("ENOENT");
    expect(error.message).not.toContain("secret-");
    expect(error.cause).toBeUndefined();
  });

  test("a failed exchange keeps identifiers and drops the original error", async () => {
    const tokenFile = join(tempDirectory(), "token");
    writeFileSync(tokenFile, ASSERTION);
    const failing = fixedCredential(async () => {
      throw Object.assign(new Error(`rejected ${ASSERTION}`), {
        statusCode: 401,
        errorResponse: {
          error: "invalid_client",
          errorDescription: `bad assertion ${ASSERTION}`,
          errorCodes: [70021],
          traceId: "trace-1",
          correlationId: "header.payload.signature",
        },
      });
    });
    const config = workloadIdentityPoolConfig(URL_OK, identity(tokenFile), failing);
    const error: Error = await passwordOf(config).catch((caught) => caught);
    expect(error.message).toContain("Microsoft Entra token exchange failed");
    expect(error.message).toContain("HTTP 401");
    expect(error.message).toContain("error=invalid_client");
    expect(error.message).toContain("AADSTS70021");
    expect(error.message).toContain("trace_id=trace-1");
    // Dotted values such as JWTs are not identifiers, even in an allowlisted field.
    expect(error.message).not.toContain("header.payload.signature");
    expect(error.message).not.toContain("secret-");
    expect(error.cause).toBeUndefined();
  });
});

/// Runs the real Azure credential against a scripted transport, so the wire content
/// and caching are the SDK's own.
describe("WorkloadIdentityCredential wiring", () => {
  function entra() {
    const exchanges: Record<string, string>[] = [];
    let issued = 0;
    const httpClient: HttpClient = {
      async sendRequest(request: PipelineRequest) {
        const respond = (status: number, body: unknown) => ({
          request,
          status,
          headers: createHttpHeaders({ "content-type": "application/json" }),
          bodyAsText: JSON.stringify(body),
        });
        if (new URL(request.url).pathname.endsWith("/oauth2/v2.0/token")) {
          exchanges.push(Object.fromEntries(new URLSearchParams(String(request.body))));
          issued += 1;
          return respond(200, {
            token_type: "Bearer",
            expires_in: 3600,
            access_token: `access-token-${issued}`,
          });
        }
        return respond(200, {
          token_endpoint: "https://login.example/tenant-id/oauth2/v2.0/token",
          authorization_endpoint: "https://login.example/tenant-id/oauth2/v2.0/authorize",
          issuer: "https://login.example/tenant-id/v2.0",
          jwks_uri: "https://login.example/tenant-id/discovery/v2.0/keys",
          end_session_endpoint: "https://login.example/tenant-id/oauth2/v2.0/logout",
        });
      },
    };
    return { exchanges, httpClient };
  }

  function credentialFor(tokenFile: string, httpClient: HttpClient) {
    return new WorkloadIdentityCredential({
      clientId: "client-id",
      tenantId: "tenant-id",
      tokenFilePath: tokenFile,
      authorityHost: "https://login.example",
      disableInstanceDiscovery: true,
      httpClient,
    });
  }

  test("presents the projected token and reuses the access token until it nears expiry", async () => {
    const tokenFile = join(tempDirectory(), "token");
    writeFileSync(tokenFile, ASSERTION);
    const { exchanges, httpClient } = entra();
    const config = workloadIdentityPoolConfig(
      URL_OK,
      identity(tokenFile),
      credentialFor(tokenFile, httpClient),
    );

    expect(await passwordOf(config)).toBe("access-token-1");
    expect(await passwordOf(config)).toBe("access-token-1");

    expect(exchanges).toHaveLength(1);
    expect(exchanges[0]).toMatchObject({
      grant_type: "client_credentials",
      client_id: "client-id",
      client_assertion: ASSERTION,
      client_assertion_type: "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
    });
    expect(exchanges[0]?.scope).toContain("https://ossrdbms-aad.database.windows.net/.default");
  });

  test("a new exchange reads a rotated token file", async () => {
    const directory = tempDirectory();
    const tokenFile = join(directory, "token");
    writeFileSync(tokenFile, ASSERTION);
    const { exchanges, httpClient } = entra();
    const credential = credentialFor(tokenFile, httpClient);
    const token = (value: AccessToken | null) => value?.token;

    expect(token(await credential.getToken("https://first.example/.default"))).toBeDefined();
    const staged = join(directory, "staged");
    writeFileSync(staged, "secret-rotated-assertion");
    renameSync(staged, tokenFile);
    const realNow = Date.now();
    // The SDK reuses the file's contents for five minutes.
    vi.spyOn(Date, "now").mockReturnValue(realNow + 6 * 60_000);
    expect(token(await credential.getToken("https://second.example/.default"))).toBeDefined();

    expect(exchanges.map((exchange) => exchange.client_assertion)).toEqual([
      ASSERTION,
      "secret-rotated-assertion",
    ]);
  });
});

describe("token failure reasons", () => {
  test("keep a leading SDK reason code and nothing after it", async () => {
    const tokenFile = join(tempDirectory(), "token");
    writeFileSync(tokenFile, ASSERTION);
    const failing = fixedCredential(async () => {
      throw new Error(`endpoints_resolution_error: details ${ASSERTION}`);
    });
    const config = workloadIdentityPoolConfig(URL_OK, identity(tokenFile), failing);
    const error: Error = await passwordOf(config).catch((caught) => caught);
    expect(error.message).toContain("reason=endpoints_resolution_error");
    expect(error.message).not.toContain("secret-");
  });
});

describe("createDb", () => {
  test("applies one tuned pool in password mode and survives losing an idle connection", async () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    const { pool } = createDb("postgres://user:password@db.example/platform");
    expect(pool.options).toMatchObject(POOL_TUNING);
    expect(pool.options.connectionString).toBe("postgres://user:password@db.example/platform");

    // Without a listener, EventEmitter throws this error at the process.
    expect(() => pool.emit("error", new Error("connection reset"), {} as pg.PoolClient)).not.toThrow();
    expect(consoleError).toHaveBeenCalledWith(expect.stringContaining("connection reset"));
    await pool.end();
  });

  test("applies the same tuning with workload identity", async () => {
    const { pool } = createDb(URL_OK, identity());
    expect(pool.options).toMatchObject(POOL_TUNING);
    expect(typeof pool.options.password).toBe("function");
    await pool.end();
  });
});

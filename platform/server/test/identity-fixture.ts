import { readFile } from "node:fs/promises";
import { PGlite } from "@electric-sql/pglite";
import { drizzle } from "drizzle-orm/pglite";
import { schema, type Db } from "@lightspeed/platform-db";
import { createAuth } from "../src/auth.js";
import { buildApp } from "../src/api.js";
import { bootstrapAdmin } from "../src/bootstrap.js";
import type { ServerEnv } from "../src/env.js";

export const identityEnv: ServerEnv = {
  databaseUrl: "postgres://unused", authSecret: "identity-test-secret-at-least-thirty-two-characters",
  baseUrl: "http://localhost:3030", trustedOrigins: [], port: 3030,
  adminEmail: "emergency@example.test", adminPassword: "emergency-test-password",
  github: null, lightspeedApiUrl: null, configuratorMcpUrl: null,
  configuratorMcpAllowPrivateNetwork: false, configuratorMcpInternalTrustedHeader: false,
  channelsHealthUrls: [], devEnvdEndpoint: null,
  oidc: {
    issuer: "https://identity.example.test", discoveryUrl: "https://identity.example.test/.well-known/openid-configuration",
    clientId: "lightspeed", clientSecret: "synthetic-client-secret", scopes: ["openid", "profile", "email"],
    groupsClaim: "groups", userGroup: "lightspeed-users", adminGroup: "lightspeed-admins",
    claimsToken: "id", resource: "urn:lightspeed", sessionMaxAgeSeconds: 28800, autoSignIn: true,
  },
};

export async function identityFixture(env: ServerEnv = identityEnv) {
  const postgres = new PGlite();
  const journal = JSON.parse(await readFile(new URL("../../db/migrations/meta/_journal.json", import.meta.url), "utf8"));
  for (const entry of journal.entries) await postgres.exec(await readFile(new URL(`../../db/migrations/${entry.tag}.sql`, import.meta.url), "utf8"));
  const db = drizzle(postgres, { schema, casing: "snake_case" }) as unknown as Db;
  await bootstrapAdmin(db, env);
  const auth = createAuth(db, env);
  const app = buildApp({ db, auth, env, pool: {} as never });
  const request = (path: string, body?: unknown, cookie = "", method = body === undefined ? "GET" : "POST") => app.request(new Request(new URL(path, env.baseUrl), {
    method, headers: { origin: env.baseUrl, cookie, ...(body === undefined ? {} : { "content-type": "application/json" }) },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }));
  return { db, postgres, auth, app, request, env };
}

export function responseCookies(response: Response): string {
  return response.headers.getSetCookie().map((cookie) => cookie.split(";")[0]).join("; ");
}

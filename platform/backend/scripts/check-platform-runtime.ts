/** Live Platform → runtime and CLI authentication exercise. Requires a running
 * local full stack, seeded local users, and a built Rust CLI. Creates isolated
 * named fixtures in the development deployment; never prints credentials. */
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { LightspeedClient } from "@lightspeed-ai/sdk";
import { Client as McpClient, StreamableHTTPClientTransport } from "@modelcontextprotocol/client";
import pg from "pg";
import { startConfigurator } from "../../configurator-mcp/src/transport.js";

const base = process.env.LIGHTSPEED_PLATFORM_LIVE_URL;
assert(base, "LIGHTSPEED_PLATFORM_LIVE_URL is required (the running local Platform origin)");
const origin = new URL(base).origin;
const root = fileURLToPath(new URL("../../../", import.meta.url));
const binary = process.env.LIGHTSPEED_LIVE_CLI ?? path.join(root, "target/debug/lightspeed");
const configDir = process.env.LIGHTSPEED_LIVE_CONNECTIONS_DIR ?? await mkdtemp(path.join(tmpdir(), "lightspeed-live-connections-"));
const password = process.env.LIGHTSPEED_PLATFORM_LIVE_PASSWORD ?? "lightspeed-dev-password";
const prefix = `live-${crypto.randomUUID().slice(0, 8)}`;
await mkdir(configDir, { recursive: true, mode: 0o700 });

async function request(route: string, options: { cookie?: string; token?: string; method?: string; body?: unknown } = {}) {
  return fetch(new URL(route, origin), {
    method: options.method ?? (options.body === undefined ? "GET" : "POST"),
    headers: { origin, ...(options.cookie ? { cookie: options.cookie } : {}), ...(options.token ? { authorization: `Bearer ${options.token}` } : {}), ...(options.body === undefined ? {} : { "content-type": "application/json" }) },
    ...(options.body === undefined ? {} : { body: JSON.stringify(options.body) }),
    redirect: "error", signal: AbortSignal.timeout(60_000),
  });
}
async function expectStatus(response: Response, expected: number) {
  assert.equal(response.status, expected, `${response.url}: expected HTTP ${expected}, received ${response.status}`);
  return response;
}
async function login(email: string) {
  const response = await expectStatus(await request("/api/auth/sign-in/email", { body: { email, password } }), 200);
  const cookie = response.headers.getSetCookie().map(value => value.split(";")[0]).join("; ");
  assert(cookie, "browser login must return a cookie");
  const me = await (await expectStatus(await request("/api/ui/me", { cookie }), 200)).json() as { user: { id: string } };
  return { cookie, id: me.user.id as string };
}
const admin = await login(process.env.LIGHTSPEED_PLATFORM_LIVE_ADMIN_EMAIL ?? "admin@lightspeed.dev");
const operator = await login(process.env.LIGHTSPEED_PLATFORM_LIVE_USER_EMAIL ?? "operator@lightspeed.dev");
const browserClient = (cookie: string) => new LightspeedClient({ endpoint: origin, headers: { cookie, origin } });
const adminApi = browserClient(admin.cookie);
const operatorApi = browserClient(operator.cookie);
const organization = (await adminApi.platform.createOrganization({ name: prefix, handle: prefix, initialAdminUserId: admin.id, defaultProtected: false })).organization;
await adminApi.platform.putOrganizationMember({ organizationId: organization.id, userId: operator.id, role: "creator" });
const writer = await operatorApi.platform.createPersonalApiKey({ name: `${prefix}-writer`, restrictions: { organizationIds: [organization.id] } });
const writerApi = new LightspeedClient({ endpoint: origin, token: writer.secret });
const created = [];
for (const suffix of ["a", "b"]) {
  const universe = (await writerApi.platform.createUniverse({ organizationId: organization.id, name: `${prefix}-${suffix}`, handle: `${prefix}-${suffix}`, protected: false })).universe;
  created.push(universe);
  assert.equal(universe.role, "owner");
  await adminApi.platform.putUniverseGrant({ universe: universe.id, principal: { kind: "user", id: admin.id }, role: "owner" });
}
const [a, b] = created;
assert(a && b);
const readOnly = await operatorApi.platform.createPersonalApiKey({ name: `${prefix}-sdk`, restrictions: { readOnly: true, organizationIds: [organization.id], universeIds: [a.lightspeedUniverseId] } });
const scoped = new LightspeedClient({ endpoint: origin, token: readOnly.secret, universe: a.lightspeedUniverseId });
assert.equal((await scoped.initialize()).runtime, "platform");
await scoped.listProfiles({});
// Exercise the generated MCP binary-read adapter through both real HTTP hops.
const binaryFixture = new Uint8Array([0, 255, 128, 10, 13, 65]);
const blobRef = await new LightspeedClient({ endpoint: origin, token: writer.secret, universe: a.lightspeedUniverseId }).putBlob(binaryFixture);
const configurator = await startConfigurator({ config: {
  bindHost: "127.0.0.1", bindPort: 0, allowedHosts: ["127.0.0.1"], allowedOrigins: [],
  apiEndpoint: origin, authMode: "authenticated", maxBodyBytes: 1024 * 1024,
  upstreamTimeoutMs: 30_000, shutdownTimeoutMs: 1_000,
} });
const mcp = new McpClient({ name: "live-platform-validation", version: "1" }, { versionNegotiation: { mode: "auto" } });
try {
  const transport = new StreamableHTTPClientTransport(new URL(`${configurator.url}/mcp`), { requestInit: { headers: { authorization: `Bearer ${readOnly.secret}` } } });
  await mcp.connect(transport as Parameters<typeof mcp.connect>[0]);
  const result = await mcp.callTool({ name: "lightspeed_blobs_read", arguments: { blobRef } });
  assert.equal(result.isError, undefined);
  assert.deepEqual(result.content, [{ type: "text", text: JSON.stringify({ encoding: "base64", data: Buffer.from(binaryFixture).toString("base64"), bytes: binaryFixture.byteLength }) }]);
} finally { await mcp.close(); await configurator.close(); }
await expectStatus(await request(`/api/v1/universes/${a.lightspeedUniverseId}/blobs/batch`, { token: readOnly.secret, body: { blobs: [{ bytesBase64: "bGl2ZQ==" }] } }), 403);
await expectStatus(await request(`/api/v1/universes/${b.lightspeedUniverseId}/profiles`, { token: readOnly.secret }), 404);
await expectStatus(await request("/api/v1/platform/credentials", { token: readOnly.secret, body: { name: "must-not-mint" } }), 403);
await expectStatus(await request("/api/ui/me", { token: readOnly.secret, cookie: operator.cookie }), 401);
await expectStatus(await request("/api/v1/connection", { token: "invalid", cookie: operator.cookie }), 401);
const scopedAdmin = await adminApi.platform.createPersonalApiKey({ name: `${prefix}-admin`, restrictions: { readOnly: true, universeIds: [a.lightspeedUniverseId] } });
await expectStatus(await request(`/api/v1/universes/${a.lightspeedUniverseId}/profiles`, { token: scopedAdmin.secret }), 200);
await expectStatus(await request(`/api/v1/universes/${b.lightspeedUniverseId}/profiles`, { token: scopedAdmin.secret }), 404);
await expectStatus(await request(`/api/v1/universes/${a.lightspeedUniverseId}/profiles/forbidden`, { token: scopedAdmin.secret, method: "PUT", body: {} }), 403);
await adminApi.platform.revokePersonalApiKey({ keyId: scopedAdmin.credential.id });


// Current membership and roles constrain an existing key on its next request.
await adminApi.platform.putUniverseGrant({ universe: a.id, principal: { kind: "user", id: operator.id }, role: "viewer" });
await expectStatus(await request(`/api/v1/universes/${a.lightspeedUniverseId}/profiles/forbidden`, { token: writer.secret, method: "PUT", body: {} }), 403);
await adminApi.platform.putUniverseGrant({ universe: a.id, principal: { kind: "user", id: operator.id }, role: "owner" });
await adminApi.platform.removeOrganizationMember({ organizationId: organization.id, userId: operator.id });
await expectStatus(await request(`/api/v1/universes/${a.lightspeedUniverseId}/profiles`, { token: writer.secret }), 404);
await adminApi.platform.putOrganizationMember({ organizationId: organization.id, userId: operator.id, role: "creator" });
// Removing the member clears their grants. Restore them for the SDK upload and isolation checks.
for (const universe of created) {
  await adminApi.platform.putUniverseGrant({ universe: universe.id, principal: { kind: "user", id: operator.id }, role: "owner" });
}

// ServiceUser creation authority is organization-bound and stops when disabled.
const serviceUser = (await adminApi.platform.createServiceUser({ organizationId: organization.id, name: `${prefix}-app`, role: "creator" })).serviceUser;
const serviceUserKey = await adminApi.platform.createServiceUserApiKey({ organizationId: organization.id, serviceUserId: serviceUser.id, name: "live service user", restrictions: {} });
const serviceUserApi = new LightspeedClient({ endpoint: origin, token: serviceUserKey.secret });
const serviceUserUniverse = (await serviceUserApi.platform.createUniverse({ organizationId: organization.id, name: `${prefix}-app`, handle: `${prefix}-app`, protected: false })).universe;
assert.equal(serviceUserUniverse.role, "owner");
assert.equal(serviceUserUniverse.createdBy, `serviceUser:${serviceUser.id}`);
const otherOrganization = (await adminApi.platform.createOrganization({ name: `${prefix}-other`, handle: `${prefix}-other`, initialAdminUserId: admin.id })).organization;
await expectStatus(await request(`/api/v1/platform/organizations/${otherOrganization.id}`, { token: serviceUserKey.secret }), 403);

await expectStatus(await request(`/api/v1/universes/${a.lightspeedUniverseId}/profiles`, { token: serviceUserKey.secret }), 404);
await adminApi.platform.updateServiceUser({ organizationId: organization.id, serviceUserId: serviceUser.id, enabled: false });
await expectStatus(await request("/api/v1/connection", { token: serviceUserKey.secret }), 401);

function cli(args: string[], input?: string, onOutput?: (output: string) => void, expectedExit = 0): Promise<string> {
  const env: NodeJS.ProcessEnv = { ...process.env, LIGHTSPEED_CONFIG_DIR: configDir };
  for (const key of Object.keys(env)) if (key.startsWith("LIGHTSPEED_API_") || key === "LIGHTSPEED_UNIVERSE" || key === "LIGHTSPEED_ORGANIZATION") delete env[key];
  return new Promise((resolve, reject) => {
    const child = spawn(binary, args, { env, stdio: ["pipe", "pipe", "pipe"] });
    let output = "";
    let error = "";
    const timeout = setTimeout(() => { child.kill("SIGTERM"); reject(new Error(`CLI ${args.slice(0, 2).join(" ")} timed out`)); }, 90_000);
    child.stdout.on("data", chunk => { output += String(chunk); onOutput?.(output); });
    child.stderr.on("data", chunk => { error += String(chunk); });
    child.on("error", reject);
    child.on("close", code => { clearTimeout(timeout); code === expectedExit ? resolve(output) : reject(new Error(`CLI ${args.slice(0, 2).join(" ")} failed (${code}): ${error}`)); });
    child.stdin.end(input);
  });
}
async function loginConnection(name: string) {
  let approval: Promise<void> | undefined;
  await cli(["connect", "login", "--url", origin, "--name", name, "--no-browser"], undefined, output => {
    const code = output.match(/Enter code: ([^\r\n]+)/)?.[1];
    if (!code || approval) return;
    approval = (async () => {
      await expectStatus(await request(`/api/auth/device?user_code=${encodeURIComponent(code)}`, { cookie: operator.cookie }), 200);
      await expectStatus(await request("/api/auth/device/approve", { cookie: operator.cookie, body: { userCode: code } }), 200);
    })();
    // Keep the rejection handled while the CLI is waiting; rethrow below.
    void approval.catch(() => undefined);
  });
  assert(approval, "CLI must print a device approval code");
  await approval;
}
await loginConnection("sdk-session");
const connections = ["sdk-session", "sdk-key"];
if (process.env.LIGHTSPEED_PLATFORM_LIVE_MODEL_CONNECTION === "true") {
  await loginConnection("sdk-model");
  connections.push("sdk-model");
}
await cli(["connect", "add", "sdk-key", "--url", origin, "--token-stdin"], readOnly.secret);
for (const name of connections) {
  await cli(["--connection", name, "platform", "organization", "use", organization.id]);
  await cli(["--connection", name, "universe", "use", a.lightspeedUniverseId]);
  const discovery = JSON.parse(await cli(["--connection", name, "api", "initialize"]));
  assert.equal(discovery.runtime, "platform");
  await cli(["--connection", name, "api", "profiles/list"]);
}
const document = path.join(configDir, "declared-universe.json");
await writeFile(document, JSON.stringify({ schemaVersion: 1, kind: "universe", handle: `${prefix}-declared`, displayName: "Live declared universe", metadata: { validation: prefix }, protected: true }), { mode: 0o600 });
await cli(["--connection", "sdk-session", "config", "diff", "-f", document], undefined, undefined, 2);
await cli(["--connection", "sdk-session", "config", "apply", "-f", document]);
await cli(["--connection", "sdk-session", "config", "diff", "-f", document]);
await cli(["--connection", "sdk-session", "config", "apply", "-f", document]);
await writeFile(path.join(configDir, "other-universe.txt"), b.lightspeedUniverseId, { mode: 0o600 });
// Leave the dedicated credentials for the SDK live suites, which revoke
// them. All other issued keys are revoked here, including the disabled app key.
await operatorApi.platform.revokePersonalApiKey({ keyId: writer.credential.id });
await expectStatus(await request("/api/v1/connection", { token: writer.secret }), 401);
await adminApi.platform.revokeServiceUserApiKey({ organizationId: organization.id, serviceUserId: serviceUser.id, keyId: serviceUserKey.credential.id });
const saved = JSON.parse(await readFile(path.join(configDir, "connections.json"), "utf8"));
assert.equal(saved.connections["sdk-session"].credentialKind, "platformSession");
assert.equal(saved.connections["sdk-key"].credentialKind, "platformApiKey");
if (process.env.LIGHTSPEED_PLATFORM_LIVE_RENEWAL_DATABASE_URL) {
  const token = (await readFile(saved.connections["sdk-session"].credentialFile, "utf8")).trim();
  const pool = new pg.Pool({ connectionString: process.env.LIGHTSPEED_PLATFORM_LIVE_RENEWAL_DATABASE_URL });
  try {
    const aged = await pool.query(`UPDATE session SET created_at = now() - interval '2 days',
      updated_at = now() - interval '2 days', expires_at = now() + interval '1 hour'
      WHERE token = $1 AND session_kind = 'cli'`, [token]);
    assert.equal(aged.rowCount, 1, "renewal fixture must identify exactly one CLI session");
    await writeFile(path.join(configDir, "renewal-required"), "1", { mode: 0o600 });
  } finally { await pool.end(); }
}
console.log(`Live Platform/runtime and CLI checks passed; SDK connection fixtures: ${configDir}`);

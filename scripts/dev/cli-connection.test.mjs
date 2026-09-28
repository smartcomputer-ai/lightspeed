import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { prepareCliConnection } from "./cli-connection.mjs";

function fixture(t) {
  const root = mkdtempSync(path.join(tmpdir(), "lightspeed-connection-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const calls = [];
  const keys = new Map();
  let sequence = 0;
  const run = async (_name, _command, args, env) => {
    calls.push({ args, env });
    if (args.includes("universe")) return { stdout: "" };
    let secret = env.LIGHTSPEED_BOOTSTRAP_API_KEY;
    if (secret && keys.get(secret) === "revoked") throw new Error("revoked");
    if (args.includes("--require-existing")) assert.equal(keys.get(secret), "active", "restart must use an active key");
    if (!secret) secret = `lsk_test_${++sequence}`;
    keys.set(secret, "active");
    return { stdout: JSON.stringify({ secret }) };
  };
  const env = { LIGHTSPEED_AUTH_MODE: "authenticated", LIGHTSPEED_API_URL: "http://127.0.0.1:18080/rpc", LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001" };
  return { root, env, run, calls, keys };
}
test("runtime and full profiles share a persistent CLI key and separate Platform key", async t => {
  const f = fixture(t);
  const first = await prepareCliConnection({ ...f, full: false });
  const universeCall = f.calls.find(c => c.args.includes("universe"));
  assert.deepEqual(universeCall.args.slice(-4), ["--universe-id", f.env.LIGHTSPEED_PG_UNIVERSE_ID, "--slug", "development"]);
  const handoff = JSON.parse(readFileSync(first.handoff));
  const secret = readFileSync(handoff.credentialFile, "utf8");
  assert.equal(statSync(handoff.credentialFile).mode & 0o777, 0o600);
  assert.equal(statSync(path.dirname(handoff.credentialFile)).mode & 0o777, 0o700);
  assert.ok(!readFileSync(first.handoff, "utf8").includes(secret));
  const second = await prepareCliConnection({ ...f, full: true });
  assert.equal(readFileSync(handoff.credentialFile, "utf8"), secret);
  assert.notEqual(second.platformSecret, secret);
  assert.ok(f.calls.some(c => c.args.includes("--require-existing")));
  f.keys.set(secret, "revoked");
  await assert.rejects(prepareCliConnection({ ...f, full: true }), /revoked/);
  assert.equal(f.keys.size, 2, "revocation does not trigger another key");
});
test("key bootstrap opt-out preserves universe setup and requires existing service credentials", async t => {
  const f = fixture(t);
  const result = await prepareCliConnection({ ...f, full: false, noBootstrap: true });
  assert.equal(result.handoff, null);
  assert.equal(f.keys.size, 0);
  assert.ok(f.calls.every(c => c.args.includes("universe")));
  await assert.rejects(prepareCliConnection({ ...f, full: true, noBootstrap: true }), /LIGHTSPEED_PLATFORM_API_KEY/);
  const supplied = await prepareCliConnection({ ...f, env: { ...f.env, LIGHTSPEED_PLATFORM_API_KEY: "external" }, full: true, noBootstrap: true });
  assert.equal(supplied.platformSecret, "external");
  assert.equal(f.keys.size, 0);
});
test("single mode writes a credential-free handoff", async t => {
  const f = fixture(t);
  const result = await prepareCliConnection({ ...f, env: { ...f.env, LIGHTSPEED_AUTH_MODE: "single" }, full: false });
  const handoff = JSON.parse(readFileSync(result.handoff));
  assert.equal(handoff.single, true);
  assert.equal(handoff.credentialFile, undefined);
  assert.equal(handoff.universe, undefined);
  assert.equal(f.keys.size, 0);
});
test("supplied bootstrap input is registered once and missing local credentials require repair", async t => {
  const f = fixture(t);
  const result = await prepareCliConnection({ ...f, env: { ...f.env, LIGHTSPEED_BOOTSTRAP_API_KEY: "lsk_supplied" }, full: true });
  const handoff = JSON.parse(readFileSync(result.handoff));
  assert.equal(readFileSync(handoff.credentialFile, "utf8"), "lsk_supplied");
  assert.notEqual(result.platformSecret, "lsk_supplied");
  rmSync(handoff.credentialFile);
  await assert.rejects(prepareCliConnection({ ...f, full: false }), /missing/);
});

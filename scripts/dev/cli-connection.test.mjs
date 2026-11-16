import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { prepareCliConnection, processEnvironment } from "./cli-connection.mjs";

function fixture(t) {
  const root = mkdtempSync(path.join(tmpdir(), "lightspeed-connection-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const calls = [];
  const run = async (_name, _command, args) => { calls.push(args); return { stdout: "" }; };
  const env = { LIGHTSPEED_AUTH_MODE: "authenticated", LIGHTSPEED_API_URL: "http://127.0.0.1:18080/api/v1", LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001" };
  return { root, env, run, calls };
}
const supplied = "lsr_supplied_012345678901234567890123456789";

test("tokens persist privately without any API-key provisioning", async t => {
  const f = fixture(t);
  const first = await prepareCliConnection({ ...f, full: true });
  const handoff = JSON.parse(readFileSync(first.handoff));
  const secret = readFileSync(handoff.credentialFile, "utf8");
  const platform = readFileSync(first.tokenEnvironment.LIGHTSPEED_PLATFORM_API_TOKEN_FILE, "utf8");
  assert.ok(secret.length >= 32);
  assert.notEqual(secret, platform);
  assert.equal(statSync(handoff.credentialFile).mode & 0o777, 0o600);
  assert.equal(statSync(path.dirname(handoff.credentialFile)).mode & 0o777, 0o700);
  assert.ok(!readFileSync(first.handoff, "utf8").includes(secret));
  const second = await prepareCliConnection({ ...f, full: true });
  assert.equal(readFileSync(handoff.credentialFile, "utf8"), secret);
  assert.equal(readFileSync(second.tokenEnvironment.LIGHTSPEED_PLATFORM_API_TOKEN_FILE, "utf8"), platform);
  assert.ok(f.calls.every(args => args.includes("universe") && !args.includes("api-key")));
});

test("supplied inline and mounted tokens become private files and can be replaced", async t => {
  const f = fixture(t);
  const mounted = path.join(f.root, "mounted");
  writeFileSync(mounted, supplied + "\n");
  const env = { ...f.env, LIGHTSPEED_API_TOKEN_CLI_FILE: mounted, LIGHTSPEED_PLATFORM_API_TOKEN: supplied + "platform" };
  const result = await prepareCliConnection({ ...f, env, full: true });
  assert.equal(readFileSync(result.tokenEnvironment.LIGHTSPEED_API_TOKEN_CLI_FILE, "utf8"), supplied);
  assert.equal(env.LIGHTSPEED_PLATFORM_API_TOKEN, undefined);
  const replacement = supplied + "replacement";
  await prepareCliConnection({ ...f, env: { ...f.env, LIGHTSPEED_API_TOKEN_CLI: replacement }, full: false });
  assert.equal(readFileSync(result.tokenEnvironment.LIGHTSPEED_API_TOKEN_CLI_FILE, "utf8"), replacement);
});

test("default token keeps its default name without a duplicate named token", async t => {
  const f = fixture(t);
  const env = { ...f.env, LIGHTSPEED_API_TOKEN: supplied };
  const result = await prepareCliConnection({ ...f, env, full: false });
  assert.equal(env.LIGHTSPEED_API_TOKEN, undefined);
  assert.equal(env.LIGHTSPEED_API_TOKEN_CLI_FILE, undefined);
  assert.equal(readFileSync(result.tokenEnvironment.LIGHTSPEED_API_TOKEN_FILE, "utf8"), supplied);
});

test("token bootstrap opt-out requires supplied or saved tokens", async t => {
  const f = fixture(t);
  await assert.rejects(prepareCliConnection({ ...f, full: false, noBootstrap: true }), /No cli runtime token/);
  await prepareCliConnection({ ...f, env: { ...f.env, LIGHTSPEED_API_TOKEN_CLI: supplied }, full: false, noBootstrap: true });
  await prepareCliConnection({ ...f, full: false, noBootstrap: true });
  await assert.rejects(prepareCliConnection({ ...f, full: true, noBootstrap: true }), /No platform runtime token/);
});

test("ambiguous and short configured tokens fail without printing secrets", async t => {
  const f = fixture(t);
  await assert.rejects(prepareCliConnection({ ...f, env: { ...f.env, LIGHTSPEED_API_TOKEN_CLI: supplied, LIGHTSPEED_API_TOKEN_CLI_FILE: "ignored" } }), /not both/);
  await assert.rejects(prepareCliConnection({ ...f, env: { ...f.env, LIGHTSPEED_API_TOKEN_CLI: "short" } }), /at least 32/);
});

test("single mode writes a credential-free handoff with its selected universe", async t => {
  const f = fixture(t);
  const result = await prepareCliConnection({ ...f, env: { ...f.env, LIGHTSPEED_AUTH_MODE: "single" }, full: false });
  const handoff = JSON.parse(readFileSync(result.handoff));
  assert.equal(handoff.single, true);
  assert.equal(handoff.credentialFile, undefined);
  assert.equal(handoff.universe, f.env.LIGHTSPEED_PG_UNIVERSE_ID);
  assert.deepEqual(result.tokenEnvironment, {});
});

test("model seeding selects universes explicitly and preserves changed or cleared defaults", async () => {
  const { seedDevelopmentModelDefaults } = await import("./cli-connection.mjs");
  const calls = [];
  const env = { LIGHTSPEED_AUTH_MODE: "authenticated", LIGHTSPEED_PLATFORM_API_TOKEN: "lsr_fixture_01234567890123456789012", LIGHTSPEED_API_URL: "http://localhost/api/v1", LIGHTSPEED_PG_UNIVERSE_ID: "development", LIGHTSPEED_PLATFORM_DEV_SEED: "true" };
  await seedDevelopmentModelDefaults({ env, full: true, fetch: async (url, init) => {
    const universe = new URL(url).pathname.split("/")[4];
    calls.push({ universe, method: init.method, params: init.body ? JSON.parse(init.body) : {} });
    assert.equal(init.headers["x-lightspeed-universe"], undefined);
    assert.equal(init.headers.authorization, "Bearer lsr_fixture_01234567890123456789012");
    assert.equal(init.redirect, "error");
    const defaults = { revision: universe === "development" ? 3 : 0, agentRun: null, speechToText: null };
    return Response.json({ defaults });
  } });
  assert.deepEqual(calls.map(c => [c.universe, c.method]), [
    ["development", "GET"],
    ["6c696768-7473-4065-8064-000000000010", "GET"],
    ["6c696768-7473-4065-8064-000000000010", "PUT"],
  ]);
  assert.equal(calls[2].params.expectedRevision, 0);
  assert.equal(calls[2].params.slot, "agentRun");
});

test("single-mode model seeding sends no authority headers and tolerates a concurrent update", async () => {
  const { seedDevelopmentModelDefaults } = await import("./cli-connection.mjs");
  const methods = [];
  await seedDevelopmentModelDefaults({
    env: { LIGHTSPEED_AUTH_MODE: "single", LIGHTSPEED_API_URL: "http://localhost/api/v1", LIGHTSPEED_PG_UNIVERSE_ID: "development" },
    full: false,
    fetch: async (url, init) => {
      assert.equal(init.headers.authorization, undefined);
      assert.equal(init.headers["x-lightspeed-universe"], undefined);
      assert.match(url, /\/universes\/development\/models\/defaults$/);
      methods.push(init.method);
      return init.method === "GET" ? Response.json({ defaults: { revision: 0 } })
        : Response.json({ kind: "conflict" }, { status: 409 });
    },
  });
  assert.deepEqual(methods, ["GET", "PUT"]);
});


test("only trusted runtime and Platform processes inherit their API token sources", () => {
  const env = { PATH: "/bin", LIGHTSPEED_API_TOKEN: supplied, LIGHTSPEED_API_TOKEN_OPERATOR_FILE: "/operator", LIGHTSPEED_PLATFORM_API_TOKEN_FILE: "/platform" };
  for (const role of ["envd", "web", "demo", "docs", "configurator", "connectors"]) {
    assert.deepEqual(processEnvironment(env, role), { PATH: "/bin" });
  }
  assert.deepEqual(processEnvironment(env, "runtime"), { PATH: "/bin", LIGHTSPEED_API_TOKEN: supplied, LIGHTSPEED_API_TOKEN_OPERATOR_FILE: "/operator" });
  assert.deepEqual(processEnvironment(env, "platform"), { PATH: "/bin", LIGHTSPEED_PLATFORM_API_TOKEN_FILE: "/platform" });
  assert.equal(env.LIGHTSPEED_API_TOKEN, supplied, "the supervisor retains its configuration");
});


for (const source of ["inline", "mounted", "saved"]) {
  test(`${source} token validation never trims invalid whitespace or non-ASCII characters`, async t => {
    const invalid = [" " + supplied, supplied + " ", supplied + "\t", supplied + "\r", supplied + "é", supplied + "\u007f", supplied + "\n\n", supplied + "\r\n\r\n"];
    if (source === "inline") invalid.push(supplied + "\n", supplied + "\r\n");
    for (const secret of invalid) {
      const f = fixture(t);
      const env = { ...f.env };
      if (source === "inline") env.LIGHTSPEED_API_TOKEN_CLI = secret;
      else if (source === "mounted") {
        const file = path.join(f.root, "mounted");
        writeFileSync(file, secret);
        env.LIGHTSPEED_API_TOKEN_CLI_FILE = file;
      } else {
        const first = await prepareCliConnection({ ...f, env: { ...f.env }, full: false });
        writeFileSync(first.tokenEnvironment.LIGHTSPEED_API_TOKEN_CLI_FILE, secret);
      }
      await assert.rejects(prepareCliConnection({ ...f, env, full: false }), error => {
        assert.match(error.message, /printable ASCII characters without whitespace/);
        assert.ok(!error.message.includes(supplied), "secret values never appear in errors");
        return true;
      });
    }
  });
}

for (const ending of ["", "\n", "\r\n"]) {
  test(`mounted and saved tokens accept exactly one optional ${JSON.stringify(ending)} terminator`, async t => {
    const f = fixture(t);
    const file = path.join(f.root, "mounted");
    writeFileSync(file, supplied + ending);
    const first = await prepareCliConnection({ ...f, env: { ...f.env, LIGHTSPEED_API_TOKEN_CLI_FILE: file }, full: false });
    const saved = first.tokenEnvironment.LIGHTSPEED_API_TOKEN_CLI_FILE;
    assert.equal(readFileSync(saved, "utf8"), supplied);
    writeFileSync(saved, supplied + ending);
    await prepareCliConnection({ ...f, env: { ...f.env }, full: false });
    assert.equal(readFileSync(saved, "utf8"), supplied);
  });
}

test("every configured token source rejects simultaneous inline and file settings", async t => {
  for (const name of ["LIGHTSPEED_API_TOKEN", "LIGHTSPEED_API_TOKEN_CLI", "LIGHTSPEED_API_TOKEN_PLATFORM", "LIGHTSPEED_PLATFORM_API_TOKEN"]) {
    const f = fixture(t);
    await assert.rejects(prepareCliConnection({ ...f, env: { ...f.env, [name]: supplied, [`${name}_FILE`]: "unused" }, full: true }), /not both/);
  }
});


for (const runtimeName of ["LIGHTSPEED_API_TOKEN", "LIGHTSPEED_API_TOKEN_CLI"]) {
  test(`Platform reuses the ${runtimeName} identity when both use one secret`, async t => {
    const f = fixture(t);
    const env = { ...f.env, [runtimeName]: supplied, LIGHTSPEED_PLATFORM_API_TOKEN: supplied };
    const first = await prepareCliConnection({ ...f, env, full: true });
    const expectedName = `${runtimeName}_FILE`;
    assert.equal(readFileSync(first.tokenEnvironment[expectedName], "utf8"), supplied);
    assert.equal(readFileSync(first.tokenEnvironment.LIGHTSPEED_PLATFORM_API_TOKEN_FILE, "utf8"), supplied);
    assert.deepEqual(Object.keys(processEnvironment(env, "runtime")).filter(name => /^LIGHTSPEED_API_TOKEN(?:_|$)/.test(name)), [expectedName]);
    assert.equal(first.tokenEnvironment.LIGHTSPEED_API_TOKEN_PLATFORM_FILE, undefined);
    const restarted = await prepareCliConnection({ ...f, env, full: true });
    assert.equal(restarted.tokenEnvironment.LIGHTSPEED_API_TOKEN_PLATFORM_FILE, undefined);
    assert.deepEqual(Object.keys(processEnvironment(env, "runtime")).filter(name => /^LIGHTSPEED_API_TOKEN(?:_|$)/.test(name)), [expectedName]);
  });
}

test("matching explicit named CLI and Platform tokens keep only the CLI runtime identity", async t => {
  const f = fixture(t);
  const env = { ...f.env, LIGHTSPEED_API_TOKEN_CLI: supplied, LIGHTSPEED_API_TOKEN_PLATFORM: supplied };
  await prepareCliConnection({ ...f, env, full: true });
  assert.equal(env.LIGHTSPEED_API_TOKEN_PLATFORM, undefined);
  assert.equal(env.LIGHTSPEED_API_TOKEN_PLATFORM_FILE, undefined);
  assert.equal(readFileSync(env.LIGHTSPEED_API_TOKEN_CLI_FILE, "utf8"), supplied);
  assert.equal(readFileSync(env.LIGHTSPEED_PLATFORM_API_TOKEN_FILE, "utf8"), supplied);
});


test("Platform can reuse the default token while the CLI has a different named token", async t => {
  const f = fixture(t);
  const env = { ...f.env, LIGHTSPEED_API_TOKEN: supplied, LIGHTSPEED_API_TOKEN_CLI: supplied + "cli", LIGHTSPEED_PLATFORM_API_TOKEN: supplied };
  await prepareCliConnection({ ...f, env, full: true });
  assert.equal(env.LIGHTSPEED_API_TOKEN, supplied);
  assert.equal(env.LIGHTSPEED_API_TOKEN_PLATFORM_FILE, undefined);
  assert.equal(readFileSync(env.LIGHTSPEED_PLATFORM_API_TOKEN_FILE, "utf8"), supplied);
  assert.equal(readFileSync(env.LIGHTSPEED_API_TOKEN_CLI_FILE, "utf8"), supplied + "cli");
});

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { startProcesses, waitForService } from "./startup.mjs";

const repoRoot = fileURLToPath(new URL("../../", import.meta.url));

for (const profile of ["full", "platform"]) {
  test(`${profile} waits for the configured runtime before Platform bootstrap`, () => {
    const output = execFileSync(process.execPath, [
      "scripts/dev/stack.mjs", "--plan", "--no-envd", profile,
    ], {
      cwd: repoRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        LIGHTSPEED_CHANNELS_CONNECTORS: "",
        LIGHTSPEED_API_URL: "http://127.0.0.1:28080/runtime/rpc",
        LIGHTSPEED_GATEWAY_BIND: "127.0.0.1:28080",
        LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT: "28081",
        LIGHTSPEED_AUTH_MODE: "authenticated",
        PORT: "23000",
      },
    });
    assert.match(output, /process: platform ->[^\n]+\n  after: runtime gateway -> http:\/\/127\.0\.0\.1:28080\/runtime\/health/);
  });
}

async function runtimeFixture(t) {
  const probed = Promise.withResolvers();
  const state = { ready: false, probes: 0 };
  const server = createServer((request, response) => {
    assert.equal(request.url, "/health");
    state.probes++;
    response.writeHead(state.ready ? 200 : 503).end();
    probed.resolve();
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  t.after(async () => {
    const closed = once(server, "close");
    server.close();
    server.closeAllConnections();
    await closed;
  });
  const service = { name: "runtime gateway", url: `http://127.0.0.1:${server.address().port}/health` };
  return { state, service, probed: probed.promise };
}

test("a listening but unready runtime cannot start Platform; readiness releases startup", { timeout: 5_000 }, async (t) => {
  const runtime = await runtimeFixture(t);
  const started = [];
  const startup = startProcesses([
    { name: "runtime" },
    { name: "platform", startAfter: runtime.service },
  ], {
    start: (process) => started.push(process.name),
    wait: (service) => waitForService(service, { timeoutMs: 2_000, pollMs: 10 }),
  });
  await runtime.probed;
  assert.deepEqual(started, ["runtime"], "bootstrap must not run during runtime startup");
  runtime.state.ready = true;
  await startup;
  assert.deepEqual(started, ["runtime", "platform"]);
  assert.ok(runtime.state.probes >= 2, "HTTP readiness, not an open socket, admits the dependency");
});

test("failed runtime readiness never spawns Platform", { timeout: 5_000 }, async (t) => {
  const runtime = await runtimeFixture(t);
  const started = [];
  await assert.rejects(startProcesses([
    { name: "runtime" },
    { name: "platform", startAfter: runtime.service },
  ], {
    start: (process) => started.push(process.name),
    wait: (service) => waitForService(service, { timeoutMs: 50, pollMs: 10 }),
  }), /runtime gateway did not become ready/);
  assert.deepEqual(started, ["runtime"]);
});

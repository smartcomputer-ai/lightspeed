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
        LIGHTSPEED_API_URL: "http://127.0.0.1:28080/runtime/api/v1",
        LIGHTSPEED_GATEWAY_BIND: "127.0.0.1:28080",
        LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT: "28081",
        LIGHTSPEED_AUTH_MODE: "authenticated",
        PORT: "23000",
      },
    });
    assert.match(output, /process: platform ->[^\n]+\n  after: runtime gateway -> http:\/\/127\.0\.0\.1:28080\/runtime\/health/);
  });
  test(`${profile} uses the browser origin for CLI approval regardless of the backend port`, () => {
    const env = { ...process.env, PORT: "23000" };
    delete env.LIGHTSPEED_PLATFORM_BASE_URL;
    const output = execFileSync(process.execPath, ["scripts/dev/stack.mjs", "--plan", "--no-envd", profile], {
      cwd: repoRoot, encoding: "utf8", env,
    });
    assert.match(output, /^platform public URL: http:\/\/localhost:5173$/m);
    assert.match(output, /^ready: platform web -> http:\/\/localhost:5173\/app\/$/m);
    assert.match(output, /^ready: platform API -> http:\/\/127\.0\.0\.1:23000\/health$/m);
  });
}

test("an explicit development public origin remains authoritative", () => {
  const output = execFileSync(process.execPath, ["scripts/dev/stack.mjs", "--plan", "--no-envd", "platform"], {
    cwd: repoRoot, encoding: "utf8",
    env: { ...process.env, LIGHTSPEED_PLATFORM_BASE_URL: "https://dev.example.test" },
  });
  assert.match(output, /^platform public URL: https:\/\/dev\.example\.test$/m);
});

test("the development web origin forwards API and device approval to the configured backend", () => {
  const output = execFileSync(process.execPath, ["--import", "tsx", "--input-type=module", "-e",
    'import configure from "./platform/web/vite.config.ts"; const config = configure({ command: "serve", mode: "development" }); console.log(JSON.stringify(config.server.proxy));',
  ], { cwd: repoRoot, encoding: "utf8", env: { ...process.env, PORT: "23000" } });
  assert.deepEqual(JSON.parse(output), { "/api": "http://localhost:23000", "/device": "http://localhost:23000" });
});

for (const profile of ["full", "platform", "runtime", "demo", "infra"]) {
  test(`${profile} scopes development fixtures to the authenticated full profile`, () => {
    const env = { ...process.env, LIGHTSPEED_AUTH_MODE: "authenticated" };
    delete env.LIGHTSPEED_PLATFORM_DEV_SEED;
    const output = execFileSync(process.execPath, ["scripts/dev/stack.mjs", "--plan", "--no-envd", profile], {
      cwd: repoRoot, encoding: "utf8", env,
    });
    assert.match(output, new RegExp(`development fixtures: ${profile === "full"}`));
  });
}

test("full profile can opt out of development fixtures", () => {
  const output = execFileSync(process.execPath, ["scripts/dev/stack.mjs", "--plan", "--no-envd", "full"], {
    cwd: repoRoot, encoding: "utf8",
    env: { ...process.env, LIGHTSPEED_AUTH_MODE: "authenticated",
      LIGHTSPEED_PLATFORM_DEV_SEED: "false" },
  });
  assert.match(output, /development fixtures: false/);
});

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
  }), (error) => {
    assert.match(error.message, /runtime gateway did not become ready/);
    assert.ok(error.details.includes(`Endpoint: ${runtime.service.url}`));
    assert.match(error.details.join("\n"), /HTTP 503/);
    return true;
  });
  assert.deepEqual(started, ["runtime"]);
});

test("stopping a dependency wait does not start the waiting process or report a timeout", async () => {
  let stopping = false;
  const started = [];
  await startProcesses([{ name: "platform", startAfter: { name: "runtime" } }], {
    start: (process) => started.push(process.name),
    wait: async (service) => {
      stopping = true;
      await waitForService(service, { isStopping: () => stopping });
    },
    isStopping: () => stopping,
  });
  assert.deepEqual(started, []);
});

test("readiness distinguishes a refused connection from an HTTP failure", { timeout: 5_000 }, async () => {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  await assert.rejects(waitForService({ name: "fixture", url: `http://127.0.0.1:${port}/health` }, {
    timeoutMs: 50, pollMs: 5,
  }), (error) => {
    assert.match(error.details.join("\n"), /Connection refused/);
    return true;
  });
});


test("full profile routes Configurator through Platform", () => {
  const output = execFileSync(process.execPath, ["scripts/dev/stack.mjs", "--plan", "--no-envd", "full"], {
    cwd: repoRoot, encoding: "utf8", env: { ...process.env },
  });
  assert.match(output, /^process: configurator ->/m);
});

for (const profile of ["runtime", "full"]) {
  test(`${profile} preflights and waits for private HTTP API, public callbacks, and environment routes separately`, () => {
    const output = execFileSync(process.execPath, ["scripts/dev/stack.mjs", "--plan", "--no-envd", profile], {
      cwd: repoRoot, encoding: "utf8", env: { ...process.env, LIGHTSPEED_GATEWAY_BIND: "127.0.0.1:28080", LIGHTSPEED_PUBLIC_BIND: "127.0.0.1:28082", LIGHTSPEED_ENVIRONMENT_GATEWAY_BIND: "127.0.0.1:28083" },
    });
    assert.match(output, /^ready: runtime gateway -> http:\/\/127\.0\.0\.1:28080\/health$/m);
    assert.match(output, /^ready: runtime public -> http:\/\/127\.0\.0\.1:28082\/health$/m);
    assert.match(output, /^ready: environment gateway -> http:\/\/127\.0\.0\.1:28083\/health$/m);
  });
}

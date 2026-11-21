import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { mkdtempSync, mkdirSync, copyFileSync, readFileSync, writeFileSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { once } from "node:events";
import { createServer } from "node:http";
import { createInterface } from "node:readline";
import { prepareCliConnection } from "./cli-connection.mjs";

const root = fileURLToPath(new URL("../../", import.meta.url));

for (const alias of ["docs", "doc", "documentation"]) {
  for (const prefix of [[], ["start"]]) {
    test(`documentation plan: ./dev.sh ${[...prefix, alias].join(" ")}`, () => {
      const result = spawnSync("./dev.sh", [...prefix, alias, "--plan"], {
        cwd: root,
        env: { ...process.env, LIGHTSPEED_AUTH_MODE: "single", LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001" },
        encoding: "utf8",
      });
      assert.equal(result.status, 0, result.stderr);
      assert.match(result.stdout, /^profile: docs$/m);
      assert.match(result.stdout, /^infrastructure: none$/m);
      const processes = result.stdout.split("\n").filter((line) => line.startsWith("process:"));
      assert.equal(processes.length, 1);
      assert.match(processes[0], /^process: docs -> .*docs\/site\/scripts\/dev\.mjs$/);
      assert.doesNotMatch(result.stdout, /^prepare:/m);
      assert.match(result.stdout, /^environment daemon: off$/m);
    });
  }
}

test("invalid configuration gets a summary, with launcher stack traces only on request", () => {
  for (const debug of [false, true]) {
    const result = spawnSync(process.execPath, ["scripts/dev/stack.mjs", "docs", "--plan", ...(debug ? ["--debug"] : [])], {
      cwd: root, encoding: "utf8", env: { ...process.env, PORT: "invalid", LIGHTSPEED_AUTH_MODE: "single", LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001" },
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Failed while configuring the docs profile/);
    assert.match(result.stderr, /PORT must be an integer/);
    if (debug) assert.match(result.stderr, /at positivePort/);
    else assert.doesNotMatch(result.stderr, /at positivePort/);
  }
});

for (const [argument, diagnostic] of [
  ["not-a-profile", /Unknown command or profile/],
  ["--allow-missing-api-keys", /unexpected development arguments/],
]) {
  test(`unsupported CLI argument ${argument} is explained before checking Docker`, () => {
    const result = spawnSync("./dev.sh", [argument], { cwd: root, encoding: "utf8" });
    assert.equal(result.status, 1);
    assert.match(result.stderr, diagnostic);
    assert.match(result.stderr, /--help/);
    assert.doesNotMatch(result.stderr, /Docker|at parseCli/);
  });
}

for (const bindVariable of ["LIGHTSPEED_GATEWAY_BIND", "LIGHTSPEED_PUBLIC_BIND", "LIGHTSPEED_ENVIRONMENT_GATEWAY_BIND"]) {
  test(`an occupied ${bindVariable} fails before infrastructure or migrations and releases supervisor state`, { timeout: 5_000 }, async (t) => {
    const directory = launcherFixture(t);
    const server = createServer();
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    t.after(() => server.close());
    const port = server.address().port;
    const result = spawnSync(process.execPath, ["scripts/dev/stack.mjs", "runtime", "--no-envd"], {
      cwd: directory, encoding: "utf8", env: { ...process.env, ...await availableListenerEnv(), [bindVariable]: `127.0.0.1:${port}`, LIGHTSPEED_AUTH_MODE: "single", LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001" },
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, new RegExp(`Port ${port} .*already in use`));
    assert.match(result.stderr, /lsof -nP/);
    assert.doesNotMatch(result.stdout, /\[infra\]|\[runtime migration\]/);
    assert.ok(!existsSync(path.join(directory, ".lightspeed/dev-supervisor.json")));
  });

}

async function availableListenerEnv() {
  const env = {};
  const reservations = [];
  try {
    for (const name of ["LIGHTSPEED_GATEWAY_BIND", "LIGHTSPEED_PUBLIC_BIND", "LIGHTSPEED_ENVIRONMENT_GATEWAY_BIND"]) {
      const reservation = createServer();
      reservation.listen(0, "127.0.0.1");
      await once(reservation, "listening");
      reservations.push(reservation);
      env[name] = `127.0.0.1:${reservation.address().port}`;
    }
    return env;
  } finally {
    await Promise.all(reservations.map(reservation => new Promise(resolve => reservation.close(resolve))));
  }
}

function launcherFixture(t) {
  const directory = mkdtempSync(path.join(tmpdir(), "lightspeed-launcher-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  mkdirSync(path.join(directory, "scripts/dev/infra"), { recursive: true });
  mkdirSync(path.join(directory, "bin"));
  for (const name of ["stack.mjs", "startup.mjs", "processes.mjs", "errors.mjs", "cli-connection.mjs"]) {
    copyFileSync(path.join(root, "scripts/dev", name), path.join(directory, "scripts/dev", name));
  }
  writeFileSync(path.join(directory, "scripts/dev/env.sh"), "true\n");
  writeFileSync(path.join(directory, "scripts/dev/infra/up.sh"), "#!/bin/sh\necho 'fixture infrastructure only'\n", { mode: 0o755 });
  return directory;
}

for (const resetExitCode of [0, 7]) {
  test(`database reset ${resetExitCode === 0 ? "clears credentials for fresh startup" : "failure preserves credentials"}`, async (t) => {
    const directory = launcherFixture(t);
    const env = {
      ...process.env,
      LIGHTSPEED_AUTH_MODE: "authenticated",
      LIGHTSPEED_API_URL: "http://127.0.0.1:18080/api/v1",
      LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001",
    };
    const run = async () => ({ stdout: "" });
    const options = { root: directory, env, full: true, run };
    const before = await prepareCliConnection(options);
    const beforeToken = readFileSync(before.tokenEnvironment.LIGHTSPEED_PLATFORM_API_TOKEN_FILE, "utf8");
    const unrelated = path.join(directory, ".lightspeed", "unrelated");
    writeFileSync(unrelated, "preserve");
    writeFileSync(path.join(directory, "scripts/dev/infra/reset.sh"), `#!/bin/sh\nexit ${resetExitCode}\n`, { mode: 0o755 });
    const result = spawnSync(process.execPath, ["scripts/dev/stack.mjs", "reset"], {
      cwd: directory, encoding: "utf8", env,
    });
    assert.equal(result.status, resetExitCode === 0 ? 0 : 1, result.stderr);
    assert.ok(existsSync(unrelated));
    const credentialDirectory = path.join(directory, ".lightspeed", "cli");
    if (resetExitCode === 0) {
      assert.ok(!existsSync(credentialDirectory));
      assert.match(result.stdout, /lightspeed connect dev/);
      delete env.LIGHTSPEED_API_TOKEN_CLI_FILE;
      delete env.LIGHTSPEED_API_TOKEN_PLATFORM_FILE;
      delete env.LIGHTSPEED_PLATFORM_API_TOKEN_FILE;
      const after = await prepareCliConnection(options);
      assert.notEqual(readFileSync(after.tokenEnvironment.LIGHTSPEED_PLATFORM_API_TOKEN_FILE, "utf8"), beforeToken);
      assert.ok(existsSync(after.handoff));
    } else {
      assert.ok(existsSync(before.handoff));
      const after = await prepareCliConnection(options);
      assert.equal(readFileSync(after.tokenEnvironment.LIGHTSPEED_PLATFORM_API_TOKEN_FILE, "utf8"), beforeToken);
    }
  });
}

for (const failure of ["migration", "runtime"]) {
  test(`${failure} failure is reported once with recent output and cleanup state`, { timeout: 10_000 }, async (t) => {
    const directory = launcherFixture(t);
    writeFileSync(path.join(directory, "bin/cargo"), `#!/bin/sh
case "$*" in
  *universe*) exit 0 ;;
  *migrate*) ${failure === "migration" ? "echo 'fixture migration rejected' >&2; exit 7" : "exit 0"} ;;
  *) echo 'fixture runtime crashed' >&2; exit 9 ;;
esac
`, { mode: 0o755 });
    const listenerEnv = await availableListenerEnv();
    const result = spawnSync(process.execPath, ["scripts/dev/stack.mjs", "runtime", "--no-envd"], {
      cwd: directory, encoding: "utf8", timeout: 5_000,
      env: { ...process.env, PATH: `${directory}/bin:${process.env.PATH}`, ...listenerEnv, LIGHTSPEED_AUTH_MODE: "single", LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001" },
    });
    assert.equal(result.status, 1, result.stderr);
    assert.equal(result.stderr.split("dev.sh: Failed").length - 1, 1);
    assert.match(result.stderr, new RegExp(`fixture ${failure} ${failure === "migration" ? "rejected" : "crashed"}`));
    assert.match(result.stderr, /Recent output:/);
    assert.match(result.stderr, /Docker infrastructure was left in place/);
    assert.doesNotMatch(result.stderr, /at launchCommand|did not become ready/);
    assert.ok(!existsSync(path.join(directory, ".lightspeed/dev-supervisor.json")));
  });
}

// The launcher uses real child process exits and three independent readiness listeners.
// Only the fake cargo child receives signals, and all addresses belong to this fixture.
const leaseFixtureCargo = `#!/usr/bin/env node
const { createServer } = require("node:http");
if (process.argv.includes("migrate") || process.argv.includes("create")) {
  console.log("fixture preparation");
  process.exit(0);
}
function fail() {
  if (process.env.FIXTURE_FAILURE === "drain") {
    console.error("ERROR temporal_runtime::gateway::replica: gateway lease lost; closing connections instance=00000000-0000-4000-8000-000000000001");
  }
  console.error(process.env.FIXTURE_FAILURE === "lease"
    ? "Error: environment gateway instance lease expired"
    : "Error: gateway HTTP shutdown exceeded 5 seconds");
  process.exit(1);
}
process.on("SIGUSR2", fail);
process.on("SIGTERM", fail);
console.log("fixture runtime pid=" + process.pid);
if (process.env.FIXTURE_BEFORE_READY === "true") fail();
const checked = new Set();
for (const key of ["LIGHTSPEED_GATEWAY_BIND", "LIGHTSPEED_PUBLIC_BIND", "LIGHTSPEED_ENVIRONMENT_GATEWAY_BIND"]) {
  const [host, port] = process.env[key].split(":");
  createServer((req, res) => {
    if (req.url === "/health") {
      res.end("ok");
      checked.add(key);
      if (checked.size === 3) console.log("fixture gateway healthy");
    } else if (req.url.endsWith("/model-defaults")) {
      res.end(JSON.stringify({ defaults: { revision: 1 } }));
    } else {
      res.writeHead(404).end();
    }
  }).listen(Number(port), host);
}
`;

for (const scenario of [
  { title: "recovers from lease loss followed by a timed-out HTTP drain", failure: "drain", starts: 2, code: 0 },
  { title: "stops after a second rapid confirmed lease expiry", failure: "lease", repeat: true, starts: 2, code: 1 },
  { title: "reports an HTTP drain timeout without lease loss", failure: "timeout", starts: 1, code: 1 },
  { title: "reports lease loss before the profile was ready", failure: "lease", beforeReady: true, starts: 1, code: 1 },
  { title: "honors explicit shutdown even when the runtime reports lease loss", failure: "lease", stop: true, starts: 1, code: 0 },
]) {
  test(`runtime supervisor ${scenario.title}`, { timeout: 12_000 }, async (t) => {
    const directory = launcherFixture(t);
    writeFileSync(path.join(directory, "bin/cargo"), leaseFixtureCargo, { mode: 0o755 });
    const listeners = await availableListenerEnv();
    const child = spawn(process.execPath, ["scripts/dev/stack.mjs", "runtime", "--no-envd"], {
      cwd: directory, timeout: 9_000,
      env: {
        ...process.env, ...listeners, PATH: `${directory}/bin:${process.env.PATH}`,
        LIGHTSPEED_API_URL: `http://${listeners.LIGHTSPEED_GATEWAY_BIND}/api/v1`,
        LIGHTSPEED_AUTH_MODE: "single", LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001",
        FIXTURE_FAILURE: scenario.failure, FIXTURE_BEFORE_READY: String(Boolean(scenario.beforeReady)),
      },
    });
    const closed = once(child, "close");
    let diagnostic = "";
    child.stderr.setEncoding("utf8").on("data", chunk => { diagnostic += chunk; });
    const starts = [];
    const probed = new Set();
    let ready = false;
    let recovered = false;
    let preparations = 0;
    for await (const line of createInterface({ input: child.stdout })) {
      const pid = line.match(/fixture runtime pid=(\d+)/);
      if (pid) starts.push(Number(pid[1]));
      if (line.includes("fixture gateway healthy")) probed.add(starts.length);
      if (line.includes("fixture preparation")) preparations++;
      if (line.includes("Lightspeed development stack is running:")) {
        ready = true;
        if (scenario.stop) child.kill("SIGTERM");
        else process.kill(starts[0], "SIGUSR2");
      }
      if (line.includes("Replacement runtime is ready.")) {
        recovered = true;
        if (scenario.repeat) process.kill(starts[1], "SIGUSR2");
        else child.kill("SIGTERM");
      }
    }
    const [code] = await closed;
    assert.equal(code, scenario.code, diagnostic);
    assert.equal(ready, !scenario.beforeReady, diagnostic);
    assert.equal(starts.length, scenario.starts, diagnostic);
    assert.equal(recovered, scenario.starts === 2, diagnostic);
    assert.equal(probed.size, scenario.beforeReady ? 0 : scenario.starts, "all three listeners must pass health checks on each start");
    assert.equal(preparations, 2, "migration and universe creation should each run once");
    assert.ok(!existsSync(path.join(directory, ".lightspeed/dev-supervisor.json")));
  });
}

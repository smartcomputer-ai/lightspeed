import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { mkdtempSync, mkdirSync, copyFileSync, readFileSync, writeFileSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { once } from "node:events";
import { createServer } from "node:http";
import { prepareCliConnection } from "./cli-connection.mjs";

const root = fileURLToPath(new URL("../../", import.meta.url));

for (const alias of ["docs", "doc", "documentation"]) {
  for (const prefix of [[], ["start"]]) {
    test(`documentation plan: ./dev.sh ${[...prefix, alias].join(" ")}`, () => {
      const result = spawnSync("./dev.sh", [...prefix, alias, "--plan"], {
        cwd: root,
        env: { ...process.env, LIGHTSPEED_CHANNELS_CONNECTORS: "", LIGHTSPEED_AUTH_MODE: "single", LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001" },
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
      cwd: root, encoding: "utf8", env: { ...process.env, PORT: "invalid", LIGHTSPEED_CHANNELS_CONNECTORS: "", LIGHTSPEED_AUTH_MODE: "single", LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001" },
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Failed while configuring the docs profile/);
    assert.match(result.stderr, /PORT must be an integer/);
    if (debug) assert.match(result.stderr, /at positivePort/);
    else assert.doesNotMatch(result.stderr, /at positivePort/);
  }
});

test("unknown CLI arguments are explained before checking Docker", () => {
  const result = spawnSync("./dev.sh", ["not-a-profile"], { cwd: root, encoding: "utf8" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Unknown command or profile/);
  assert.match(result.stderr, /--help/);
  assert.doesNotMatch(result.stderr, /Docker|at parseCli/);
});

for (const bindVariable of ["LIGHTSPEED_GATEWAY_BIND", "LIGHTSPEED_PUBLIC_BIND", "LIGHTSPEED_ENVIRONMENT_GATEWAY_BIND"]) {
  test(`an occupied ${bindVariable} fails before infrastructure or migrations and releases supervisor state`, { timeout: 5_000 }, async (t) => {
    const directory = launcherFixture(t);
    const server = createServer();
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    t.after(() => server.close());
    const port = server.address().port;
    const result = spawnSync(process.execPath, ["scripts/dev/stack.mjs", "runtime", "--no-envd"], {
      cwd: directory, encoding: "utf8", env: { ...process.env, ...await availableListenerEnv(), [bindVariable]: `127.0.0.1:${port}`, LIGHTSPEED_CHANNELS_CONNECTORS: "", LIGHTSPEED_AUTH_MODE: "single", LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001" },
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
      env: { ...process.env, PATH: `${directory}/bin:${process.env.PATH}`, ...listenerEnv, LIGHTSPEED_CHANNELS_CONNECTORS: "", LIGHTSPEED_AUTH_MODE: "single", LIGHTSPEED_PG_UNIVERSE_ID: "00000000-0000-0000-0000-000000000001" },
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

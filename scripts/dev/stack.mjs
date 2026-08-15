#!/usr/bin/env node

// One product-level development supervisor.
//
// Stateful dependencies run in Docker Compose. Rust and TypeScript processes
// run from the checkout so cargo, tsx, and Vite retain their normal edit loops.
import { spawn, spawnSync } from "node:child_process";
import {
  closeSync,
  mkdirSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

const devDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.join(devDir, "..", "..");
const infraDir = path.join(devDir, "infra");
const supervisorStatePath = path.join(repoRoot, ".lightspeed", "dev-supervisor.json");
try {
  process.loadEnvFile(path.join(repoRoot, ".env"));
} catch (error) {
  if (error?.code !== "ENOENT") throw error;
}
const profiles = new Set(["full", "platform", "runtime", "infra"]);
const actions = new Set(["start", "stop", "down", "reset", "status"]);
const cli = parseCli(process.argv.slice(2));
const children = [];
let stopping = false;
let requestedExitCode = 0;

if (cli.help) {
  printHelp();
  process.exit(0);
}

const baseEnv = loadDevEnvironment();

if (cli.action !== "start") {
  await runDevelopmentAction(cli, baseEnv);
  process.exit(0);
}

const plan = createPlan(cli.profile, baseEnv);
if (cli.planOnly) {
  printPlan(plan);
  process.exit(0);
}

validateProviderCredentials(plan, cli.allowMissingApiKeys);
ensureLocalTooling(plan);
if (plan.profile !== "infra") {
  assertSupervisorStopped();
  claimSupervisor(plan.profile);
  process.once("SIGINT", () => shutdown(0));
  process.once("SIGTERM", () => shutdown(0));
}
runChecked("infra", path.join(infraDir, "up.sh"), [], baseEnv);
if (plan.profile === "infra") {
  process.exit(0);
}

for (const port of plan.ports) {
  if (await tcpUp(port.port)) {
    throw new Error(
      `port ${port.port} (${port.name}) is already in use; stop the existing process or choose another development port`,
    );
  }
}

for (const preparation of plan.preparations) {
  runChecked(preparation.name, preparation.command, preparation.args, preparation.env);
}

for (const processPlan of plan.processes) {
  startProcess(processPlan);
}

try {
  await waitForReadiness(plan);
} catch (error) {
  console.error(`[readiness] ${error.message}`);
  shutdown(1);
}
if (!stopping) printRunning(plan);

function parseCli(argv) {
  const args = [...argv];
  const help = removeFlag(args, "--help") || removeFlag(args, "-h");
  const planOnly = removeFlag(args, "--plan");
  const allowMissingApiKeys = removeFlag(args, "--allow-missing-api-keys");
  let action = "start";
  let profile = "full";

  if (actions.has(args[0])) action = args.shift();
  if (action === "start" && profiles.has(args[0])) profile = args.shift();

  let volumes = false;
  if (action === "down") {
    volumes = removeFlag(args, "--volumes") || removeFlag(args, "-v");
  }
  if (args.length > 0) {
    throw new TypeError(`unexpected development arguments: ${args.join(" ")}`);
  }
  if (planOnly && action !== "start") {
    throw new TypeError("--plan is supported only for start profiles");
  }
  if (allowMissingApiKeys && action !== "start") {
    throw new TypeError("--allow-missing-api-keys is supported only when starting a profile");
  }

  return { action, profile, planOnly, help, volumes, allowMissingApiKeys };
}

function removeFlag(args, flag) {
  const index = args.indexOf(flag);
  if (index === -1) return false;
  args.splice(index, 1);
  return true;
}

function loadDevEnvironment() {
  const result = spawnSync(
    "bash",
    ["-c", 'source "$1" >/dev/null && env -0', "bash", path.join(devDir, "env.sh")],
    { cwd: repoRoot, env: process.env, encoding: "buffer" },
  );
  if (result.status !== 0) {
    process.stderr.write(result.stderr ?? Buffer.from(""));
    throw new Error("could not load scripts/dev/env.sh");
  }
  return Object.fromEntries(
    result.stdout
      .toString()
      .split("\0")
      .filter(Boolean)
      .map((entry) => {
        const separator = entry.indexOf("=");
        return [entry.slice(0, separator), entry.slice(separator + 1)];
      }),
  );
}

function createPlan(profile, sourceEnv) {
  const runtimeRpc = sourceEnv.LIGHTSPEED_API_URL ?? "http://127.0.0.1:18080/rpc";
  const platformDatabaseUrl =
    sourceEnv.LIGHTSPEED_PLATFORM_DATABASE_URL ??
    sourceEnv.LIGHTSPEED_TEST_POSTGRES_URL;
  const externalPlatformGateway =
    profile === "platform" &&
    sourceEnv.LIGHTSPEED_PLATFORM_DEV_REAL_GATEWAY === "1";
  const stubPort = positivePort(sourceEnv.STUB_GATEWAY_PORT, 19_999, "STUB_GATEWAY_PORT");
  const platformPort = positivePort(sourceEnv.PORT, 3_000, "PORT");
  const configuratorPort = positivePort(
    sourceEnv.LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT,
    18_081,
    "LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT",
  );
  const runtimePort = addressPort(sourceEnv.LIGHTSPEED_GATEWAY_BIND, 18_080);
  const temporalAddress =
    sourceEnv.TEMPORAL_ADDRESS ?? `127.0.0.1:${sourceEnv.TEMPORAL_PORT ?? "7233"}`;
  const platformApiUrl = externalPlatformGateway
    ? runtimeRpc
    : profile === "platform"
      ? `http://127.0.0.1:${stubPort}/rpc`
      : runtimeRpc;
  const runtimeAuthMode =
    sourceEnv.LIGHTSPEED_AUTH_MODE ?? (profile === "full" ? "trusted-header" : "single");
  const connectorNames = profile === "full" ? parseConnectors(sourceEnv.LIGHTSPEED_CHANNELS_CONNECTORS) : [];
  if (profile !== "full" && sourceEnv.LIGHTSPEED_CHANNELS_CONNECTORS?.trim()) {
    throw new TypeError("LIGHTSPEED_CHANNELS_CONNECTORS is supported only by the full development profile");
  }
  validateConnectorEnvironment(connectorNames, sourceEnv);

  const healthUrls = connectorNames.map((connector) => {
    const port = connectorHealthPort(connector, sourceEnv);
    return `http://127.0.0.1:${port}`;
  });
  const env = {
    ...sourceEnv,
    LIGHTSPEED_API_URL: platformApiUrl,
    LIGHTSPEED_ENDPOINT: runtimeRpc,
    LIGHTSPEED_AUTH_MODE: runtimeAuthMode,
    LIGHTSPEED_PLATFORM_DATABASE_URL: platformDatabaseUrl,
    LIGHTSPEED_PLATFORM_AUTH_SECRET:
      sourceEnv.LIGHTSPEED_PLATFORM_AUTH_SECRET ??
      "local-platform-auth-secret-0123456789abcdef",
    LIGHTSPEED_PLATFORM_BASE_URL:
      sourceEnv.LIGHTSPEED_PLATFORM_BASE_URL ??
      `http://127.0.0.1:${platformPort}`,
    LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS:
      sourceEnv.LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS ??
      "http://127.0.0.1:5173,http://localhost:5173",
    LIGHTSPEED_PLATFORM_ADMIN_EMAIL:
      sourceEnv.LIGHTSPEED_PLATFORM_ADMIN_EMAIL ??
      "admin@lightspeed.dev",
    LIGHTSPEED_PLATFORM_ADMIN_PASSWORD:
      sourceEnv.LIGHTSPEED_PLATFORM_ADMIN_PASSWORD ??
      "lightspeed-dev-password",
    LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL:
      sourceEnv.LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL ??
      `http://127.0.0.1:${configuratorPort}/mcp`,
    LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL:
      sourceEnv.LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL ?? runtimeRpc,
    TEMPORAL_ADDRESS: temporalAddress,
    ...(healthUrls.length === 0 || sourceEnv.LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS
      ? {}
      : { LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS: healthUrls.join(",") }),
  };

  const tsx = path.join(repoRoot, "node_modules", ".bin", "tsx");
  const vite = path.join(repoRoot, "node_modules", ".bin", "vite");
  const processes = [];
  const preparations = [];
  const ports = [];
  const readiness = [];

  if (profile === "runtime" || profile === "full") {
    ports.push({ name: "runtime gateway", port: runtimePort });
    readiness.push({ name: "runtime gateway", url: `http://127.0.0.1:${runtimePort}/health` });
    preparations.push({
      name: "runtime migration",
      command: "cargo",
      args: ["run", "-p", "temporal-server", "--", "migrate"],
      env,
    });
    processes.push({
      name: "runtime",
      command: "cargo",
      args: ["run", "-p", "temporal-server"],
      cwd: repoRoot,
      env,
    });
  }

  if (profile === "platform" || profile === "full") {
    ports.push({ name: "platform API", port: platformPort });
    ports.push({ name: "platform web", port: 5_173 });
    readiness.push(
      { name: "platform API", url: `http://127.0.0.1:${platformPort}/health` },
      { name: "platform web", url: "http://localhost:5173/app/" },
    );
    if (profile === "platform" && !externalPlatformGateway) {
      ports.push({ name: "stub gateway", port: stubPort });
      processes.push({
        name: "stub",
        command: process.execPath,
        args: [path.join(repoRoot, "platform", "scripts", "stub-gateway.mjs")],
        cwd: repoRoot,
        env,
      });
    }
    processes.push(
      {
        name: "platform",
        command: tsx,
        args: ["watch", "platform/server/src/main.ts"],
        cwd: repoRoot,
        env,
      },
      {
        name: "web",
        command: vite,
        args: ["--host", "localhost"],
        cwd: path.join(repoRoot, "platform", "web"),
        env,
      },
    );
  }

  if (profile === "full") {
    ports.push({ name: "Configurator MCP", port: configuratorPort });
    ports.push({ name: "Channels workflow metrics", port: 9_090 });
    ports.push({ name: "Channels activity metrics", port: 9_093 });
    readiness.push(
      { name: "Configurator MCP", url: `http://127.0.0.1:${configuratorPort}/health` },
      { name: "Channels workflow worker", port: 9_090 },
      { name: "Channels activity worker", port: 9_093 },
    );
    processes.splice(1, 0, {
      name: "configurator",
      command: tsx,
      args: ["platform/configurator-mcp/src/bin.ts"],
      cwd: repoRoot,
      env,
    });
    processes.push(
      channelsProcess("channels-workflows", "workflows", 9_090, env, tsx),
      channelsProcess("channels-activities", "activities", 9_093, env, tsx),
    );
    for (const connector of connectorNames) {
      const metricsPort = connector === "telegram" ? 9_091 : 9_092;
      const healthPort = connectorHealthPort(connector, env);
      ports.push({ name: `${connector} metrics`, port: metricsPort });
      ports.push({ name: `${connector} health`, port: healthPort });
      readiness.push({
        name: `${connector} connector`,
        url: `http://127.0.0.1:${healthPort}/healthz`,
      });
      processes.push(channelsProcess(`channels-${connector}`, connector, metricsPort, env, tsx));
    }
  }

  return {
    profile,
    env,
    preparations,
    processes,
    ports: uniquePorts(ports),
    readiness,
    connectors: connectorNames,
    tools: profile === "platform" || profile === "full" ? [tsx, vite] : [],
  };
}

function channelsProcess(name, role, metricsPort, env, tsx) {
  return {
    name,
    command: tsx,
    args: ["platform/channels/src/runtime/main.ts", role],
    cwd: repoRoot,
    env: { ...env, LIGHTSPEED_CHANNELS_METRICS_PORT: String(metricsPort) },
  };
}

function parseConnectors(value) {
  if (value === undefined || value.trim() === "") return [];
  const result = [];
  for (const raw of value.split(",")) {
    const connector = raw.trim();
    if (connector !== "telegram" && connector !== "whatsapp") {
      throw new TypeError(
        `invalid LIGHTSPEED_CHANNELS_CONNECTORS entry ${JSON.stringify(connector)}; expected telegram or whatsapp`,
      );
    }
    if (!result.includes(connector)) result.push(connector);
  }
  return result;
}

function validateConnectorEnvironment(connectors, env) {
  const requirements = {
    telegram: ["LIGHTSPEED_CHANNELS_TELEGRAM_BOT_TOKEN", "LIGHTSPEED_CHANNELS_TELEGRAM_ACCOUNT_ID"],
    whatsapp: [
      "LIGHTSPEED_CHANNELS_WHATSAPP_ACCOUNT_ID",
      "LIGHTSPEED_CHANNELS_WHATSAPP_AUTH_DIR",
      "LIGHTSPEED_CHANNELS_WHATSAPP_MEDIA_LOCATOR_KEY",
    ],
  };
  for (const connector of connectors) {
    const missing = requirements[connector].filter((name) => !env[name]?.trim());
    if (missing.length > 0) {
      throw new TypeError(`${connector} development connector requires ${missing.join(", ")}`);
    }
  }
}

function connectorHealthPort(connector, env) {
  const specific =
    connector === "telegram"
      ? env.LIGHTSPEED_CHANNELS_TELEGRAM_HEALTH_PORT
      : env.LIGHTSPEED_CHANNELS_WHATSAPP_HEALTH_PORT;
  return positivePort(
    specific ?? env.LIGHTSPEED_CHANNELS_HEALTH_PORT,
    connector === "telegram" ? 8_091 : 8_092,
    `LIGHTSPEED_CHANNELS_${connector.toUpperCase()}_HEALTH_PORT`,
  );
}

function positivePort(raw, fallback, name) {
  if (raw === undefined || raw === "") return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value <= 0 || value > 65_535) {
    throw new TypeError(`${name} must be an integer between 1 and 65535`);
  }
  return value;
}

function addressPort(address, fallback) {
  if (!address) return fallback;
  const separator = address.lastIndexOf(":");
  return positivePort(address.slice(separator + 1), fallback, "LIGHTSPEED_GATEWAY_BIND");
}

function uniquePorts(ports) {
  const seen = new Map();
  for (const entry of ports) {
    const existing = seen.get(entry.port);
    if (existing) {
      throw new TypeError(
        `development port ${entry.port} is assigned to both ${existing} and ${entry.name}`,
      );
    }
    seen.set(entry.port, entry.name);
  }
  return ports;
}

function ensureLocalTooling(plan) {
  for (const tool of plan.tools) {
    const check = spawnSync("test", ["-x", tool]);
    if (check.status !== 0) {
      throw new Error(
        `missing ${path.relative(repoRoot, tool)}; run npm install from the repository root`,
      );
    }
  }
}

function validateProviderCredentials(plan, allowMissingApiKeys) {
  const usesRuntime = plan.profile === "full" || plan.profile === "runtime";
  if (!usesRuntime) {
    if (allowMissingApiKeys) {
      throw new TypeError(
        "--allow-missing-api-keys applies only to the full and runtime profiles",
      );
    }
    return;
  }
  const configured = [plan.env.OPENAI_API_KEY, plan.env.ANTHROPIC_API_KEY].some(
    (value) => value?.trim() && !value.startsWith("set_your_"),
  );
  if (configured) return;
  if (!allowMissingApiKeys) {
    throw new Error(
      "no OPENAI_API_KEY or ANTHROPIC_API_KEY is configured; copy .env.example to .env and set a provider key, or pass --allow-missing-api-keys",
    );
  }
  console.warn(`
[credentials] No OPENAI_API_KEY or ANTHROPIC_API_KEY is configured.
[credentials] Continuing because --allow-missing-api-keys was provided.
[credentials] Provider-backed runs will fail until credentials are configured.`);
}

async function runDevelopmentAction(options, env) {
  if (options.action === "stop") {
    await stopSupervisor();
    return;
  }
  if (options.action === "down") {
    await stopSupervisor();
    runChecked(
      "infra",
      path.join(infraDir, "down.sh"),
      options.volumes ? ["--volumes"] : [],
      env,
    );
    return;
  }
  if (options.action === "reset") {
    const supervisor = readSupervisorState();
    if (supervisor) {
      throw new Error(
        `development supervisor is running (profile ${supervisor.profile}, pid ${supervisor.pid}); run ./dev.sh stop before resetting state`,
      );
    }
    runChecked("infra", path.join(infraDir, "reset.sh"), [], env);
    return;
  }
  const supervisor = readSupervisorState();
  console.log(
    supervisor
      ? `Host supervisor: running (profile ${supervisor.profile}, pid ${supervisor.pid}, started ${supervisor.startedAt})`
      : "Host supervisor: stopped",
  );
  runChecked(
    "infra",
    "docker",
    [
      "compose",
      "--project-name",
      env.COMPOSE_PROJECT_NAME ?? "lightspeed-dev",
      "-f",
      path.join(devDir, "docker-compose.yaml"),
      "ps",
    ],
    env,
  );
}

function assertSupervisorStopped() {
  const supervisor = readSupervisorState();
  if (!supervisor) return;
  throw new Error(
    `development supervisor is already running (profile ${supervisor.profile}, pid ${supervisor.pid}); run ./dev.sh stop first`,
  );
}

function claimSupervisor(profile) {
  mkdirSync(path.dirname(supervisorStatePath), { recursive: true });
  const state = {
    version: 1,
    pid: process.pid,
    profile,
    startedAt: new Date().toISOString(),
  };
  let descriptor;
  try {
    descriptor = openSync(supervisorStatePath, "wx", 0o600);
    writeFileSync(descriptor, `${JSON.stringify(state, null, 2)}\n`, "utf8");
  } catch (error) {
    if (error?.code === "EEXIST") {
      throw new Error(
        "development supervisor state appeared during startup; run ./dev.sh status and retry",
      );
    }
    throw error;
  } finally {
    if (descriptor !== undefined) closeSync(descriptor);
  }
  process.once("exit", () => clearSupervisorState(process.pid));
}

function readSupervisorState() {
  let state;
  try {
    state = JSON.parse(readFileSync(supervisorStatePath, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    rmSync(supervisorStatePath, { force: true });
    return null;
  }
  if (
    state?.version !== 1 ||
    !Number.isSafeInteger(state.pid) ||
    typeof state.profile !== "string" ||
    typeof state.startedAt !== "string" ||
    !isSupervisorProcess(state.pid)
  ) {
    rmSync(supervisorStatePath, { force: true });
    return null;
  }
  return state;
}

function isSupervisorProcess(pid) {
  try {
    process.kill(pid, 0);
  } catch {
    return false;
  }
  const result = spawnSync("ps", ["-p", String(pid), "-o", "command="], {
    encoding: "utf8",
  });
  return result.status === 0 && result.stdout.includes("scripts/dev/stack.mjs");
}

async function stopSupervisor() {
  const supervisor = readSupervisorState();
  if (!supervisor) {
    console.log("Host supervisor is not running.");
    return;
  }
  console.log(
    `[supervisor] stopping ${supervisor.profile} development stack (pid ${supervisor.pid})`,
  );
  process.kill(supervisor.pid, "SIGTERM");
  const deadline = Date.now() + 30_000;
  while (isSupervisorProcess(supervisor.pid) && Date.now() < deadline) {
    await delay(100);
  }
  if (isSupervisorProcess(supervisor.pid)) {
    throw new Error(
      `development supervisor pid ${supervisor.pid} did not stop; infrastructure was left running`,
    );
  }
  clearSupervisorState(supervisor.pid);
  console.log("Host supervisor stopped.");
}

function clearSupervisorState(expectedPid) {
  try {
    const state = JSON.parse(readFileSync(supervisorStatePath, "utf8"));
    if (state?.pid === expectedPid) rmSync(supervisorStatePath, { force: true });
  } catch (error) {
    if (error?.code !== "ENOENT") rmSync(supervisorStatePath, { force: true });
  }
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function waitForReadiness(plan) {
  if (plan.readiness.length === 0) return;
  console.log("\nWaiting for application services...");
  await Promise.all(plan.readiness.map(waitForService));
  for (const service of plan.readiness) console.log(`  ready  ${service.name}`);
}

async function waitForService(service) {
  const deadline = Date.now() + 60_000;
  while (!stopping && Date.now() < deadline) {
    const ready = service.url ? await httpUp(service.url) : await tcpUp(service.port);
    if (ready) return;
    await delay(250);
  }
  throw new Error(`${service.name} did not become ready within 60 seconds`);
}

async function httpUp(url) {
  try {
    const response = await fetch(url, { signal: AbortSignal.timeout(1_000) });
    return response.ok;
  } catch {
    return false;
  }
}

function runChecked(name, command, args, env) {
  console.log(`[${name}] ${displayCommand(command, args)}`);
  const result = spawnSync(command, args, { cwd: repoRoot, env, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${name} command exited with ${result.status ?? "a signal"}`);
  }
}

function startProcess(processPlan) {
  const child = spawn(processPlan.command, processPlan.args, {
    cwd: processPlan.cwd,
    env: processPlan.env,
    shell: false,
  });
  children.push(child);
  const prefix = `[${processPlan.name}] `;
  pipePrefixed(child.stdout, process.stdout, prefix);
  pipePrefixed(child.stderr, process.stderr, prefix);
  child.once("error", (error) => {
    console.error(`${prefix}${error.message}`);
    shutdown(1);
  });
  child.once("exit", (code, signal) => {
    console.log(`${prefix}exited (${code ?? signal ?? "unknown"})`);
    if (!stopping) shutdown(code === 0 ? 1 : (code ?? 1));
  });
}

function pipePrefixed(stream, output, prefix) {
  let buffer = "";
  stream.on("data", (chunk) => {
    buffer += chunk.toString();
    const lines = buffer.split("\n");
    buffer = lines.pop() ?? "";
    for (const line of lines) output.write(prefix + line + "\n");
  });
  stream.on("end", () => {
    if (buffer) output.write(prefix + buffer + "\n");
  });
}

function shutdown(exitCode) {
  if (stopping) return;
  stopping = true;
  requestedExitCode = exitCode;
  for (const child of children) {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
  }
  const deadline = setTimeout(() => process.exit(requestedExitCode), 2_000);
  deadline.unref();
  Promise.all(children.map(waitForExit)).then(() => process.exit(requestedExitCode));
}

function waitForExit(child) {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve) => child.once("exit", resolve));
}

function tcpUp(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ port, host: "127.0.0.1", timeout: 500 });
    socket.once("connect", () => {
      socket.destroy();
      resolve(true);
    });
    socket.once("error", () => resolve(false));
    socket.once("timeout", () => {
      socket.destroy();
      resolve(false);
    });
  });
}

function printPlan(plan) {
  console.log(`profile: ${plan.profile}`);
  console.log("infrastructure: postgres, pgadmin, minio, temporal");
  for (const preparation of plan.preparations) {
    console.log(
      `prepare: ${preparation.name} -> ${displayCommand(preparation.command, preparation.args)}`,
    );
  }
  for (const processPlan of plan.processes) {
    console.log(
      `process: ${processPlan.name} -> ${displayCommand(processPlan.command, processPlan.args)}`,
    );
  }
  console.log(`connectors: ${plan.connectors.length > 0 ? plan.connectors.join(", ") : "none"}`);
  console.log(`runtime auth: ${plan.env.LIGHTSPEED_AUTH_MODE}`);
}

function printRunning(plan) {
  const platform = plan.profile === "platform" || plan.profile === "full";
  console.log("\nLightspeed development stack is running:");
  console.log(`  profile       ${plan.profile}`);
  if (plan.profile === "runtime" || plan.profile === "full") {
    console.log(`  runtime       ${plan.env.LIGHTSPEED_ENDPOINT}`);
  }
  if (platform) {
    console.log(`  platform API  http://127.0.0.1:${plan.env.PORT ?? "3000"}`);
    console.log("  web           http://localhost:5173/app/");
    console.log(
      `  login         ${plan.env.LIGHTSPEED_PLATFORM_ADMIN_EMAIL} / ${plan.env.LIGHTSPEED_PLATFORM_ADMIN_PASSWORD}`,
    );
  }
  if (plan.connectors.length > 0) {
    console.log(`  connectors    ${plan.connectors.join(", ")}`);
  }
  console.log("\nPress Ctrl-C to stop host processes; infrastructure remains available.\n");
}

function displayCommand(command, args) {
  const relative = path.isAbsolute(command) ? path.relative(repoRoot, command) : command;
  return [relative, ...args].join(" ");
}

function printHelp() {
  console.log(`Usage:
  ./dev.sh                                 Bootstrap and start the full editable product
  ./dev.sh [start] <profile>               Start full, platform, runtime, or infra
  ./dev.sh [profile] --allow-missing-api-keys
                                           Permit full/runtime startup without provider keys
  ./dev.sh --plan <profile>                Print a profile without starting it
  ./dev.sh status                          Show host supervisor and infrastructure
  ./dev.sh stop                            Stop host processes; keep infrastructure
  ./dev.sh down [--volumes]                Stop host processes and infrastructure
  ./dev.sh reset                           Reset Postgres and MinIO development state

The npm run dev commands are aliases for the same launcher.

Profiles:
  full      Infrastructure, Rust runtime, Configurator, Platform, web, and
            Channels workflow/activity workers. LIGHTSPEED_CHANNELS_CONNECTORS optionally
            adds telegram and/or whatsapp.
  platform  Infrastructure, stub gateway, Platform API, and web UI. Set
            LIGHTSPEED_PLATFORM_DEV_REAL_GATEWAY=1 to use an external runtime.
  runtime   Infrastructure and the migrated Rust runtime.
  infra     Postgres, pgAdmin, MinIO, and Temporal only.`);
}

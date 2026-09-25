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
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startProcesses, waitForService as awaitService, tcpUp } from "./startup.mjs";

const devDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.join(devDir, "..", "..");
const infraDir = path.join(devDir, "infra");
const supervisorStatePath = path.join(repoRoot, ".lightspeed", "dev-supervisor.json");
try {
  process.loadEnvFile(path.join(repoRoot, ".env"));
} catch (error) {
  if (error?.code !== "ENOENT") throw error;
}
const profiles = new Set(["full", "platform", "runtime", "demo", "infra"]);
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

validateProviderCredentials(plan, cli.requireApiKeys);
ensureLocalTooling(plan);
if (plan.profile !== "infra") {
  assertSupervisorStopped();
  claimSupervisor(plan.profile);
  process.once("SIGINT", () => shutdown(0));
  process.once("SIGTERM", () => shutdown(0));
}
if (plan.infra) runChecked("infra", path.join(infraDir, "up.sh"), [], baseEnv);
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

if (plan.profile === "full") {
  const runtime = plan.processes.find((p) => p.name === "runtime");
  if (runtime.env.LIGHTSPEED_AUTH_MODE === "authenticated" && !runtime.env.LIGHTSPEED_PLATFORM_API_KEY) {
    console.log("[prepare] local development universe and Platform deployment key");
    const result = spawnSync("cargo", ["run", "-p", "temporal-server", "--", "api-key", "bootstrap", "--universe-id", runtime.env.LIGHTSPEED_PG_UNIVERSE_ID], {
      cwd: repoRoot, env: { ...runtime.env, RUST_LOG: "off" }, encoding: "utf8",
    });
    if (result.status !== 0) throw new Error(`development key bootstrap failed: ${result.stderr}`);
    const credential = JSON.parse(result.stdout);
    for (const processPlan of plan.processes) {
      processPlan.env.LIGHTSPEED_PLATFORM_API_KEY = credential.secret;
    }
  }
}

try {
  await startProcesses(plan.processes, { start: startProcess, wait: waitForService });
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
  // Accepted for compatibility; missing keys only warn since keys can be
  // added per universe from the Platform UI (Integrations).
  removeFlag(args, "--allow-missing-api-keys");
  const requireApiKeys = removeFlag(args, "--require-api-keys");
  const noEnvd = removeFlag(args, "--no-envd");
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
  if (requireApiKeys && action !== "start") {
    throw new TypeError("--require-api-keys is supported only when starting a profile");
  }
  if (noEnvd && action !== "start") {
    throw new TypeError("--no-envd is supported only when starting a profile");
  }

  return { action, profile, planOnly, help, volumes, requireApiKeys, noEnvd };
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
  const platformPort = positivePort(sourceEnv.PORT, 3_000, "PORT");
  const configuratorPort = positivePort(
    sourceEnv.LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT,
    18_081,
    "LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT",
  );
  const defaultConfiguratorMcpUrl = `http://127.0.0.1:${configuratorPort}/mcp`;
  const configuratorMcpUrl =
    sourceEnv.LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL ?? defaultConfiguratorMcpUrl;
  const configuratorInternalTrustedHeader = "false";
  const runtimePort = addressPort(sourceEnv.LIGHTSPEED_GATEWAY_BIND, 18_080);
  const temporalAddress =
    sourceEnv.TEMPORAL_ADDRESS ?? `127.0.0.1:${sourceEnv.TEMPORAL_PORT ?? "7233"}`;
  // The focused platform profile talks to whatever runtime LIGHTSPEED_API_URL
  // names; the frontend-only loop is `npm run demo` (in-browser backend).
  const platformApiUrl = runtimeRpc;
  const runtimeAuthMode =
    sourceEnv.LIGHTSPEED_AUTH_MODE ?? (profile === "full" ? "authenticated" : "single");
  // Local environment daemon: a directly attached `lightspeed-envd` on the
  // developer machine (no provider; registered as an external environment).
  const envdEnabled =
    (profile === "runtime" || profile === "full") &&
    !cli.noEnvd &&
    (sourceEnv.LIGHTSPEED_DEV_ENVD ?? "on") !== "off";
  const envdListen = sourceEnv.LIGHTSPEED_ENVD_LISTEN ?? "127.0.0.1:19091";
  const envdPort = addressPort(envdListen, 19_091);
  const envdWorkspace =
    sourceEnv.LIGHTSPEED_DEV_ENVD_CWD ?? path.join(repoRoot, ".lightspeed-dev", "envd", "workspace");
  const envdEndpoint = `ws://${envdListen.startsWith(":") ? "127.0.0.1" : envdListen.split(":")[0]}:${envdPort}/`;
  const connectorNames = profile === "full" ? parseConnectors(sourceEnv.LIGHTSPEED_CHANNELS_CONNECTORS) : [];
  if (profile !== "full" && sourceEnv.LIGHTSPEED_CHANNELS_CONNECTORS?.trim()) {
    throw new TypeError("LIGHTSPEED_CHANNELS_CONNECTORS is supported only by the full development profile");
  }
  validateConnectorEnvironment(connectorNames, sourceEnv);

  const connectorHealthPort = positivePort(
    sourceEnv.LIGHTSPEED_CONNECTOR_HEALTH_PORT,
    8_090,
    "LIGHTSPEED_CONNECTOR_HEALTH_PORT",
  );
  const connectorMetricsPort = positivePort(
    sourceEnv.LIGHTSPEED_CONNECTOR_METRICS_PORT,
    9_090,
    "LIGHTSPEED_CONNECTOR_METRICS_PORT",
  );
  const healthUrls =
    connectorNames.length === 0 ? [] : [`http://127.0.0.1:${connectorHealthPort}`];
  const env = {
    ...sourceEnv,
    LIGHTSPEED_API_URL: platformApiUrl,
    LIGHTSPEED_AUTH_MODE: runtimeAuthMode,
    LIGHTSPEED_MCP_OAUTH_ALLOW_PRIVATE_NETWORKS:
      sourceEnv.LIGHTSPEED_MCP_OAUTH_ALLOW_PRIVATE_NETWORKS ?? "true",
    LIGHTSPEED_MCP_PRIVATE_NETWORKS:
      sourceEnv.LIGHTSPEED_MCP_PRIVATE_NETWORKS ?? "localhost,127.0.0.1,::1",
    LIGHTSPEED_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER_URL:
      sourceEnv.LIGHTSPEED_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER_URL ??
      (configuratorInternalTrustedHeader === "true" ? configuratorMcpUrl : ""),
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
    LIGHTSPEED_PLATFORM_DEV_SEED:
      sourceEnv.LIGHTSPEED_PLATFORM_DEV_SEED ??
      (profile === "full" && runtimeAuthMode === "authenticated" ? "true" : "false"),
    LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL:
      configuratorMcpUrl,
    LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_ALLOW_PRIVATE_NETWORK:
      sourceEnv.LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_ALLOW_PRIVATE_NETWORK ?? "true",
    LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER:
      configuratorInternalTrustedHeader,
    LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL:
      sourceEnv.LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL ?? runtimeRpc,
    TEMPORAL_ADDRESS: temporalAddress,
    ...(envdEnabled
      ? { LIGHTSPEED_PLATFORM_DEV_ENVD_ENDPOINT: envdEndpoint }
      : {}),
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
    if (envdEnabled) {
      mkdirSync(envdWorkspace, { recursive: true });
      ports.push({ name: "environment daemon", port: envdPort });
      readiness.push({ name: "environment daemon", port: envdPort });
      processes.push({
        name: "envd",
        command: "cargo",
        args: ["run", "-p", "environment-daemon", "--bin", "lightspeed-envd"],
        cwd: repoRoot,
        env: {
          ...env,
          LIGHTSPEED_ENVD_LISTEN: envdListen,
          LIGHTSPEED_ENVD_CWD: envdWorkspace,
          LIGHTSPEED_ENVD_STATE_DIR: path.join(envdWorkspace, "..", "state"),
        },
      });
    }
  }

  if (profile === "platform" || profile === "full") {
    ports.push({ name: "platform API", port: platformPort });
    ports.push({ name: "platform web", port: 5_173 });
    readiness.push(
      { name: "platform API", url: `http://127.0.0.1:${platformPort}/health` },
      { name: "platform web", url: "http://localhost:5173/app/" },
    );
    processes.push(
      {
        name: "platform",
        command: tsx,
        args: ["watch", "platform/server/src/main.ts"],
        cwd: repoRoot,
        env,
        // First-login bootstrap checks the canonical administrator through RPC.
        // Wait for the configured runtime in both full and platform-only loops.
        startAfter: {
          name: "runtime gateway",
          url: new URL("health", platformApiUrl).href,
        },
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

  // The demo is the web UI over its in-browser backend: no infrastructure,
  // no runtime, no Platform server — just Vite in demo mode.
  if (profile === "demo") {
    ports.push({ name: "demo web", port: 5_175 });
    readiness.push({ name: "demo web", url: "http://localhost:5175/demo/" });
    processes.push({
      name: "demo",
      command: vite,
      args: ["--mode", "demo", "--host", "localhost"],
      cwd: path.join(repoRoot, "platform", "web"),
      env,
    });
  }

  if (profile === "full") {
    ports.push({ name: "Configurator MCP", port: configuratorPort });
    readiness.push({ name: "Configurator MCP", url: `http://127.0.0.1:${configuratorPort}/health` });
    processes.splice(1, 0, {
      name: "configurator",
      command: tsx,
      args: ["platform/configurator-mcp/src/bin.ts"],
      cwd: repoRoot,
      env,
    });
    // Bots and Channels core run inside the Rust runtime. The only Node
    // worker left is the connector host: one process serving every enabled
    // Telegram/WhatsApp account it discovers through the core API.
    if (connectorNames.length > 0) {
      ports.push({ name: "connector host health", port: connectorHealthPort });
      ports.push({ name: "connector host metrics", port: connectorMetricsPort });
      readiness.push({
        name: "connector host",
        url: `http://127.0.0.1:${connectorHealthPort}/healthz`,
      });
      processes.push({
        name: "connectors",
        command: tsx,
        args: ["platform/connectors/src/host/main.ts"],
        cwd: repoRoot,
        env: {
          ...env,
          LIGHTSPEED_CONNECTOR_PROVIDERS: connectorNames.join(","),
          LIGHTSPEED_CONNECTOR_HEALTH_PORT: String(connectorHealthPort),
          LIGHTSPEED_CONNECTOR_METRICS_PORT: String(connectorMetricsPort),
          ...(connectorNames.includes("whatsapp") && !env.LIGHTSPEED_CONNECTOR_WHATSAPP_AUTH_DIR
            ? {
                LIGHTSPEED_CONNECTOR_WHATSAPP_AUTH_DIR: path.join(
                  repoRoot,
                  ".lightspeed-dev",
                  "whatsapp-auth",
                ),
              }
            : {}),
        },
        // Discovery needs the core API; wait for the gateway so a cold start
        // does not log a failed first pass.
        startAfter: {
          name: "runtime gateway",
          url: `http://127.0.0.1:${runtimePort}/health`,
        },
      });
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
    envd: envdEnabled ? { endpoint: envdEndpoint, workspace: envdWorkspace } : null,
    infra: profile !== "demo",
    tools:
      profile === "platform" || profile === "full" ? [tsx, vite] : profile === "demo" ? [vite] : [],
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

// Provider tokens are leased from the core (`auth/grants/lease`), so a
// Telegram connector leases provider credentials from core. WhatsApp keeps its Baileys
// session on disk and seals media locators with a deployment key.
function validateConnectorEnvironment(connectors, env) {
  if (connectors.length > 0 && !env.LIGHTSPEED_CONNECTOR_API_KEY?.trim()) {
    throw new TypeError("development connectors require LIGHTSPEED_CONNECTOR_API_KEY with scoped capabilities");
  }
  if (!connectors.includes("whatsapp")) return;
  const missing = ["LIGHTSPEED_CONNECTOR_WHATSAPP_MEDIA_LOCATOR_KEY"].filter(
    (name) => !env[name]?.trim(),
  );
  if (missing.length > 0) {
    throw new TypeError(`whatsapp development connector requires ${missing.join(", ")}`);
  }
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

function validateProviderCredentials(plan, requireApiKeys) {
  if (!["full", "runtime"].includes(plan.profile)) {
    if (requireApiKeys) {
      throw new TypeError("--require-api-keys applies only to the full and runtime profiles");
    }
    return;
  }
  const configured = [plan.env.OPENAI_API_KEY, plan.env.ANTHROPIC_API_KEY].some(
    (value) => value?.trim() && !value.startsWith("set_your_"),
  );
  if (configured) return;
  if (requireApiKeys) {
    throw new Error(
      "no OPENAI_API_KEY or ANTHROPIC_API_KEY is configured; set a deployment key in .env or drop --require-api-keys and add keys per universe under Integrations",
    );
  }
  console.warn(`
[credentials] No deployment-wide OPENAI_API_KEY or ANTHROPIC_API_KEY is configured.
[credentials] Starting anyway: add provider API keys per universe in the Platform UI
[credentials] (Settings -> Integrations). Sessions fail until a key exists for their provider.
[credentials] Pass --require-api-keys to make this fatal (for CI).`);
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

function waitForService(service) {
  return awaitService(service, { isStopping: () => stopping });
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

function printPlan(plan) {
  console.log(`profile: ${plan.profile}`);
  console.log(`infrastructure: ${plan.infra ? "postgres, pgadmin, minio, temporal" : "none"}`);
  for (const preparation of plan.preparations) {
    console.log(
      `prepare: ${preparation.name} -> ${displayCommand(preparation.command, preparation.args)}`,
    );
  }
  for (const processPlan of plan.processes) {
    console.log(
      `process: ${processPlan.name} -> ${displayCommand(processPlan.command, processPlan.args)}`,
    );
    if (processPlan.startAfter) {
      console.log(`  after: ${processPlan.startAfter.name} -> ${processPlan.startAfter.url ?? processPlan.startAfter.port}`);
    }
  }
  console.log(`connectors: ${plan.connectors.length > 0 ? plan.connectors.join(", ") : "none"}`);
  console.log(`runtime auth: ${plan.env.LIGHTSPEED_AUTH_MODE}`);
  console.log(`development fixtures: ${plan.env.LIGHTSPEED_PLATFORM_DEV_SEED}`);
  console.log(
    `environment daemon: ${plan.envd ? `${plan.envd.endpoint} (workspace ${plan.envd.workspace})` : "off"}`,
  );
}

function printRunning(plan) {
  const platform = plan.profile === "platform" || plan.profile === "full";
  console.log("\nLightspeed development stack is running:");
  console.log(`  profile       ${plan.profile}`);
  if (plan.profile === "runtime" || plan.profile === "full") {
    console.log(`  runtime       ${plan.env.LIGHTSPEED_API_URL}`);
    if (plan.envd) {
      console.log(`  envd          ${plan.envd.endpoint}  (attach: Environments -> Register external)`);
    }
  }
  if (platform) {
    console.log(`  platform API  http://127.0.0.1:${plan.env.PORT ?? "3000"}`);
    console.log("  web           http://localhost:5173/app/");
    console.log(
      `  login         ${plan.env.LIGHTSPEED_PLATFORM_ADMIN_EMAIL} / ${plan.env.LIGHTSPEED_PLATFORM_ADMIN_PASSWORD}`,
    );
    if (plan.env.LIGHTSPEED_PLATFORM_DEV_SEED === "true") {
      console.log("  Test logins   operator@lightspeed.dev, contributor@lightspeed.dev, viewer@lightspeed.dev");
      console.log("  initial password  same as Admin (existing passwords are preserved)");
    }
  }
  if (plan.profile === "demo") {
    console.log("  demo web      http://localhost:5175/demo/  (in-browser backend, scripted data, no sign-in)");
  }
  if (plan.connectors.length > 0) {
    console.log(`  connectors    ${plan.connectors.join(", ")}`);
  }
  console.log(
    plan.infra
      ? "\nPress Ctrl-C to stop host processes; infrastructure remains available.\n"
      : "\nPress Ctrl-C to stop.\n",
  );
}

function displayCommand(command, args) {
  const relative = path.isAbsolute(command) ? path.relative(repoRoot, command) : command;
  return [relative, ...args].join(" ");
}

function printHelp() {
  console.log(`Usage:
  ./dev.sh                                 Bootstrap and start the full editable product
  ./dev.sh [start] <profile>               Start full, platform, runtime, demo, or infra
  ./dev.sh [profile] --require-api-keys    Fail full/runtime startup without provider keys
                                           (default only warns; keys can be added per
                                           universe under Settings -> Integrations)
  ./dev.sh [profile] --no-envd             Do not start the local environment daemon
                                           (same as LIGHTSPEED_DEV_ENVD=off)
  ./dev.sh --plan <profile>                Print a profile without starting it
  ./dev.sh status                          Show host supervisor and infrastructure
  ./dev.sh stop                            Stop host processes; keep infrastructure
  ./dev.sh down [--volumes]                Stop host processes and infrastructure
  ./dev.sh reset                           Reset Postgres and MinIO development state

The npm run dev commands are aliases for the same launcher.

Profiles:
  full      Infrastructure, Rust runtime (with Bots and Channels core),
            Configurator, Platform, and web. LIGHTSPEED_CHANNELS_CONNECTORS
            optionally adds the connector host serving telegram and/or whatsapp.
  platform  Infrastructure, Platform API, and web UI against the runtime at
            LIGHTSPEED_API_URL (start one with the runtime profile).
  runtime   Infrastructure and the migrated Rust runtime.
  demo      Web UI only, on http://localhost:5175/demo/, over the in-browser
            demo backend (scripted data, no sign-in). No Docker, no runtime.
  infra     Postgres, pgAdmin, MinIO, and Temporal only.`);
}

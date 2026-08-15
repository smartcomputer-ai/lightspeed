#!/usr/bin/env node

// One product-level development supervisor.
//
// Stateful dependencies run in Docker Compose. Rust and TypeScript processes
// run from the checkout so cargo, tsx, and Vite retain their normal edit loops.
import { spawn, spawnSync } from "node:child_process";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

const devDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.join(devDir, "..");
try {
  process.loadEnvFile(path.join(repoRoot, ".env"));
} catch (error) {
  if (error?.code !== "ENOENT") throw error;
}
const profiles = new Set(["full", "platform", "runtime", "infra"]);
const actions = new Set(["start", "down", "reset", "status"]);
const cli = parseCli(process.argv.slice(2));

if (cli.help) {
  printHelp();
  process.exit(0);
}

const baseEnv = loadDevEnvironment();

if (cli.action !== "start") {
  runInfrastructureAction(cli, baseEnv);
  process.exit(0);
}

const plan = createPlan(cli.profile, baseEnv);
if (cli.planOnly) {
  printPlan(plan);
  process.exit(0);
}

ensureLocalTooling(plan);
runChecked("infra", path.join(devDir, "up.sh"), [], baseEnv);
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

const children = [];
let stopping = false;
let requestedExitCode = 0;

for (const processPlan of plan.processes) {
  startProcess(processPlan);
}

printRunning(plan);

process.once("SIGINT", () => shutdown(0));
process.once("SIGTERM", () => shutdown(0));

function parseCli(argv) {
  const args = [...argv];
  const help = removeFlag(args, "--help") || removeFlag(args, "-h");
  const planOnly = removeFlag(args, "--plan");
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

  return { action, profile, planOnly, help, volumes };
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
    ["-c", "source dev/env.sh >/dev/null && env -0"],
    { cwd: repoRoot, env: process.env, encoding: "buffer" },
  );
  if (result.status !== 0) {
    process.stderr.write(result.stderr ?? Buffer.from(""));
    throw new Error("could not load dev/env.sh");
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

  if (profile === "runtime" || profile === "full") {
    ports.push({ name: "runtime gateway", port: runtimePort });
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
        args: [],
        cwd: path.join(repoRoot, "platform", "web"),
        env,
      },
    );
  }

  if (profile === "full") {
    ports.push({ name: "Configurator MCP", port: configuratorPort });
    ports.push({ name: "Channels workflow metrics", port: 9_090 });
    ports.push({ name: "Channels activity metrics", port: 9_093 });
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
      ports.push({ name: `${connector} metrics`, port: metricsPort });
      ports.push({ name: `${connector} health`, port: connectorHealthPort(connector, env) });
      processes.push(channelsProcess(`channels-${connector}`, connector, metricsPort, env, tsx));
    }
  }

  return {
    profile,
    env,
    preparations,
    processes,
    ports: uniquePorts(ports),
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

function runInfrastructureAction(options, env) {
  if (options.action === "down") {
    runChecked("infra", path.join(devDir, "down.sh"), options.volumes ? ["-v"] : [], env);
    return;
  }
  if (options.action === "reset") {
    runChecked("infra", path.join(devDir, "reset.sh"), [], env);
    return;
  }
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
    console.log("  web           http://127.0.0.1:5173/app/");
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
  npm run dev                              Start the full editable product
  npm run dev -- [start] <profile>         Start full, platform, runtime, or infra
  npm run dev -- --plan <profile>          Print a profile without starting it
  npm run dev -- status                    Show development containers
  npm run dev -- down [--volumes]          Stop infrastructure
  npm run dev -- reset                     Reset Postgres and MinIO development state

Profiles:
  full      Infrastructure, Rust runtime, Configurator, Platform, web, and
            Channels workflow/activity workers. LIGHTSPEED_CHANNELS_CONNECTORS optionally
            adds telegram and/or whatsapp.
  platform  Infrastructure, stub gateway, Platform API, and web UI. Set
            LIGHTSPEED_PLATFORM_DEV_REAL_GATEWAY=1 to use an external runtime.
  runtime   Infrastructure and the migrated Rust runtime.
  infra     Postgres, pgAdmin, MinIO, and Temporal only.`);
}

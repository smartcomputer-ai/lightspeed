// One-command local dev: `npm run dev` (from app/).
//
// Layering: containers for state, host processes for code you edit.
//   [db]   compose postgres (started here if not already up)
//   [stub] stub Lightspeed gateway  (skip with LSBOT_DEV_REAL_GATEWAY=1)
//   [api]  tsx watch server         → http://localhost:3000  (+ built SPA at /app)
//   [web]  vite dev server (HMR)    → http://localhost:5173/app/
//
// Database credentials resolve from infra/.env when present. The app dev
// stack deliberately owns 15433 by default so it can run beside Lightspeed's
// local postgres on 15432. POSTGRES_PORT remains an explicit override.
import { spawn, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import pg from "pg";

const appDir = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
const infraDir = path.join(appDir, "..", "infra");

function readInfraEnv() {
  const values = {};
  try {
    for (const line of readFileSync(path.join(infraDir, ".env"), "utf8").split("\n")) {
      const match = /^\s*([A-Z0-9_]+)\s*=\s*(.*)\s*$/.exec(line);
      if (match) {
        values[match[1]] = match[2].replace(/^"|"$/g, "");
      }
    }
  } catch {
    // no infra/.env — compose defaults apply
  }
  return values;
}

const infraEnv = readInfraEnv();
const pgPort = process.env.POSTGRES_PORT ?? "15433";
const pgPassword = process.env.LSBOT_DB_PASSWORD ?? infraEnv.LSBOT_DB_PASSWORD ?? "change-me";
const realGateway = process.env.LSBOT_DEV_REAL_GATEWAY === "1";

const env = {
  ...process.env,
  LSBOT_DATABASE_URL:
    process.env.LSBOT_DATABASE_URL ??
    `postgres://lsbot:${pgPassword}@127.0.0.1:${pgPort}/lsbot`,
  LSBOT_AUTH_SECRET:
    process.env.LSBOT_AUTH_SECRET ?? "dev-only-secret-0123456789abcdef",
  LSBOT_ADMIN_EMAIL: process.env.LSBOT_ADMIN_EMAIL ?? "lukas@smartcomputer.company",
  LSBOT_ADMIN_PASSWORD: process.env.LSBOT_ADMIN_PASSWORD ?? "dev-password",
  LIGHTSPEED_API_URL:
    process.env.LIGHTSPEED_API_URL ??
    (realGateway ? "http://127.0.0.1:18080/rpc" : "http://127.0.0.1:19999/rpc"),
  LSBOT_CONFIGURATOR_MCP_URL:
    process.env.LSBOT_CONFIGURATOR_MCP_URL ?? "http://127.0.0.1:18081/mcp",
};

async function canConnect() {
  const client = new pg.Client({
    connectionString: env.LSBOT_DATABASE_URL,
    connectionTimeoutMillis: 1500,
  });
  try {
    await client.connect();
    await client.query("SELECT 1");
    return { ok: true };
  } catch (error) {
    return { ok: false, code: error.code, message: error.message };
  } finally {
    await client.end().catch(() => undefined);
  }
}

async function ensurePostgres() {
  const first = await canConnect();
  if (first.ok) {
    console.log(`[db] postgres ready on :${pgPort}`);
    return;
  }
  // TCP reachable but not our database → a different postgres owns the
  // explicitly selected port.
  if (first.code === "28P01" || /password|authentication|database/i.test(first.message ?? "")) {
    console.error(
      `[db] something answers on :${pgPort} but it is not the ls.bot postgres\n` +
        `     (${first.message})\n` +
        `     Choose an unused ls.bot development port, for example:\n` +
        `       POSTGRES_PORT=15434 npm run dev`,
    );
    process.exit(1);
  }
  console.log(`[db] postgres not reachable on :${pgPort} — starting compose postgres…`);
  const result = spawnSync(
    "docker",
    [
      "compose",
      "--project-name",
      "lsbot-dev",
      "-f",
      path.join(infraDir, "compose.yaml"),
      "up",
      "-d",
      "postgres",
    ],
    { stdio: "inherit", env: { ...process.env, POSTGRES_PORT: pgPort } },
  );
  if (result.status !== 0) {
    console.error(
      "[db] failed to start postgres. Start it yourself:\n" +
        "  docker compose -f infra/compose.yaml up -d postgres",
    );
    process.exit(1);
  }
  for (let i = 0; i < 30; i++) {
    if ((await canConnect()).ok) {
      console.log("[db] postgres is up");
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  const last = await canConnect();
  console.error(`[db] postgres did not become ready: ${last.message}`);
  process.exit(1);
}

const children = [];

function run(name, command, args, cwd) {
  const child = spawn(command, args, { cwd, env, shell: false });
  children.push(child);
  const prefix = `[${name}] `;
  const pipe = (stream, out) => {
    let buffer = "";
    stream.on("data", (chunk) => {
      buffer += chunk.toString();
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";
      for (const line of lines) {
        out.write(prefix + line + "\n");
      }
    });
  };
  pipe(child.stdout, process.stdout);
  pipe(child.stderr, process.stderr);
  child.on("exit", (code) => {
    console.log(`${prefix}exited (${code ?? "signal"})`);
  });
  return child;
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

await ensurePostgres();

// Fail fast when another dev stack (or anything else) holds our ports —
// a half-started stack is far more confusing than an early exit.
const needed = [
  [3000, "api"],
  [5173, "web"],
  ...(realGateway ? [] : [[19999, "stub gateway"]]),
];
for (const [port, what] of needed) {
  if (await tcpUp(port)) {
    console.error(
      `[dev] port ${port} (${what}) is already in use — is another dev stack running?`,
    );
    process.exit(1);
  }
}

if (!realGateway) {
  run("stub", "node", [path.join(appDir, "scripts", "stub-gateway.mjs")], appDir);
} else {
  console.log(`[stub] skipped — using real gateway at ${env.LIGHTSPEED_API_URL}`);
}
run("api", "npx", ["tsx", "watch", "server/src/main.ts"], appDir);
run("web", "npx", ["vite"], path.join(appDir, "web"));

console.log(`
  dev stack up:
    api   http://localhost:3000        (health: /health, SPA: /app if built)
    web   http://localhost:5173/app/   (HMR — use this one for UI work)
    login ${env.LSBOT_ADMIN_EMAIL} / ${env.LSBOT_ADMIN_PASSWORD}
`);

const shutdown = () => {
  for (const child of children) {
    child.kill("SIGTERM");
  }
  process.exit(0);
};
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);

// Private local token files shared by the runtime, operator CLI, and Platform.
import { chmodSync, existsSync, lstatSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { randomBytes, randomUUID } from "node:crypto";
import { DevError } from "./errors.mjs";

function writePrivate(file, text) {
  const dir = path.dirname(file);
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  if (lstatSync(dir).isSymbolicLink()) throw new DevError("Development credential directory must not be a symlink.");
  chmodSync(dir, 0o700);
  const temporary = `${file}.${randomUUID()}.tmp`;
  writeFileSync(temporary, text, { mode: 0o600, flag: "wx" });
  renameSync(temporary, file);
}
function readPrivate(file) {
  const stat = lstatSync(file);
  if (!stat.isFile() || (stat.mode & 0o077) !== 0) throw new DevError("Development credentials must be owner-only regular files.");
  return readFileSync(file, "utf8");
}

// Execution environments and frontend processes must not inherit API credentials.
export function processEnvironment(env, role) {
  return Object.fromEntries(Object.entries(env).filter(([name]) => {
    if (/^LIGHTSPEED_API_TOKEN(?:_|$)/.test(name)) return role === "runtime";
    if (/^LIGHTSPEED_PLATFORM_API_TOKEN(?:_|$)/.test(name)) return role === "platform";
    return true;
  }));
}

function validateToken(secret, source) {
  if (secret.length < 32 || /[^\x21-\x7e]/.test(secret)) {
    throw new DevError(`${source} must contain at least 32 printable ASCII characters without whitespace.`);
  }
  return secret;
}

// A mounted secret may have one final newline, as produced by an editor or echo.
function fileToken(contents) {
  return contents.endsWith("\r\n") ? contents.slice(0, -2)
    : contents.endsWith("\n") ? contents.slice(0, -1) : contents;
}

function readPrivateToken(file) {
  return validateToken(fileToken(readPrivate(file)), "Saved runtime token");
}

function configuredSecret(env, name) {
  const inline = env[name];
  const file = env[`${name}_FILE`];
  if (inline !== undefined && file !== undefined) throw new DevError(`Set ${name} or ${name}_FILE, not both.`);
  const secret = file !== undefined ? fileToken(readFileSync(file, "utf8")) : inline;
  return secret === undefined ? undefined : validateToken(secret, name);
}

export async function prepareCliConnection({ root, env, full, noBootstrap, run }) {
  const directory = path.join(root, ".lightspeed", "cli");
  const handoff = path.join(directory, "connection.json");
  const universe = env.LIGHTSPEED_PG_UNIVERSE_ID;
  await run("development universe", "cargo", ["run", "-p", "temporal-runtime", "--", "universe", "create", "--universe-id", universe, "--handle", "development"], env);
  if (env.LIGHTSPEED_AUTH_MODE === "single") {
    writePrivate(handoff, JSON.stringify({ endpoint: env.LIGHTSPEED_API_URL, single: true, credentialKind: "none", serverKind: "runtime", ownedCredential: false, universe }, null, 2));
    return { handoff, tokenEnvironment: {} };
  }
  if (env.LIGHTSPEED_AUTH_MODE !== "authenticated") throw new DevError("Expected single or authenticated runtime mode.");

  function token(kind, supplied) {
    const file = path.join(directory, `${kind}.token`);
    const saved = supplied === undefined && existsSync(file) ? readPrivateToken(file) : undefined;
    const secret = supplied ?? saved ?? (noBootstrap ? undefined : `lsr_${randomBytes(32).toString("base64url")}`);
    if (!secret) throw new DevError(`No ${kind} runtime token configured. Supply a token or omit --no-token-bootstrap.`);
    validateToken(secret, `The ${kind} runtime token`);
    writePrivate(file, secret);
    return { file, secret };
  }

  const namedCli = configuredSecret(env, "LIGHTSPEED_API_TOKEN_CLI");
  const defaultToken = configuredSecret(env, "LIGHTSPEED_API_TOKEN");
  const cli = token("cli", namedCli ?? defaultToken);
  const usesDefault = namedCli === undefined && defaultToken !== undefined;
  const tokenEnvironment = usesDefault
    ? { LIGHTSPEED_API_TOKEN_FILE: cli.file }
    : { LIGHTSPEED_API_TOKEN_CLI_FILE: cli.file };
  // Replace inline sources with private file paths so process plans never need secrets.
  delete env.LIGHTSPEED_API_TOKEN_CLI;
  if (full) {
    const platform = token("platform", configuredSecret(env, "LIGHTSPEED_PLATFORM_API_TOKEN") ?? configuredSecret(env, "LIGHTSPEED_API_TOKEN_PLATFORM"));
    // A shared secret has one runtime identity. Keep the CLI/default name;
    // Platform still gets its own protected consumer file.
    if (platform.secret !== cli.secret && platform.secret !== defaultToken) {
      tokenEnvironment.LIGHTSPEED_API_TOKEN_PLATFORM_FILE = platform.file;
    } else {
      delete env.LIGHTSPEED_API_TOKEN_PLATFORM_FILE;
    }
    tokenEnvironment.LIGHTSPEED_PLATFORM_API_TOKEN_FILE = platform.file;
    delete env.LIGHTSPEED_API_TOKEN_PLATFORM;
    delete env.LIGHTSPEED_PLATFORM_API_TOKEN;
  }
  // The default token, if used for the CLI, must not also appear under a second name.
  if (usesDefault) {
    delete env.LIGHTSPEED_API_TOKEN;
    delete env.LIGHTSPEED_API_TOKEN_CLI_FILE;
  }
  Object.assign(env, tokenEnvironment);
  writePrivate(handoff, JSON.stringify({ endpoint: env.LIGHTSPEED_API_URL, single: false, credentialKind: "runtimeToken", serverKind: "runtime", credentialFile: cli.file, ownedCredential: false, universe }, null, 2));
  return { handoff, tokenEnvironment };
}

// This is development fixture policy, never a server-side model fallback.
// Revision zero distinguishes untouched universes from deliberately cleared
// defaults. Seed only after runtime (and, for Test, Platform) readiness.
export async function seedDevelopmentModelDefaults({ env, handoff, full, fetch: request = globalThis.fetch }) {
  const single = env.LIGHTSPEED_AUTH_MODE === "single";
  const connection = handoff ? JSON.parse(readPrivate(handoff)) : null;
  const secret = single ? null : connection?.credentialFile
    ? readPrivateToken(connection.credentialFile) : configuredSecret(env, "LIGHTSPEED_PLATFORM_API_TOKEN");
  if (!single && !secret) return false;
  const universes = [env.LIGHTSPEED_PG_UNIVERSE_ID];
  if (full && env.LIGHTSPEED_PLATFORM_DEV_SEED === "true") universes.push("6c696768-7473-4065-8064-000000000010");
  for (const universe of new Set(universes)) {
    const headers = { "content-type": "application/json" };
    if (!single) {
      headers.authorization = `Bearer ${secret}`;
    }
    const endpoint = `${env.LIGHTSPEED_API_URL.replace(/\/$/, "")}/universes/${encodeURIComponent(universe)}/model-defaults`;
    async function call(method, params) {
      const url = method === "PUT" ? `${endpoint}/agentRun` : endpoint;
      const response = await request(url, {
        method, headers, redirect: "error", signal: AbortSignal.timeout(10_000),
        ...(params ? { body: JSON.stringify(params) } : {}),
      });
      // Another setup or user won the revision race. Their choice wins.
      if (method === "PUT" && response.status === 409) return;
      if (!response.ok) throw new DevError(`Development model setup failed (HTTP ${response.status}).`);
      return response.json();
    }
    const { defaults } = await call("GET");
    if (defaults.revision !== 0) continue;
    await call("PUT", {
      expectedRevision: 0,
      model: { providerId: "openai", apiKind: "openai:responses", model: "gpt-6-sol" },
    });
  }
  return true;
}

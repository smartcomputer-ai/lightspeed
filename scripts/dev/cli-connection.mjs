// Local credential handoff; all authority remains in the runtime key store.
import { chmodSync, existsSync, lstatSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { randomUUID } from "node:crypto";
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
  return readFileSync(file, "utf8").trim();
}

export async function prepareCliConnection({ root, env, full, noBootstrap, run }) {
  const directory = path.join(root, ".lightspeed", "cli");
  const handoff = path.join(directory, "connection.json");
  const statePath = path.join(directory, "provisioned.json");
  const state = existsSync(statePath) ? JSON.parse(readPrivate(statePath)) : {};
  const universe = env.LIGHTSPEED_PG_UNIVERSE_ID;
  await run("development universe", "cargo", ["run", "-p", "temporal-server", "--", "universe", "create", "--universe-id", universe, "--slug", "development"], env);
  if (env.LIGHTSPEED_AUTH_MODE === "single") {
    writePrivate(handoff, JSON.stringify({ endpoint: env.LIGHTSPEED_API_URL, single: true, ownedCredential: false }, null, 2));
    return { handoff };
  }
  if (env.LIGHTSPEED_AUTH_MODE !== "authenticated") throw new DevError("Expected single or authenticated runtime mode.");

  async function provision(kind, supplied, assertActor) {
    const file = path.join(directory, `${kind}.key`);
    const exists = existsSync(file);
    if (state[kind] && !exists) {
      throw new DevError(`The saved ${kind} development credential is missing.`, { hint: "Explicitly provision a replacement and restore its protected credential file; startup will not silently mint another administrator key." });
    }
    const previous = exists ? readPrivate(file) : null;
    if (previous && supplied && previous !== supplied.trim()) throw new DevError(`The supplied ${kind} key differs from the saved development key.`, { hint: "Explicitly provision the replacement and update the protected credential file." });
    const secret = previous ?? supplied?.trim();
    if (noBootstrap && !secret) return null;
    const args = ["run", "-p", "temporal-server", "--", "api-key", "provision", "--name", `Development ${kind}: ${root}`];
    if (assertActor) args.push("--assert-actor");
    if (exists || noBootstrap) args.push("--require-existing");
    const provisioningEnv = { ...env, RUST_LOG: "off" };
    delete provisioningEnv.LIGHTSPEED_BOOTSTRAP_API_KEY;
    if (secret) provisioningEnv.LIGHTSPEED_BOOTSTRAP_API_KEY = secret;
    const result = await run(`development ${kind} key`, "cargo", args, provisioningEnv, { captureStdout: true });
    let credential;
    try {
      credential = JSON.parse(result.stdout);
      if (typeof credential.secret !== "string" || !credential.secret.startsWith("lsk_")) throw new Error();
    } catch {
      throw new DevError("API-key provisioning returned an invalid credential response; secret output withheld.");
    }
    writePrivate(file, credential.secret);
    state[kind] = true;
    writePrivate(statePath, JSON.stringify(state));
    return { file, secret: credential.secret };
  }

  const cli = await provision("cli", env.LIGHTSPEED_BOOTSTRAP_API_KEY, false);
  let platformSecret = env.LIGHTSPEED_PLATFORM_API_KEY;
  if (full && !platformSecret) {
    const platform = await provision("platform", undefined, true);
    if (!platform) throw new DevError("Platform needs LIGHTSPEED_PLATFORM_API_KEY when --no-api-key-bootstrap is set.");
    platformSecret = platform.secret;
  }
  if (cli) {
    writePrivate(handoff, JSON.stringify({ endpoint: env.LIGHTSPEED_API_URL, single: false, credentialFile: cli.file, ownedCredential: false, universe }, null, 2));
  } else {
    // Replace any old single-mode handoff so it cannot be imported as current.
    writePrivate(handoff, JSON.stringify({ endpoint: env.LIGHTSPEED_API_URL, single: false, ownedCredential: false }, null, 2));
  }
  return { handoff: cli ? handoff : null, platformSecret };
}

// This is development fixture policy, never a server-side model fallback.
// Revision zero distinguishes untouched universes from deliberately cleared
// defaults. Seed only after runtime (and, for Test, Platform) readiness.
export async function seedDevelopmentModelDefaults({ env, handoff, full, fetch: request = globalThis.fetch }) {
  const single = env.LIGHTSPEED_AUTH_MODE === "single";
  const connection = handoff ? JSON.parse(readPrivate(handoff)) : null;
  const secret = single ? null : connection?.credentialFile
    ? readPrivate(connection.credentialFile) : env.LIGHTSPEED_PLATFORM_API_KEY;
  if (!single && !secret) return false;
  const universes = [env.LIGHTSPEED_PG_UNIVERSE_ID];
  if (full && env.LIGHTSPEED_PLATFORM_DEV_SEED === "true") universes.push("6c696768-7473-4065-8064-000000000010");
  for (const universe of new Set(universes)) {
    const headers = { "content-type": "application/json" };
    if (!single) {
      headers.authorization = `Bearer ${secret}`;
      headers["x-lightspeed-universe"] = universe;
    }
    async function call(method, params) {
      const response = await request(env.LIGHTSPEED_API_URL, {
        method: "POST", headers, redirect: "error", signal: AbortSignal.timeout(10_000),
        body: JSON.stringify({ jsonrpc: "2.0", id: "development-model-defaults", method, params }),
      });
      if (!response.ok) throw new DevError(`Development model setup failed (HTTP ${response.status}).`);
      const rpc = await response.json();
      if (rpc.error) {
        // Another setup or user won the revision race. Their choice wins.
        if (method === "models/defaults/put" && rpc.error.data?.kind === "conflict") return;
        throw new DevError(`Development model setup failed for ${method}.`, { hint: "Check that the development key has models access, then configure defaults with the CLI." });
      }
      return rpc.result.result;
    }
    const { defaults } = await call("models/defaults/read", {});
    if (defaults.revision !== 0) continue;
    await call("models/defaults/put", {
      slot: "agentRun", expectedRevision: 0,
      model: { providerId: "openai", apiKind: "openai:responses", model: "gpt-6-sol" },
    });
  }
  return true;
}

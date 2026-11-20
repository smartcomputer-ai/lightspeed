/// <reference types="node" />

import { randomUUID } from "node:crypto";
/** Node-only import of the CLI's saved connection; never bundled by the browser entry. */
import { constants } from "node:fs";
import { open, lstat, realpath, rename, unlink, type FileHandle } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { LightspeedClient, type LightspeedClientOptions } from "./client.js";
import { normalizeEndpoint } from "./endpoint.js";
import { HTTP_CONTRACT_REVISION } from "./generated/http.js";

type CredentialKind = "none" | "runtimeToken" | "platformApiKey" | "platformSession";
interface SavedConnection {
  endpoint: string;
  single: boolean;
  credentialKind: CredentialKind;
  serverKind: "runtime" | "platform";
  credentialFile?: string;
  ownedCredential?: boolean;
  organizationId?: string;
  universe?: string;
  allowInsecureHttp?: boolean;
}
export interface ConnectionClientOptions {
  /** Named connection; defaults to the CLI's selected connection. */
  name?: string;
  /** Defaults to LIGHTSPEED_CONFIG_DIR, then XDG_CONFIG_HOME/lightspeed or ~/.config/lightspeed. */
  configDir?: string;
  /** Invocation-only selection; never rewrites the CLI's saved metadata. */
  universe?: string;
  /** A different organization clears the saved universe unless universe is explicitly supplied. */
  organization?: string;
  fetch?: typeof globalThis.fetch;
}

const rotations = new Map<string, Promise<void>>();
function record(value: unknown): value is Record<string, unknown> { return value !== null && typeof value === "object" && !Array.isArray(value); }
function nonempty(value: unknown): value is string { return typeof value === "string" && value.length > 0; }
function ownedByCurrentUser(uid: number) { return typeof process.getuid !== "function" || uid === process.getuid(); }
function privateMode(mode: number) { return process.platform === "win32" || (mode & 0o077) === 0; }

async function credentialHandle(path: string): Promise<FileHandle> {
  const before = await lstat(path);
  if (!before.isFile() || !privateMode(before.mode) || !ownedByCurrentUser(before.uid)) throw new Error("Credential must be a regular owner-only file (chmod 600)");
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const after = await handle.stat();
    if (!after.isFile() || !privateMode(after.mode) || !ownedByCurrentUser(after.uid) || before.ino !== after.ino || before.dev !== after.dev) throw new Error("Credential file changed while opening");
    return handle;
  } catch (error) { await handle.close(); throw error; }
}
async function tokenFromHandle(handle: FileHandle): Promise<string> {
  const stat = await handle.stat();
  if (stat.size > 4096) throw new Error("Invalid credential file");
  const bytes = Buffer.alloc(4097);
  const { bytesRead } = await handle.read(bytes, 0, bytes.length, 0);
  const token = new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, bytesRead)).trim();
  if (!token || token.length > 4096 || /[^\x21-\x7e]/.test(token)) throw new Error("Invalid credential file");
  return token;
}
async function readCredential(path: string): Promise<string> {
  const handle = await credentialHandle(path);
  try { return await tokenFromHandle(handle); } finally { await handle.close(); }
}
async function readMetadata(path: string): Promise<unknown> {
  const before = await lstat(path);
  if (!before.isFile() || !ownedByCurrentUser(before.uid) || process.platform !== "win32" && (before.mode & 0o022) !== 0) throw new Error("Connection metadata must be a regular file writable only by its owner");
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const after = await handle.stat();
    if (after.ino !== before.ino || after.dev !== before.dev || !after.isFile() || !ownedByCurrentUser(after.uid) || process.platform !== "win32" && (after.mode & 0o022) !== 0) throw new Error("Connection metadata changed while opening");
    return JSON.parse(await handle.readFile("utf8")) as unknown;
  } finally { await handle.close(); }
}
function missing(error: unknown) { return record(error) && error.code === "ENOENT"; }

/** Readers share the CLI's atomic replacement convention. Compare the request's
 * credential before writing; metadata checks keep a concurrent logout/relogin
 * from reattaching this factory to another saved credential. */
async function rotateCredential(path: string, dir: string, token: string, previous: string, binding: () => Promise<void>): Promise<void> {
  if (token === previous) return;
  const previousRotation = rotations.get(path) ?? Promise.resolve();
  const pending = previousRotation.catch(() => {}).then(async () => {
    await binding();
    let handle: FileHandle;
    try { handle = await credentialHandle(path); } catch (error) { if (missing(error)) return; throw error; }
    const temporary = join(dirname(path), `.${randomUUID()}.tmp`);
    let temporaryExists = false;
    try {
      if (await realpath(dirname(path)) !== join(await realpath(dir), "credentials")) throw new Error("Owned session credential directory must not redirect outside the CLI configuration");
      if (await tokenFromHandle(handle) !== previous) return;
      const original = await handle.stat();
      const output = await open(temporary, "wx", 0o600);
      temporaryExists = true;
      let installed;
      try { await output.writeFile(token, "utf8"); await output.sync(); installed = await output.stat(); }
      finally { await output.close(); }
      await binding();
      let current;
      try { current = await lstat(path); } catch (error) { if (missing(error)) return; throw error; }
      if (!current.isFile() || current.ino !== original.ino || current.dev !== original.dev || await readCredential(path) !== previous) return;
      await rename(temporary, path);
      temporaryExists = false;
      try { await binding(); }
      catch (error) {
        // Logout writes metadata before removing the owned file. A relogin uses a
        // new file. Remove only this rotation's orphan, never another replacement.
        let referenced = true;
        try {
          const current = await readMetadata(join(dir, "connections.json"));
          if (record(current) && record(current.connections)) referenced = Object.values(current.connections).some(entry => record(entry) && typeof entry.credentialFile === "string" && resolve(entry.credentialFile) === resolve(path));
        } catch { /* Unreadable metadata cannot prove the credential is orphaned. */ }
        const linked = await lstat(path).catch(() => undefined);
        if (!referenced && linked?.ino === installed.ino && linked.dev === installed.dev) await unlink(path).catch(() => {});
        throw error;
      }
    } finally { await handle.close(); if (temporaryExists) await unlink(temporary).catch(() => {}); }
  });
  rotations.set(path, pending);
  try { await pending; } finally { if (rotations.get(path) === pending) rotations.delete(path); }
}

function savedConnection(value: unknown): SavedConnection {
  if (!record(value) || !nonempty(value.endpoint) || typeof value.single !== "boolean" ||
      !["none", "runtimeToken", "platformApiKey", "platformSession"].includes(String(value.credentialKind)) ||
      !["runtime", "platform"].includes(String(value.serverKind))) throw new Error("Invalid saved connection metadata; save it again with the CLI");
  for (const field of ["credentialFile", "organizationId", "universe"] as const) if (value[field] != null && !nonempty(value[field])) throw new Error(`Invalid saved connection ${field}`);
  for (const field of ["ownedCredential", "allowInsecureHttp"] as const) if (value[field] !== undefined && typeof value[field] !== "boolean") throw new Error(`Invalid saved connection ${field}`);
  const saved = { ...value } as unknown as SavedConnection;
  for (const field of ["credentialFile", "organizationId", "universe"] as const) if (value[field] === null) delete saved[field];
  if ((saved.credentialKind === "none") !== saved.single || (saved.single ? saved.credentialFile != null : !saved.credentialFile)) throw new Error("Saved connection credential does not match its authentication mode");
  if ((saved.serverKind === "runtime") !== ["none", "runtimeToken"].includes(saved.credentialKind)) throw new Error("Saved credential kind does not match the deployment kind");
  if (saved.credentialFile && !isAbsolute(saved.credentialFile)) throw new Error("Saved credential path must be absolute");
  return saved;
}

/** Read a named CLI connection and verify current server authority. This never
 * starts login, borrows environment credentials, or changes saved selections. */
export async function createClientFromConnection(options: ConnectionClientOptions = {}): Promise<LightspeedClient> {
  const dir = resolve(options.configDir ?? process.env.LIGHTSPEED_CONFIG_DIR ?? join(process.env.XDG_CONFIG_HOME ?? join(homedir(), ".config"), "lightspeed"));
  let config: unknown;
  try { config = await readMetadata(join(dir, "connections.json")); }
  catch { throw new Error("Cannot read saved CLI connections; use lightspeed connect add or lightspeed connect login first"); }
  if (!record(config) || !record(config.connections)) throw new Error("Invalid saved connections");
  const name = options.name ?? config.selected;
  if (!nonempty(name) || !Object.hasOwn(config.connections, name)) throw new Error("No matching saved CLI connection");
  const saved = savedConnection(config.connections[name]);
  const endpoint = normalizeEndpoint(saved.endpoint);
  const url = new URL(endpoint);
  const host = url.hostname.replace(/^\[|\]$/g, "");
  const loopback = host === "localhost" || host === "::1" || /^127(?:\.\d{1,3}){3}$/.test(host);
  if (url.protocol !== "https:" && !loopback && !saved.allowInsecureHttp) throw new Error("Saved deployment requires HTTPS or an explicit CLI insecure-HTTP opt-in");
  const organizationId = options.organization ?? saved.organizationId;
  if (organizationId !== undefined && (!organizationId.trim() || saved.serverKind !== "platform")) throw new Error("Organization selection requires a Platform deployment and a nonempty organization ID");
  let universe = options.universe ?? (options.organization !== undefined && options.organization !== saved.organizationId ? undefined : saved.universe);
  if (universe !== undefined && !universe.trim()) throw new Error("Universe selection must not be empty");
  const file = saved.credentialFile;
  if (file) await readCredential(file); // Refuse insecure input before any network request.
  const ownedSession = saved.credentialKind === "platformSession" && saved.ownedCredential === true;
  if (ownedSession && (!file || dirname(resolve(file)) !== join(dir, "credentials"))) throw new Error("Owned CLI sessions must be stored in the CLI credential directory");
  const binding = async () => {
    let current: unknown;
    try { current = await readMetadata(join(dir, "connections.json")); }
    catch { throw new Error("Saved connection changed or was removed"); }
    if (!record(current) || !record(current.connections) || !Object.hasOwn(current.connections, name)) throw new Error("Saved connection changed or was removed");
    const next = savedConnection(current.connections[name]);
    if (normalizeEndpoint(next.endpoint) !== endpoint || next.credentialKind !== saved.credentialKind || next.serverKind !== saved.serverKind ||
        next.single !== saved.single || next.credentialFile !== saved.credentialFile || !!next.ownedCredential !== !!saved.ownedCredential ||
        !!next.allowInsecureHttp !== !!saved.allowInsecureHttp) throw new Error("Saved connection changed; create a new client to use it");
  };
  let verified = false;
  let initialRotation: { token: string; previous: string } | undefined;
  const clientOptions: LightspeedClientOptions = {
    endpoint,
    token: async () => { const pending = file ? rotations.get(file) : undefined; if (pending) await pending; await binding(); return file ? readCredential(file) : undefined; },
    ...(organizationId === undefined ? {} : { organizationId }),
    ...(universe === undefined ? {} : { universe }),
    ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
    ...(ownedSession && file ? { onSessionToken: async (token: string, previous: string) => { if (!verified) { initialRotation = { token, previous }; return; } await rotateCredential(file, dir, token, previous, binding); } } : {}),
  };
  let client = new LightspeedClient(clientOptions);
  const discovery: unknown = await client.call("initialize", {});
  if (!record(discovery) || discovery.contractRevision !== HTTP_CONTRACT_REVISION || discovery.runtime !== saved.serverKind ||
      !record(discovery.caller) || discovery.caller.single !== saved.single) throw new Error("Saved connection does not match deployment discovery");
  if (saved.serverKind === "platform") {
    const platform = discovery.platform;
    const kind = record(platform) && record(platform.credential) ? platform.credential.kind : undefined;
    if (saved.credentialKind === "platformSession" ? kind !== "cliSession" : kind !== "personalKey" && kind !== "serviceUserKey") throw new Error("Saved credential kind does not match authenticated Platform authority");
  }
  verified = true;
  if (initialRotation && file) await rotateCredential(file, dir, initialRotation.token, initialRotation.previous, binding);
  if (universe === undefined && (!organizationId || saved.serverKind === "runtime") && nonempty(discovery.defaultUniverse)) {
    universe = discovery.defaultUniverse;
    client = new LightspeedClient({ ...clientOptions, universe });
  }
  return client;
}

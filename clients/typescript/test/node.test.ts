import { chmod, lstat, mkdir, mkdtemp, readFile, rename, rm, symlink, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it, vi } from "vitest";
import { createClientFromConnection } from "../src/node.js";
import { HTTP_CONTRACT_REVISION } from "../src/server.js";

const directories: string[] = [];
afterEach(async () => { vi.unstubAllEnvs(); vi.restoreAllMocks(); await Promise.all(directories.splice(0).map(dir => rm(dir, { recursive: true, force: true }))); });
async function fixture(overrides: Record<string, unknown> = {}) {
  const dir = await mkdtemp(join(tmpdir(), "lightspeed-sdk-")); directories.push(dir);
  await mkdir(join(dir, "credentials"), { mode: 0o700 });
  const file = join(dir, "credentials", "session");
  await writeFile(file, "initial-secret", { mode: 0o600 });
  const saved = { endpoint: "https://platform.test/team/api/v1", single: false, serverKind: "platform", credentialKind: "platformSession", credentialFile: file, ownedCredential: true, accountId: "account-a", universe: "universe-a", ...overrides };
  const config = { selected: "test", connections: { test: saved } };
  const path = join(dir, "connections.json");
  const save = () => writeFile(path, JSON.stringify(config), { mode: 0o600 });
  await save();
  return { dir, file, path, saved, config, save };
}
function discovery(kind = "cliSession", extra: Record<string, unknown> = {}) {
  return Response.json({ runtime: "platform", contractRevision: HTTP_CONTRACT_REVISION, caller: { single: false }, platform: { principal: { kind: "user", id: "person", name: "Ada", platformAdmin: false }, credential: { kind, id: "key" }, accounts: [], restrictions: {} }, ...extra });
}
const auth = (init?: RequestInit) => new Headers(init?.headers).get("authorization");
const denied = (token: string) => Response.json({ type: "about:blank", title: "Forbidden", status: 403, kind: "forbidden", detail: "Outside scope" }, { status: 403, headers: { "set-auth-token": token } });

it("imports and verifies a CLI session without borrowing environment credentials", async () => {
  const state = await fixture(); vi.stubEnv("LIGHTSPEED_API_TOKEN", "ambient-secret"); vi.stubEnv("LIGHTSPEED_API_URL", "https://foreign.test");
  const calls: [string, string | null][] = [];
  const fetch = vi.fn<typeof globalThis.fetch>(async (url, init) => {
    expect(init?.redirect).toBe("manual"); calls.push([String(url), auth(init)]);
    return String(url).endsWith("/connection") ? discovery() : Response.json({ profiles: [] });
  });
  const client = await createClientFromConnection({ configDir: state.dir, fetch });
  expect(client.accountId).toBe("account-a");
  await client.call("profiles/list", {});
  expect(calls).toEqual([["https://platform.test/team/api/v1/connection", "Bearer initial-secret"], ["https://platform.test/team/api/v1/universes/universe-a/profiles", "Bearer initial-secret"]]);
  expect(await readFile(state.path, "utf8")).not.toContain("initial-secret");
});
it("atomically persists renewal even when the operation is denied, then reuses it for a download", async () => {
  const state = await fixture(); const before = await lstat(state.file);
  const fetch = vi.fn<typeof globalThis.fetch>(async (url, init) => {
    if (String(url).endsWith("/connection")) return discovery();
    if (String(url).endsWith("/platform/accounts")) return denied("renewed-secret");
    expect(auth(init)).toBe("Bearer renewed-secret"); return new Response(new Uint8Array([1, 2]));
  });
  const client = await createClientFromConnection({ configDir: state.dir, fetch });
  await expect(client.platform.listAccounts()).rejects.toMatchObject({ status: 403 });
  expect(await readFile(state.file, "utf8")).toBe("renewed-secret");
  if (process.platform !== "win32") expect((await lstat(state.file)).mode & 0o777).toBe(0o600);
  expect((await lstat(state.file)).ino).not.toBe(before.ino);
  expect(new Uint8Array(await (await client.openDownload("blobs/read", { blobRef: "blob" })).arrayBuffer())).toEqual(new Uint8Array([1, 2]));
});
it("rereads credentials and stops before sending when logout or a changed connection invalidates the binding", async () => {
  const state = await fixture(); const fetch = vi.fn<typeof globalThis.fetch>(async (url, init) => {
    if (String(url).endsWith("/connection")) return discovery(); expect(auth(init)).toBe("Bearer replacement-secret"); return Response.json({ profiles: [] });
  });
  const client = await createClientFromConnection({ configDir: state.dir, fetch });
  const replacement = join(state.dir, "replacement"); await writeFile(replacement, "replacement-secret", { mode: 0o600 }); await rename(replacement, state.file);
  await client.call("profiles/list", {});
  state.saved.endpoint = "https://other.test/api/v1"; await state.save();
  await expect(client.call("profiles/list", {})).rejects.toThrow("Saved connection changed");
  expect(fetch).toHaveBeenCalledTimes(2);
});
it("does not recreate a credential after logout while a renewal is in flight", async () => {
  const state = await fixture(); const fetch = vi.fn<typeof globalThis.fetch>(async url => {
    if (String(url).endsWith("/connection")) return discovery();
    delete (state.saved as Record<string, unknown>).credentialFile; state.saved.credentialKind = "none"; await state.save(); await unlink(state.file);
    return denied("renewed-secret");
  });
  const client = await createClientFromConnection({ configDir: state.dir, fetch });
  await expect(client.platform.listAccounts()).rejects.toThrow();
  await expect(lstat(state.file)).rejects.toMatchObject({ code: "ENOENT" });
});
it("never renews imported credential files, API keys, or overridden authorization", async () => {
  for (const variant of ["external", "api-key", "override"]) {
    const state = await fixture(variant === "external" ? { ownedCredential: false } : variant === "api-key" ? { credentialKind: "platformApiKey" } : {});
    const client = await createClientFromConnection({ configDir: state.dir, fetch: async url => String(url).endsWith("/connection") ? discovery(variant === "api-key" ? "personalKey" : "cliSession") : denied("wrong-secret") });
    await expect(client.platform.listAccounts({}, variant === "override" ? { headers: { authorization: "Bearer different-secret" } } : {})).rejects.toThrow();
    expect(await readFile(state.file, "utf8")).toBe("initial-secret");
  }
});
it("ignores stale renewal responses after another request already rotated the session", async () => {
  const state = await fixture(); let finishFirst!: (response: Response) => void;
  const first = new Promise<Response>(resolve => { finishFirst = resolve; }); let sent = 0;
  const client = await createClientFromConnection({ configDir: state.dir, fetch: async url => {
    if (String(url).endsWith("/connection")) return discovery(); return ++sent === 1 ? first : denied("newest-secret");
  } });
  const pending = client.platform.listAccounts().catch(() => {});
  await vi.waitFor(() => expect(sent).toBe(1));
  await expect(client.platform.listAccounts()).rejects.toThrow(); finishFirst(denied("stale-secret")); await pending;
  expect(await readFile(state.file, "utf8")).toBe("newest-secret");
});
it("does not mutate credentials when discovery reports a mismatched kind", async () => {
  const state = await fixture(); const response = discovery("personalKey"); response.headers.set("set-auth-token", "wrong-secret");
  await expect(createClientFromConnection({ configDir: state.dir, fetch: async () => response })).rejects.toThrow("credential kind");
  expect(await readFile(state.file, "utf8")).toBe("initial-secret");
});
it("clears an inherited universe on account override and does not use an unrelated discovery default", async () => {
  const state = await fixture(); const fetch = vi.fn<typeof globalThis.fetch>(async () => discovery("cliSession", { defaultUniverse: "universe-a" }));
  const client = await createClientFromConnection({ configDir: state.dir, account: "account-b", fetch });
  expect(client.accountId).toBe("account-b"); await expect(client.call("profiles/list", {})).rejects.toThrow("universe"); expect(fetch).toHaveBeenCalledTimes(1);
});
it("discovers a single runtime universe without adding credentials", async () => {
  const state = await fixture({ serverKind: "runtime", single: true, credentialKind: "none", credentialFile: undefined, ownedCredential: false, accountId: undefined, universe: undefined, endpoint: "http://localhost:18080" });
  const fetch = vi.fn<typeof globalThis.fetch>(async (url, init) => { expect(auth(init)).toBeNull(); return String(url).endsWith("/connection") ? Response.json({ runtime: "runtime", contractRevision: HTTP_CONTRACT_REVISION, caller: { single: true }, defaultUniverse: "local" }) : Response.json({ profiles: [] }); });
  const client = await createClientFromConnection({ configDir: state.dir, fetch }); await client.call("profiles/list", {});
  expect(String(fetch.mock.calls[1]![0])).toBe("http://localhost:18080/api/v1/universes/local/profiles");
});
it("rejects insecure files, symlinks, changed metadata, and unapproved remote HTTP before sending", async () => {
  const fetch = vi.fn<typeof globalThis.fetch>();
  const http = await fixture({ endpoint: "http://remote.test/api/v1" }); await expect(createClientFromConnection({ configDir: http.dir, fetch })).rejects.toThrow("HTTPS");
  const linked = await fixture(); const target = join(linked.dir, "target"); await rename(linked.file, target); await symlink(target, linked.file);
  await expect(createClientFromConnection({ configDir: linked.dir, fetch })).rejects.toThrow("regular owner-only");
  if (process.platform !== "win32") {
    const publicSecret = await fixture(); await chmod(publicSecret.file, 0o644); await expect(createClientFromConnection({ configDir: publicSecret.dir, fetch })).rejects.toThrow("owner-only");
    const writableConfig = await fixture(); await chmod(writableConfig.path, 0o666); await expect(createClientFromConnection({ configDir: writableConfig.dir, fetch })).rejects.toThrow("Cannot read");
  }
  expect(fetch).not.toHaveBeenCalled();
});

import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { gatewayRoutes } from "./gateway.js";
const auth = vi.hoisted(() => ({ role: "contributor" }));
vi.mock("./universes.js", () => ({ universeForSession: vi.fn(async (_ctx, _c, id: string) => ({
  universe: { lightspeedUniverseId: id, gatewayUrl: "https://engine.example/rpc" },
  slug: "test", role: auth.role, member: { userId: "member", role: auth.role },
})) }));
beforeEach(() => { auth.role = "contributor"; });
afterEach(() => vi.unstubAllGlobals());
const audio = { blobRef: `sha256:${"a".repeat(64)}`, mime: "audio/webm", name: "dictation.webm" };
function fixture() {
  const requests: { method: string; params: Record<string, unknown> }[] = [];
  const fetch = vi.fn(async (_url: unknown, init: RequestInit) => {
    expect(new Headers(init.headers).get("x-lightspeed-universe")).toBe("universe");
    expect(new Headers(init.headers).get("x-lightspeed-actor")).toBe("member");
    const rpc = JSON.parse(String(init.body));
    requests.push(rpc);
    return Response.json({ id: rpc.id, result: { result: rpc.method === "blobs/put" ? { blobs: [{ blobRef: audio.blobRef, bytes: 5 }] } : { transcription: { transcriptionId: "job", status: "running" } }, notifications: [] } });
  });
  vi.stubGlobal("fetch", fetch);
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => { c.set("session", { user: { id: "member" } } as ApiVariables["session"]); await next(); });
  app.route("/", gatewayRoutes({ env: { lightspeedApiUrl: "https://engine.example/rpc", lightspeedApiKey: "lsk_fixture" } } as AppContext));
  const call = (method: string, path = "", body?: unknown) => app.request(`/universe/transcriptions${path}`, {
    method, headers: { "content-type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  return { call, requests, fetch };
}
it("lets contributors upload, transcribe, inspect, and cancel under their own identity", async () => {
  const f = fixture();
  expect(await (await f.call("POST", "/audio", { bytesBase64: btoa("audio") })).json()).toMatchObject({ blobs: [{ blobRef: audio.blobRef }] });
  expect(await (await f.call("POST", "", { audio, idempotencyKey: "request" })).json()).toMatchObject({ transcriptionId: "job" });
  expect((await f.call("GET", "/job")).status).toBe(200);
  expect((await f.call("POST", "/job/cancel")).status).toBe(200);
  expect(f.requests.map(({ method, params }) => ({ method, params }))).toEqual([
    { method: "blobs/put", params: { blobs: [{ bytesBase64: btoa("audio") }] } },
    { method: "transcriptions/start", params: { audio, idempotencyKey: "request" } },
    { method: "transcriptions/read", params: { transcriptionId: "job" } },
    { method: "transcriptions/cancel", params: { transcriptionId: "job" } },
  ]);
});
it("rejects viewer mutations before reaching the runtime", async () => {
  auth.role = "viewer";
  const f = fixture();
  expect((await f.call("POST", "/audio", { bytesBase64: btoa("audio") })).status).toBe(403);
  expect((await f.call("POST", "", { audio, idempotencyKey: "request" })).status).toBe(403);
  expect((await f.call("POST", "/job/cancel")).status).toBe(403);
  expect(f.fetch).not.toHaveBeenCalled();
});
it("rejects explicit model overrides and malformed audio before reaching the runtime", async () => {
  const f = fixture();
  expect((await f.call("POST", "", { audio, idempotencyKey: "request", model: { providerId: "other", apiKind: "openai:audio-transcriptions", model: "override" } })).status).toBe(400);
  expect((await f.call("POST", "/audio", { bytesBase64: "!!!" })).status).toBe(400);
  expect(f.fetch).not.toHaveBeenCalled();
});
it("preserves the missing speech default error", async () => {
  const f = fixture();
  f.fetch.mockImplementation(async (_url, init) => {
    const rpc = JSON.parse(String(init.body));
    return Response.json({ id: rpc.id, error: { code: -32014, message: "Choose a speech model", data: { kind: "model_default_unset", message: "Choose a speech model", modelDefaultSlot: "speechToText" } } });
  });
  const response = await f.call("POST", "", { audio, idempotencyKey: "request" });
  expect(response.status).toBe(400);
  expect(await response.json()).toMatchObject({ kind: "model_default_unset", modelDefaultSlot: "speechToText" });
});

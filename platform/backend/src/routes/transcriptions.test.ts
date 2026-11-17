import { userSubject } from "../../test/subject-fixture.js";
import { decodeHttpRequest, problemResponse } from "../../../test/http-fixture.js";
import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { gatewayRoutes } from "./gateway.js";
const auth = vi.hoisted(() => ({ role: "contributor" }));
vi.mock("./universes.js", () => ({ universeForSession: vi.fn(async (_ctx, _c, id: string) => ({
  universe: { lightspeedUniverseId: id, gatewayUrl: "https://engine.example/api/v1" },
  handle: "test", role: auth.role, member: { userId: "member", role: auth.role },
})) }));
beforeEach(() => { auth.role = "contributor"; });
afterEach(() => vi.unstubAllGlobals());
const audio = { blobRef: `sha256:${"a".repeat(64)}`, mime: "audio/webm", name: "dictation.webm" };
function fixture() {
  const requests: { method: string; params: Record<string, unknown> }[] = [];
  const fetch = vi.fn(async (_url: unknown, init: RequestInit) => {
    expect(decodeHttpRequest(_url as string | Request | URL, init).universe).toBe("universe");
    expect(new Headers(init.headers).has("x-lightspeed-universe")).toBe(false);
    expect(new Headers(init.headers).get("x-lightspeed-actor")).toBe("member");
    const request = decodeHttpRequest(_url as string | Request | URL, init);
    requests.push({ method: request.method, params: request.params });
    return Response.json(request.method === "blobs/put" ? { blobs: [{ blobRef: audio.blobRef, bytes: 5 }] } : { transcription: { transcriptionId: "job", status: "running", createdBy: { kind: "actor", id: "member" } } });
  });
  vi.stubGlobal("fetch", fetch);
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => { c.set("session", { user: { id: "member" } } as ApiVariables["session"]); c.set("auth", userSubject(c.get("session").user.id, c.get("session").user.role === "admin")); await next(); });
  app.route("/", gatewayRoutes({ env: { lightspeedApiUrl: "https://engine.example/api/v1", lightspeedApiToken: "lsr_fixture" } } as AppContext));
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
    const request = decodeHttpRequest(_url as string | Request | URL, init);
    return problemResponse({ type: "about:blank", title: "Choose a speech model", detail: "Choose a speech model", status: 409, kind: "modelDefaultUnset", modelDefaultSlot: "speechToText" });
  });
  const response = await f.call("POST", "", { audio, idempotencyKey: "request" });
  expect(response.status).toBe(400);
  expect(await response.json()).toMatchObject({ kind: "modelDefaultUnset", modelDefaultSlot: "speechToText" });
});

it.each(["contributor", "operator", "owner"])("never returns another actor's transcription or cancels it as %s", async role => {
  auth.role = role;
  const f = fixture();
  const calls: string[] = [];
  f.fetch.mockImplementation(async (_url, init) => {
    const request = decodeHttpRequest(_url as string | Request | URL, init);
    calls.push(request.method);
    return Response.json({ transcription: {
      transcriptionId: "job", status: "completed", text: "private transcript content",
      createdBy: { kind: "actor", id: "another-member" },
    } });
  });
  for (const [method, path] of [["GET", "/job"], ["POST", "/job/cancel"]] as const) {
    const response = await f.call(method, path);
    expect(response.status).toBe(403);
    expect(await response.text()).not.toContain("private transcript content");
  }
  expect(calls).toEqual(["transcriptions/read", "transcriptions/read"]);
});

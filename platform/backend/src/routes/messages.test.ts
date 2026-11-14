import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { gatewayRoutes } from "./gateway.js";

const auth = vi.hoisted(() => ({ role: "contributor" }));
vi.mock("./universes.js", () => ({ universeForSession: vi.fn(async (_ctx, _c, id: string) => ({
  universe: { lightspeedUniverseId: id, gatewayUrl: "https://engine.example/rpc" },
  handle: "test", role: auth.role, member: { userId: "member", role: auth.role },
})) }));
beforeEach(() => { auth.role = "contributor"; });
afterEach(() => vi.unstubAllGlobals());

const image = { blobRef: `sha256:${"b".repeat(64)}`, mime: "image/png", kind: "image", name: "screen.png" };
const pdf = { blobRef: `sha256:${"c".repeat(64)}`, mime: "application/pdf", kind: "document", name: "offer.pdf" };
const route = { providerId: "openai", apiKind: "openai:responses", model: "gpt-5.5-mini" };

function fixture(steeringError?: { code: number; message: string }) {
  const requests: { method: string; params: Record<string, unknown> }[] = [];
  const fetch = vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    // The member gate reads the session before a session-targeted call.
    if (rpc.method === "session/read") {
      return Response.json({ id: rpc.id, result: { result: { session: { access: { visibility: "universe" } } }, notifications: [] } });
    }
    requests.push(rpc);
    if (rpc.method === "session/runs/steer" && steeringError) {
      return Response.json({ id: rpc.id, error: steeringError });
    }
    const result = rpc.method === "blobs/put"
      ? { blobs: [{ blobRef: image.blobRef, bytes: 3 }] }
      : rpc.method === "session/runs/steer"
        ? { steeringId: "steer_1", run: { id: "run_1", status: "running" } }
        : { run: { id: "run_1", status: "running", startedAtMs: 123,
          source: { type: "input", items: [{ type: "text", text: "Complete queued input" }] } } };
    return Response.json({ id: rpc.id, result: { result, notifications: [] } });
  });
  vi.stubGlobal("fetch", fetch);
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => { c.set("session", { user: { id: "member" } } as ApiVariables["session"]); await next(); });
  app.route("/", gatewayRoutes({ env: { lightspeedApiUrl: "https://engine.example/rpc", lightspeedApiKey: "lsk_fixture" } } as AppContext));
  const call = (path: string, body: unknown) => app.request(`/universe${path}`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
  });
  const get = (path: string) => app.request(`/universe${path}`);
  return { call, get, requests, fetch };
}

it("reads complete run input through the member-scoped client", async () => {
  const f = fixture();
  const response = await f.get("/sessions/s1/runs/run_1");
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({
    source: { type: "input", items: [{ type: "text", text: "Complete queued input" }] },
  });
  expect(f.requests).toMatchObject([{ method: "session/runs/read", params: { sessionId: "s1", runId: "run_1" } }]);
});

it("returns whether a cancelled run had already started", async () => {
  const f = fixture();
  const response = await f.call("/sessions/s1/runs/run_1/cancel", {});
  expect(await response.json()).toEqual({ run: { id: "run_1", status: "running", startedAtMs: 123 } });
});

it("sends operator attachments as media items before the text, with per-message run options", async () => {
  auth.role = "operator";
  const f = fixture();
  const response = await f.call("/sessions/s1/messages", {
    text: "Compare these", submissionId: "sub", attachments: [image, pdf],
    options: { model: route, reasoningEffort: "high" },
  });
  expect(response.status).toBe(200);
  expect(f.requests[0]).toMatchObject({
    method: "session/runs/start",
    params: {
      sessionId: "s1",
      submissionId: "sub",
      source: { type: "input", items: [
        { type: "media", origin: "user:member", ...image },
        { type: "media", origin: "user:member", ...pdf },
        { type: "text", origin: "user:member", text: "Compare these" },
      ] },
      config: { model: route, generation: { reasoningEffort: "high" } },
    },
  });
});

it("accepts an attachment-only message and sends no config without options", async () => {
  const f = fixture();
  expect((await f.call("/sessions/s1/messages", { submissionId: "sub", attachments: [image] })).status).toBe(200);
  const params = f.requests[0]!.params as { source: { items: unknown[] }; config?: unknown };
  expect(params.source.items).toEqual([{ type: "media", origin: "user:member", ...image }]);
  expect(params.config).toBeUndefined();
});

it("steers with attachments", async () => {
  const f = fixture();
  const response = await f.call("/sessions/s1/runs/run_1/steer", { text: "Also this", attachments: [pdf] });
  expect(await response.json()).toMatchObject({ steeringId: "steer_1" });
  expect(f.requests[0]!.params).toMatchObject({ items: [
    { type: "media", origin: "user:member", ...pdf },
    { type: "text", origin: "user:member", text: "Also this" },
  ] });
});

it.each([
  "run is already terminal: run_1",
  "run is not accepting steering: run_1",
])("identifies steering refused before submission: %s", async (message) => {
  const f = fixture({ code: -32010, message });
  const response = await f.call("/sessions/s1/runs/run_1/steer", { text: "Follow up" });
  expect(response.status).toBe(409);
  expect(await response.json()).toEqual({ error: message, kind: "steering_not_submitted" });
});

it.each([
  { code: -32010, message: "run ended before steering reached it: run_1" },
  { code: -32015, message: "steering has not been admitted yet and can still land on run_1" },
  { code: -32009, message: "run is already terminal: run_1" },
])("does not label an uncertain steering outcome as unsubmitted: %j", async (error) => {
  const f = fixture(error);
  const response = await f.call("/sessions/s1/runs/run_1/steer", { text: "Follow up" });
  expect(response.status).toBeGreaterThanOrEqual(400);
  expect(await response.json()).not.toHaveProperty("kind", "steering_not_submitted");
});

it.each([
  ["an empty message", { submissionId: "sub" }],
  ["whitespace with no attachments", { text: "   ", submissionId: "sub" }],
  ["an unsupported type", { submissionId: "sub", attachments: [{ ...pdf, mime: "application/msword" }] }],
  ["a kind that contradicts the type", { submissionId: "sub", attachments: [{ ...image, kind: "document" }] }],
  ["too many attachments", { submissionId: "sub", attachments: Array.from({ length: 9 }, () => image) }],
  ["an unknown option", { text: "hi", submissionId: "sub", options: { temperature: 1 } }],
  ["a processing tier, which belongs to the session config", { text: "hi", submissionId: "sub", options: { processingTier: "flex" } }],
])("rejects %s before reaching the runtime", async (_name, body) => {
  const f = fixture();
  expect((await f.call("/sessions/s1/messages", body)).status).toBe(400);
  expect(f.fetch).not.toHaveBeenCalled();
});

it("uploads attachments as blobs for contributors only", async () => {
  const f = fixture();
  expect(await (await f.call("/attachments", { bytesBase64: btoa("png") })).json()).toMatchObject({ blobs: [{ blobRef: image.blobRef }] });
  expect(f.requests[0]).toMatchObject({ method: "blobs/put", params: { blobs: [{ bytesBase64: btoa("png") }] } });
  auth.role = "viewer";
  const viewer = fixture();
  expect((await viewer.call("/attachments", { bytesBase64: btoa("png") })).status).toBe(403);
  expect(viewer.fetch).not.toHaveBeenCalled();
});


it("rejects contributor model and reasoning overrides before reaching the runtime", async () => {
  const f = fixture();
  for (const options of [{ model: route }, { reasoningEffort: "high" }]) {
    expect((await f.call("/sessions/s1/messages", { text: "Hello", submissionId: "sub", options })).status).toBe(403);
  }
  expect(f.fetch).not.toHaveBeenCalled();
});

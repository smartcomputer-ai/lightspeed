import { afterEach, expect, it, vi } from "vitest";
import { createDemoStore } from "./fixtures";
import { createDemoRouter } from "./router";
import type { TranscriptionView } from "@lightspeed-ai/sdk";
function boot() {
  const store = createDemoStore();
  const app = createDemoRouter(store);
  const universe = store.universeByHandle("software-factory")!;
  const base = `/api/ui/universes/${universe.universe.id}/transcriptions`;
  const call = (method: string, path = "", body?: unknown) => app.request(`${base}${path}`, {
    method, headers: { "content-type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  return { store, universe, call };
}
afterEach(() => vi.useRealTimers());
it("requires a speech default, pins it at admission, and completes without sending a session message", async () => {
  vi.useFakeTimers();
  const { universe, call } = boot();
  const sessionsBefore = JSON.stringify([...universe.sessions]);
  const upload = await (await call("POST", "/audio", { bytesBase64: btoa("audio") })).json() as { blobs: { blobRef: string }[] };
  const request = { audio: { blobRef: upload.blobs[0]!.blobRef, mime: "audio/webm", name: "dictation.webm" }, idempotencyKey: "key" };
  expect((await call("POST", "", request)).status).toBe(400);
  const model = { providerId: "openai", apiKind: "openai:audio-transcriptions", model: "speech" };
  universe.modelDefaults.speechToText = model;
  const job = await (await call("POST", "", request)).json() as TranscriptionView;
  expect(job).toMatchObject({ status: "running", model });
  universe.modelDefaults.speechToText = null;
  expect(await (await call("POST", "", request)).json()).toEqual(job);
  await vi.advanceTimersByTimeAsync(1000);
  const done = await (await call("GET", `/${job.transcriptionId}`)).json() as TranscriptionView;
  expect(done).toMatchObject({ status: "succeeded", model });
  expect(done.text).toContain("Please summarize");
  expect(JSON.stringify([...universe.sessions])).toBe(sessionsBefore);
});

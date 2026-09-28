// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { transcribeRecording, type DictationRecording } from "./dictation";
const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", () => ({ api: mocks.api }));
let recording: DictationRecording;
const base = "/api/v1/universes/universe/transcriptions";
beforeEach(() => {
  vi.useFakeTimers();
  mocks.api.mockReset();
  recording = { blob: new Blob(["audio"], { type: "audio/webm" }), name: "dictation.webm", idempotencyKey: "request", blobRef: `sha256:${"a".repeat(64)}` };
});
afterEach(() => vi.useRealTimers());
it("uploads audio, uses the universe default, and returns text without sending a session message", async () => {
  vi.useRealTimers();
  recording.blobRef = undefined;
  mocks.api.mockResolvedValueOnce({ blobs: [{ blobRef: "uploaded" }] }).mockResolvedValueOnce({ transcriptionId: "job", status: "succeeded", text: " Preview me " });
  expect(await transcribeRecording("universe", recording, new AbortController().signal)).toBe("Preview me");
  expect(mocks.api.mock.calls.map((call) => call.slice(0, 2))).toEqual([["POST", `${base}/audio`], ["POST", base]]);
  expect(mocks.api.mock.calls[0]![2]).toEqual({ bytesBase64: btoa("audio") });
  expect(mocks.api.mock.calls[1]![2]).toEqual({ idempotencyKey: "request", audio: { blobRef: "uploaded", mime: "audio/webm", name: "dictation.webm" } });
});
it("rejoins the same job after a transport failure without uploading again", async () => {
  mocks.api.mockResolvedValueOnce({ transcriptionId: "job", status: "running" }).mockRejectedValueOnce(new Error("network"));
  const pending = transcribeRecording("universe", recording, new AbortController().signal);
  const rejected = expect(pending).rejects.toThrow("network");
  await vi.advanceTimersByTimeAsync(1000);
  await rejected;
  expect(recording.idempotencyKey).toBe("request");
  mocks.api.mockResolvedValueOnce({ transcriptionId: "job", status: "succeeded", text: "done" });
  expect(await transcribeRecording("universe", recording, new AbortController().signal)).toBe("done");
  expect(mocks.api.mock.calls.map((call) => call[1])).toEqual([base, `${base}/job`, base]);
  expect(mocks.api.mock.calls[2]![2].idempotencyKey).toBe("request");
});
it("cancels a start that is admitted after the user has cancelled", async () => {
  let admit!: (value: unknown) => void;
  mocks.api.mockImplementationOnce(() => new Promise((resolve) => { admit = resolve; })).mockResolvedValue({ status: "cancelled" });
  const controller = new AbortController();
  const pending = transcribeRecording("universe", recording, controller.signal);
  controller.abort();
  admit({ transcriptionId: "late", status: "running" });
  await expect(pending).rejects.toMatchObject({ name: "AbortError" });
  expect(mocks.api).toHaveBeenLastCalledWith("POST", `${base}/late/cancel`);
});
it("cancels an active workflow and stops polling", async () => {
  mocks.api.mockResolvedValue({ transcriptionId: "job", status: "running" });
  const controller = new AbortController();
  const pending = transcribeRecording("universe", recording, controller.signal);
  const rejected = expect(pending).rejects.toMatchObject({ name: "AbortError" });
  await vi.advanceTimersByTimeAsync(0);
  controller.abort();
  await rejected;
  await vi.advanceTimersByTimeAsync(2000);
  expect(mocks.api.mock.calls.map((call) => call.slice(0, 2))).toEqual([["POST", base], ["POST", `${base}/job/cancel`]]);
});
it.each(["failed", "expired", "cancelled"])("uses a fresh key when retrying a terminal %s job", async (status) => {
  mocks.api.mockResolvedValue({ transcriptionId: "job", status, failure: { message: "Try again" } });
  await expect(transcribeRecording("universe", recording, new AbortController().signal)).rejects.toThrow("Try again");
  expect(recording.idempotencyKey).not.toBe("request");
  expect(recording.transcriptionId).toBeUndefined();
});
it("reports empty speech without changing a draft", async () => {
  mocks.api.mockResolvedValue({ transcriptionId: "job", status: "succeeded", text: "  " });
  await expect(transcribeRecording("universe", recording, new AbortController().signal)).rejects.toThrow("No speech was detected");
});

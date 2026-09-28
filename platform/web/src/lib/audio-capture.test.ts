// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { startAudioCapture } from "./audio-capture";

const mock = vi.hoisted(() => ({ supported: "audio/mp4", instances: [] as any[] }));
vi.mock("extendable-media-recorder", () => ({ MediaRecorder: class {
  static isTypeSupported(mime: string) { return mime === mock.supported; }
  state = "inactive";
  mimeType: string;
  ondataavailable: any;
  onstop: any;
  onerror: any;
  constructor(_stream: unknown, options: { mimeType: string }) { this.mimeType = options.mimeType; mock.instances.push(this); }
  start() { this.state = "recording"; }
  stop() { this.state = "inactive"; queueMicrotask(() => this.onstop?.()); }
} }));
let track: EventTarget & { stop: ReturnType<typeof vi.fn> };
let getUserMedia: ReturnType<typeof vi.fn>;
beforeEach(() => {
  mock.instances = [];
  mock.supported = "audio/mp4";
  track = Object.assign(new EventTarget(), { stop: vi.fn() });
  getUserMedia = vi.fn().mockResolvedValue({ getTracks: () => [track] });
  vi.stubGlobal("navigator", { mediaDevices: { getUserMedia } });
});
afterEach(() => vi.unstubAllGlobals());
it("chooses a supported format and releases the microphone after stopping", async () => {
  const capture = await startAudioCapture(new AbortController().signal, vi.fn());
  mock.instances[0].ondataavailable({ data: new Blob(["audio"], { type: "audio/mp4" }) });
  capture.stop();
  const blob = await capture.result;
  expect(blob.type).toBe("audio/mp4");
  expect(blob.size).toBe(5);
  expect(capture.name).toBe("dictation.m4a");
  expect(track.stop).toHaveBeenCalled();
});
it("releases a microphone granted after cancellation", async () => {
  let grant!: (value: unknown) => void;
  getUserMedia.mockImplementation(() => new Promise((resolve) => { grant = resolve; }));
  const controller = new AbortController();
  const pending = startAudioCapture(controller.signal, vi.fn());
  await vi.waitFor(() => expect(getUserMedia).toHaveBeenCalled());
  controller.abort();
  grant({ getTracks: () => [track] });
  await expect(pending).rejects.toMatchObject({ name: "AbortError" });
  expect(track.stop).toHaveBeenCalled();
  expect(mock.instances).toHaveLength(0);
});
it.each(["error", "ended", "abort"])("cleans up capture on %s", async (reason) => {
  const controller = new AbortController();
  const onError = vi.fn();
  const capture = await startAudioCapture(controller.signal, onError);
  const rejected = expect(capture.result).rejects.toMatchObject({ name: reason === "abort" ? "AbortError" : "Error" });
  if (reason === "abort") controller.abort();
  else if (reason === "ended") track.dispatchEvent(new Event("ended"));
  else mock.instances[0].onerror();
  await rejected;
  expect(track.stop).toHaveBeenCalled();
  expect(mock.instances[0].state).toBe("inactive");
  expect(onError).toHaveBeenCalledTimes(reason === "abort" ? 0 : 1);
});
it("does not open the microphone when no accepted format is supported", async () => {
  mock.supported = "unsupported";
  await expect(startAudioCapture(new AbortController().signal, vi.fn())).rejects.toThrow("supported audio format");
  expect(getUserMedia).not.toHaveBeenCalled();
});
it("passes microphone permission errors to the caller", async () => {
  getUserMedia.mockRejectedValue(new DOMException("Denied", "NotAllowedError"));
  await expect(startAudioCapture(new AbortController().signal, vi.fn())).rejects.toMatchObject({ name: "NotAllowedError" });
});

import { MAX_DICTATION_AUDIO_BYTES } from "@lightspeed/platform-shared";

export interface AudioCapture {
  result: Promise<Blob>;
  stop: () => void;
  name: string;
}
export const isDemoDictation = import.meta.env.MODE === "demo";
const formats = [
  ["audio/webm;codecs=opus", "webm"],
  ["audio/mp4", "m4a"],
  ["audio/ogg;codecs=opus", "ogg"],
  ["audio/webm", "webm"],
] as const;

export function audioCaptureUnavailableReason(): string | undefined {
  if (isDemoDictation) return;
  if (!globalThis.isSecureContext) return "Dictation requires HTTPS or localhost.";
  if (!navigator.mediaDevices?.getUserMedia || !globalThis.MediaRecorder) return "Audio recording is not supported by this browser.";
}

export async function startAudioCapture(signal: AbortSignal, onError: (error: Error) => void): Promise<AudioCapture> {
  if (isDemoDictation) {
    let finish!: (blob: Blob) => void;
    const result = new Promise<Blob>((resolve) => { finish = resolve; });
    return { result, name: "demo.webm", stop: () => finish(new Blob(["demo audio"], { type: "audio/webm" })) };
  }
  const { MediaRecorder } = await import("extendable-media-recorder");
  signal.throwIfAborted();
  const format = formats.find(([mime]) => MediaRecorder.isTypeSupported(mime));
  if (!format) throw new Error("This browser cannot record a supported audio format.");
  const stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1 }, video: false });
  const release = () => stream.getTracks().forEach((track) => track.stop());
  if (signal.aborted) { release(); signal.throwIfAborted(); }
  try {
    const recorder = new MediaRecorder(stream, { mimeType: format[0], audioBitsPerSecond: 64_000 });
    const chunks: Blob[] = [];
    let bytes = 0;
    let settled = false;
    let resolve!: (blob: Blob) => void;
    let reject!: (error: Error) => void;
    const result = new Promise<Blob>((yes, no) => { resolve = yes; reject = no; });
    // Recording errors can happen before the user asks for the result.
    void result.catch(() => {});
    const stop = () => {
      if (recorder.state !== "inactive") recorder.stop();
      release();
    };
    const cleanup = () => {
      signal.removeEventListener("abort", abort);
      stream.getTracks().forEach((track) => track.removeEventListener("ended", ended));
      recorder.ondataavailable = null;
      recorder.onerror = null;
      recorder.onstop = null;
      release();
    };
    const fail = (error: Error, notify: boolean) => {
      if (settled) return;
      settled = true;
      try { stop(); } finally { cleanup(); }
      chunks.length = 0;
      reject(error);
      if (notify) onError(error);
    };
    const ended = () => fail(new Error("Microphone disconnected. Try recording again."), true);
    const abort = () => fail(new DOMException("Recording cancelled", "AbortError"), false);
    recorder.ondataavailable = (event) => {
      bytes += event.data.size;
      if (bytes > MAX_DICTATION_AUDIO_BYTES) { fail(new Error("Recording exceeds 25 MiB. Try a shorter recording."), true); return; }
      if (event.data.size) chunks.push(event.data);
    };
    recorder.onerror = () => fail(new Error("Recording failed. Check your microphone and try again."), true);
    recorder.onstop = () => {
      if (settled) return;
      settled = true;
      const blob = new Blob(chunks, { type: recorder.mimeType || format[0] });
      cleanup();
      chunks.length = 0;
      resolve(blob);
    };
    stream.getTracks().forEach((track) => track.addEventListener("ended", ended, { once: true }));
    signal.addEventListener("abort", abort, { once: true });
    try { recorder.start(1000); } catch (error) { cleanup(); throw error; }
    return { result, stop, name: `dictation.${format[1]}` };
  } catch (error) { release(); throw error; }
}

import type { BlobPutResponse, TranscriptionView } from "@lightspeed-ai/agent-client";
import { MAX_DICTATION_AUDIO_BYTES } from "@lightspeed/platform-shared";
import { api } from "@/api";
import { blobBase64 } from "@/lib/blob-base64";

export interface DictationRecording {
  blob: Blob;
  name: string;
  idempotencyKey: string;
  blobRef?: string;
  transcriptionId?: string;
}

function pause(signal: AbortSignal) {
  return new Promise<void>((resolve, reject) => {
    signal.throwIfAborted();
    const abort = () => { clearTimeout(timer); reject(new DOMException("Cancelled", "AbortError")); };
    const timer = setTimeout(() => { signal.removeEventListener("abort", abort); resolve(); }, 1000);
    signal.addEventListener("abort", abort, { once: true });
  });
}

export function cancelRecording(universeId: string, recording: DictationRecording) {
  if (recording.transcriptionId) void api("POST", `/api/v1/universes/${universeId}/transcriptions/${recording.transcriptionId}/cancel`).catch(() => {});
}

export async function transcribeRecording(universeId: string, recording: DictationRecording, signal: AbortSignal): Promise<string> {
  if (!recording.blob.size) throw new Error("No audio was recorded. Try again.");
  if (recording.blob.size > MAX_DICTATION_AUDIO_BYTES) throw new Error("Recording exceeds 25 MiB. Try a shorter recording.");
  const path = `/api/v1/universes/${universeId}/transcriptions`;
  if (!recording.blobRef) {
    const bytesBase64 = await blobBase64(recording.blob, signal);
    const result = await api<BlobPutResponse>("POST", `${path}/audio`, { bytesBase64 }, signal);
    recording.blobRef = result.blobs?.[0]?.blobRef;
    if (!recording.blobRef) throw new Error("Audio upload did not return a reference.");
  }
  signal.throwIfAborted();
  // Let admission return its id even after cancellation, so a late start can
  // be cancelled instead of leaving an unseen job running.
  let view = await api<TranscriptionView>("POST", path, {
    idempotencyKey: recording.idempotencyKey,
    audio: { blobRef: recording.blobRef, mime: recording.blob.type, name: recording.name },
  });
  recording.transcriptionId = view.transcriptionId;
  const cancel = () => cancelRecording(universeId, recording);
  if (signal.aborted) { cancel(); signal.throwIfAborted(); }
  signal.addEventListener("abort", cancel, { once: true });
  try {
    while (view.status === "pending" || view.status === "running") {
      await pause(signal);
      view = await api<TranscriptionView>("GET", `${path}/${view.transcriptionId}`, undefined, signal);
    }
    signal.throwIfAborted();
    recording.transcriptionId = undefined;
    if (view.status !== "succeeded") {
      // A user-requested retry of a terminal failure is a new job; transport
      // failures keep the original key and rejoin it on retry.
      recording.idempotencyKey = crypto.randomUUID();
      throw new Error(view.failure?.message ?? (view.status === "expired" ? "The transcript expired. Try again." : "Transcription was cancelled."));
    }
    if (!view.text?.trim()) throw new Error("No speech was detected. Try recording again.");
    return view.text.trim();
  } finally { signal.removeEventListener("abort", cancel); }
}

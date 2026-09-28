import { z } from "zod";

export const MAX_DICTATION_AUDIO_BYTES = 25 * 1024 * 1024;
export const MAX_DICTATION_SECONDS = 10 * 60;
export const transcriptionUploadSchema = z.object({
  bytesBase64: z.string().min(4).max(4 * Math.ceil(MAX_DICTATION_AUDIO_BYTES / 3)).regex(/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/),
}).strict();

// Web dictation always uses the universe default. Model overrides belong to
// the core API, not this browser route.
export const transcriptionStartSchema = z.object({
  idempotencyKey: z.string().min(1).max(200),
  audio: z.object({
    blobRef: z.string().regex(/^sha256:[a-f0-9]{64}$/),
    mime: z.string().min(1).max(128),
    name: z.string().min(1).max(256),
  }).strict(),
}).strict();

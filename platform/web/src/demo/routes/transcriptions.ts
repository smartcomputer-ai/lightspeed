import { Hono } from "hono";
import type { TranscriptionView } from "@lightspeed-ai/agent-client";
import { MAX_DICTATION_AUDIO_BYTES, roleAtLeast, transcriptionStartSchema, transcriptionUploadSchema } from "@lightspeed/platform-shared";
import type { DemoStore } from "../store";
import { badRequest, conflict, notFound, readBody, universeFor } from "./common";

export function transcriptionRoutes(store: DemoStore) {
  const app = new Hono();
  const jobs = new Map<string, { universeId: string; key: string; request: string; view: TranscriptionView }>();
  app.post("/:id/transcriptions/audio", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    if (store.currentUser.role !== "admin" && !roleAtLeast(universe.universe.role ?? "viewer", "contributor")) return c.json({ error: "contributor role required" }, 403);
    const body = transcriptionUploadSchema.safeParse(await readBody(c));
    if (!body.success) return badRequest(c, "Invalid audio upload");
    const bytes = Uint8Array.from(atob(body.data.bytesBase64), (char) => char.charCodeAt(0));
    if (bytes.length > MAX_DICTATION_AUDIO_BYTES) return c.json({ error: "Recording exceeds 25 MiB." }, 413);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
    const blobRef = `sha256:${[...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("")}`;
    store.blobs.set(blobRef, { blobRef, bytes: bytes.length, bytesBase64: body.data.bytesBase64 });
    return c.json({ blobs: [{ blobRef, bytes: bytes.length }] });
  });
  app.post("/:id/transcriptions", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    if (store.currentUser.role !== "admin" && !roleAtLeast(universe.universe.role ?? "viewer", "contributor")) return c.json({ error: "contributor role required" }, 403);
    const body = transcriptionStartSchema.safeParse(await readBody(c));
    if (!body.success) return badRequest(c, "Invalid transcription request");
    const request = JSON.stringify(body.data);
    const existing = [...jobs.values()].find((job) => job.universeId === universe.universe.id && job.key === body.data.idempotencyKey && job.view.createdBy.kind === "actor" && job.view.createdBy.id === store.currentUser.id);
    if (existing) return existing.request === request ? c.json(existing.view) : conflict(c, "Transcription key already used with different audio");
    if (!universe.modelDefaults.speechToText) return c.json({ error: "Set a speech-to-text default in Models.", kind: "model_default_unset", modelDefaultSlot: "speechToText" }, 400);
    if (!store.blobs.has(body.data.audio.blobRef)) return badRequest(c, "Audio blob is missing");
    const transcriptionId = `transcription_${crypto.randomUUID()}`;
    const view: TranscriptionView = { transcriptionId, createdBy: { kind: "actor", id: store.currentUser.id }, audio: body.data.audio,
      model: structuredClone(universe.modelDefaults.speechToText), status: "running", createdAtMs: Date.now(), transcriptRef: null, text: null, failure: null };
    jobs.set(transcriptionId, { universeId: universe.universe.id, key: body.data.idempotencyKey, request, view });
    return c.json(view);
  });
  app.get("/:id/transcriptions/:transcriptionId", (c) => {
    const universe = universeFor(store, c);
    const job = jobs.get(c.req.param("transcriptionId"));
    if (!universe || !job || job.universeId !== universe.universe.id || job.view.createdBy.kind !== "actor" || job.view.createdBy.id !== store.currentUser.id) return notFound(c);
    if (job.view.status === "running" && Date.now() - job.view.createdAtMs >= 750) {
      job.view.text = "Please summarize the next steps and highlight anything that needs my attention.";
      job.view.transcriptRef = store.putText(job.view.text);
      job.view.status = "succeeded";
    }
    return c.json(job.view);
  });
  app.post("/:id/transcriptions/:transcriptionId/cancel", (c) => {
    const universe = universeFor(store, c);
    const job = jobs.get(c.req.param("transcriptionId"));
    if (!universe || !job || job.universeId !== universe.universe.id || job.view.createdBy.kind !== "actor" || job.view.createdBy.id !== store.currentUser.id) return notFound(c);
    if (store.currentUser.role !== "admin" && !roleAtLeast(universe.universe.role ?? "viewer", "contributor")) return c.json({ error: "contributor role required" }, 403);
    if (job.view.status === "running") job.view.status = "cancelled";
    return c.json(job.view);
  });
  return app;
}

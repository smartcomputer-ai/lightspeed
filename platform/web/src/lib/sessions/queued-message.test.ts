import { expect, it, vi } from "vitest";
import type { InputItem, RunView } from "@lightspeed-ai/sdk";
import { queuedMessage } from "./queued-message";

const run = (items: InputItem[]): RunView => ({ id: "queued", status: "queued", source: { type: "input", items } });
const media = { type: "media" as const, blobRef: `sha256:${"b".repeat(64)}`, mime: "image/png", kind: "image" as const, name: "image.png" };

it("loads full referenced text and keeps attachments", async () => {
  const text = "long input ".repeat(1000);
  const load = vi.fn().mockResolvedValue(text);
  expect(await queuedMessage(run([media, { type: "textRef", blobRef: "text" }]), load)).toEqual({
    text, attachments: [{ blobRef: media.blobRef, mime: media.mime, kind: media.kind, name: media.name, size: 0 }],
  });
  expect(load).toHaveBeenCalledWith("text");
});

it("supports attachment-only messages without synthesizing text", async () => {
  const message = await queuedMessage(run([media]), vi.fn());
  expect(message.text).toBe("");
  expect(message.attachments).toHaveLength(1);
});

it.each<{ items: InputItem[] }>([
  { items: [] },
  { items: [{ type: "text", text: " ".repeat(5) }] },
  { items: [{ type: "text", text: "x".repeat(100_001) }] },
  { items: [{ ...media, kind: "audio", mime: "audio/mpeg" }] },
  { items: [{ type: "text", text: "supported" }, { type: "catalog", title: "catalog", text: "must not be dropped" }] },
])("rejects unsupported input before cancellation: %#", async ({ items }) => {
  await expect(queuedMessage(run(items), vi.fn())).rejects.toThrow("cannot be converted");
});

it("propagates a failed blob read before cancellation", async () => {
  await expect(queuedMessage(run([{ type: "textRef", blobRef: "missing" }]), async () => {
    throw new Error("Blob unavailable");
  })).rejects.toThrow("Blob unavailable");
});

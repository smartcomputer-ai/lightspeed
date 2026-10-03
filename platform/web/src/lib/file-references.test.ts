import { expect, it, vi } from "vitest";
import { findFileReference, filesByHandle, filesFromAttachments } from "./file-references";
import { applyEvents, emptyTranscript } from "./sessions/transcript";
import { TranscriptWindow } from "./sessions/transcript-window";
import type { SessionEvent, ToolAttachmentView } from "@/api";

const handle = `file:${"a".repeat(24)}`;
const blobRef = `sha256:${"a".repeat(64)}`;
const attachment = (overrides: Partial<ToolAttachmentView> = {}): ToolAttachmentView => ({
  kind: "file", handle, contentRef: blobRef, name: "résumé #?.txt",
  source: { kind: "vfs_workspace", id: "ws", path: "docs/résumé #?.txt" }, ...overrides,
});
const completion = (attachments = [attachment()]): SessionEvent => ({
  cursor: { seq: 2 }, observedAtMs: 2, joins: {}, sessionId: "session",
  kind: { type: "toolCallCompleted", runId: "run", turnId: "turn", batchId: "batch", callId: "c1", status: "succeeded", attachments },
});
const collision = attachment({ contentRef: `sha256:${"a".repeat(24)}${"b".repeat(40)}` });

it("resolves recorded file attachments independently of their source", () => {
  expect(filesFromAttachments([attachment()]).get(handle)).toEqual({ handle, blobRef, name: "résumé #?.txt", path: "docs/résumé #?.txt", workspace: "ws" });
  expect(filesFromAttachments([attachment({ source: undefined })]).get(handle)).toEqual({ handle, blobRef, name: "résumé #?.txt" });
});
it.each<Partial<ToolAttachmentView>>([
  { handle: "file:bad" }, { contentRef: "javascript:alert(1)" }, { name: "" },
  { contentRef: `sha256:${"b".repeat(64)}` }, { kind: "media" },
])("ignores malformed or unrelated attachments: %j", (data) => {
  expect(filesFromAttachments([attachment(data)]).size).toBe(0);
});
it("treats names as descriptions but rejects conflicting content identities", () => {
  expect(filesFromAttachments([attachment(), attachment({ name: "alias.txt", source: undefined })]).get(handle)?.blobRef).toBe(blobRef);
  expect(filesFromAttachments([attachment(), collision, attachment()]).get(handle)).toBeNull();
});
it("does not turn an invalid origin path into a workspace backlink", () => {
  expect(filesFromAttachments([attachment({ source: { kind: "vfs_workspace", id: "ws", path: "../secret" } })]).get(handle)?.workspace).toBeUndefined();
});
it("restores attachments from completion events on reload and history pagination", () => {
  const event = completion();
  const live = applyEvents(emptyTranscript(), [event]);
  const recorded = JSON.parse(JSON.stringify(event)) as SessionEvent;
  const history = new TranscriptWindow(); history.prepend([recorded]);
  expect(filesByHandle(live.entries).get(handle)?.blobRef).toBe(blobRef);
  expect(filesByHandle(history.state.entries)).toEqual(filesByHandle(live.entries));
});
it("looks up older attachments without relying on the visible transcript", async () => {
  const read = vi.fn().mockResolvedValueOnce({ events: [], complete: false, nextCursor: { seq: 10 } }).mockResolvedValueOnce({ events: [completion()], complete: true });
  expect((await findFileReference(handle, read))?.blobRef).toBe(blobRef);
  expect(read.mock.calls).toEqual([[null], [10]]);
});
it("rejects malformed handles without reading, and returns null for unknown handles", async () => {
  const read = vi.fn().mockResolvedValue({ events: [], complete: true });
  expect(await findFileReference("file:invalid", read)).toBeNull();
  expect(read).not.toHaveBeenCalled();
  expect(await findFileReference(handle, read)).toBeNull();
});
it("rejects history gaps and nonadvancing cursors", async () => {
  await expect(findFileReference(handle, async () => ({ events: [], gap: {}, complete: false }))).rejects.toThrow("unavailable");
  const read = vi.fn().mockResolvedValue({ events: [], complete: false, nextCursor: { seq: 10 } });
  await expect(findFileReference(handle, read)).rejects.toThrow("did not advance");
});
it("checks later pages for collisions even after finding the handle", async () => {
  const read = vi.fn().mockResolvedValueOnce({ events: [completion()], complete: false, nextCursor: { seq: 10 } }).mockResolvedValueOnce({ events: [completion([collision])], complete: true });
  expect(await findFileReference(handle, read)).toBeNull();
  expect(read).toHaveBeenCalledTimes(2);
});

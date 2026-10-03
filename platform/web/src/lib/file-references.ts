import { validWorkspaceTransferPath } from "@lightspeed-ai/platform-shared";
import { blobDigest } from "./blob-view";
import type { SessionEventsPage, ToolAttachmentView } from "@/api";
import type { TranscriptEntry } from "./sessions/transcript";

export interface FileReference {
  handle: string;
  blobRef: string;
  name: string;
  path?: string;
  workspace?: string;
  type?: string;
}

export const isFileHandle = (value: string): boolean => /^file:[a-f0-9]{24}$/.test(value);

export function filesByHandle(entries: TranscriptEntry[]): Map<string, FileReference | null> {
  return filesFromAttachments(entries.flatMap((entry) => entry.kind === "tool-group"
    ? entry.calls.flatMap((call) => call.attachments ?? []) : []));
}

/// Names and sources can differ for equal bytes; collisions of content identity cannot.
export function filesFromAttachments(attachments: ToolAttachmentView[]): Map<string, FileReference | null> {
  const files = new Map<string, FileReference | null>();
  for (const attachment of attachments) {
    if (attachment.kind !== "file" || !isFileHandle(attachment.handle)) continue;
    const digest = blobDigest(attachment.contentRef);
    if (!digest || attachment.handle !== `file:${digest.slice(0, 24)}` || !attachment.name) continue;
    const source = attachment.source;
    const workspaceSource = source?.kind === "vfs_workspace" && validWorkspaceTransferPath(source.path) ? source : undefined;
    const file: FileReference = {
      handle: attachment.handle, blobRef: attachment.contentRef, name: attachment.name,
      ...(attachment.mediaType ? { type: attachment.mediaType } : {}),
      ...(workspaceSource ? { workspace: workspaceSource.id, path: workspaceSource.path } : {}),
    };
    if (!files.has(file.handle)) files.set(file.handle, file);
    else if (files.get(file.handle)?.blobRef !== file.blobRef) files.set(file.handle, null);
    else if (!files.get(file.handle)?.workspace && file.workspace) files.set(file.handle, file);
  }
  return files;
}

/// Resolve recorded attachments across all history pages, including collisions outside the window.
export async function findFileReference(
  handle: string,
  readPage: (before: number | null) => Promise<SessionEventsPage>,
): Promise<FileReference | null> {
  if (!isFileHandle(handle)) return null;
  let before: number | null = null;
  const recorded: ToolAttachmentView[] = [];
  for (;;) {
    const page = await readPage(before);
    if (page.gap) throw new Error("File reference history is unavailable");
    recorded.push(...(page.events ?? []).flatMap((event) => event.kind.type === "toolCallCompleted"
      ? (event.kind.attachments ?? []).filter((item) => item.handle === handle) : []));
    if (page.complete) return filesFromAttachments(recorded).get(handle) ?? null;
    const next = page.nextCursor?.seq;
    if (next == null || (before !== null && next >= before)) throw new Error("File reference history cursor did not advance");
    before = next;
  }
}

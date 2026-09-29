import { useCallback, useEffect, useRef, useState } from "react";
import type { BlobPutResponse } from "@lightspeed-ai/agent-client";
import {
  ATTACHMENT_SUMMARY,
  MAX_MESSAGE_ATTACHMENTS,
  attachmentLimit,
  attachmentType,
  type AttachmentKind,
  type MessageAttachment,
} from "@lightspeed/platform-shared";
import { api } from "@/api";
import { blobBase64 } from "@/lib/blob-base64";

/// One file in the composer. Uploads start when the file is added, so
/// sending is instant; only a ready attachment is sent.
export interface ComposerAttachment {
  id: string;
  name: string;
  mime: string;
  kind: AttachmentKind;
  size: number;
  status: "uploading" | "ready" | "failed";
  blobRef?: string;
  error?: string;
  /// Object URL of a picked image, for its thumbnail. Absent for documents
  /// and for attachments restored from a saved draft.
  previewUrl?: string;
}

/// An attachment handed to the page on send. Its preview URL now belongs to
/// the page, which shows it in the optimistic transcript echo.
export interface SentAttachment extends MessageAttachment {
  size: number;
  previewUrl?: string;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  const mib = bytes / (1024 * 1024);
  return `${mib < 10 ? mib.toFixed(1) : Math.round(mib)} MB`;
}

/// The short type label shown on a chip, e.g. "PDF" or "PNG".
export function attachmentLabel(attachment: Pick<ComposerAttachment, "mime" | "name">): string {
  return attachmentType(attachment.mime, attachment.name)?.label ?? "File";
}

/// Checks a picked file against the model's accepted types and limits.
/// Returns the accepted MIME type and kind, or why the file is refused.
export function checkAttachment(
  file: Pick<File, "name" | "type" | "size">,
  apiKind: string | undefined,
): { mime: string; kind: AttachmentKind } | { refusal: string } {
  const type = attachmentType(file.type, file.name);
  if (!type) return { refusal: `${file.name} is not a type the model can read.` };
  if (file.size === 0) return { refusal: `${file.name} is empty.` };
  const limit = attachmentLimit(type, apiKind);
  if (file.size > limit) {
    return { refusal: `${file.name} is ${formatBytes(file.size)}. ${type.label} files can be up to ${formatBytes(limit)}.` };
  }
  return { mime: type.mime, kind: type.kind };
}

function restore(key: string): ComposerAttachment[] {
  try {
    const parsed: unknown = JSON.parse(window.localStorage.getItem(key) ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return parsed.flatMap((value): ComposerAttachment[] => {
      const item = value as Partial<ComposerAttachment>;
      return typeof item.id === "string" && typeof item.name === "string" && typeof item.mime === "string"
        && (item.kind === "image" || item.kind === "document") && typeof item.size === "number"
        && typeof item.blobRef === "string"
        ? [{ id: item.id, name: item.name, mime: item.mime, kind: item.kind, size: item.size, blobRef: item.blobRef, status: "ready" }]
        : [];
    });
  } catch {
    return [];
  }
}

function persist(key: string, items: ComposerAttachment[]) {
  try {
    const ready = items.filter((item) => item.status === "ready")
      .map(({ id, name, mime, kind, size, blobRef }) => ({ id, name, mime, kind, size, blobRef }));
    if (ready.length) window.localStorage.setItem(key, JSON.stringify(ready));
    else window.localStorage.removeItem(key);
  } catch {
    // Attachments stay usable in this view when storage is blocked or full.
  }
}

function refusalNotice(refusals: string[]): string {
  if (refusals.length === 0) return "";
  const first = refusals[0]!;
  const more = refusals.length > 1 ? ` ${refusals.length - 1} more ${refusals.length === 2 ? "file was" : "files were"} not attached.` : "";
  const help = /type the model can read/.test(refusals.join(" ")) ? ` Attach ${ATTACHMENT_SUMMARY.charAt(0).toLowerCase()}${ATTACHMENT_SUMMARY.slice(1)}.` : "";
  return `${first}${more}${help}`;
}

/// Composer attachments for one draft. Ready attachments are saved beside
/// the draft text, so they survive navigation the way the text does; a
/// restored attachment points at an uploaded blob and has no thumbnail.
export function useComposerAttachments(universeId: string | undefined, apiKind: string | undefined, storageKey: string) {
  const [items, setItems] = useState<ComposerAttachment[]>(() => restore(storageKey));
  const [notice, setNotice] = useState<string>();
  const itemsRef = useRef(items);
  itemsRef.current = items;
  const files = useRef(new Map<string, File>());
  const uploads = useRef(new Map<string, AbortController>());

  useEffect(() => persist(storageKey, items), [storageKey, items]);
  useEffect(() => () => {
    for (const controller of uploads.current.values()) controller.abort();
    for (const item of itemsRef.current) if (item.previewUrl) URL.revokeObjectURL(item.previewUrl);
  }, []);

  const update = (id: string, patch: Partial<ComposerAttachment>) =>
    setItems((current) => current.map((item) => (item.id === id ? { ...item, ...patch } : item)));

  const upload = useCallback(async (id: string, file: File) => {
    if (!universeId) return;
    const controller = new AbortController();
    uploads.current.get(id)?.abort();
    uploads.current.set(id, controller);
    update(id, { status: "uploading", error: undefined });
    try {
      const bytesBase64 = await blobBase64(file, controller.signal);
      const result = await api<BlobPutResponse>("POST", `/api/v1/universes/${universeId}/attachments`, { bytesBase64 }, controller.signal);
      const blobRef = result.blobs?.[0]?.blobRef;
      if (!blobRef) throw new Error("The upload did not return a reference.");
      if (controller.signal.aborted) return;
      files.current.delete(id);
      update(id, { status: "ready", blobRef });
    } catch (cause) {
      if (controller.signal.aborted) return;
      update(id, { status: "failed", error: cause instanceof Error ? cause.message : "Upload failed." });
    } finally {
      if (uploads.current.get(id) === controller) uploads.current.delete(id);
    }
  }, [universeId]);

  const add = useCallback((picked: readonly File[]) => {
    const refusals: string[] = [];
    const accepted: ComposerAttachment[] = [];
    const present = itemsRef.current;
    for (const file of picked) {
      if (present.length + accepted.length >= MAX_MESSAGE_ATTACHMENTS) {
        refusals.push(`A message can carry up to ${MAX_MESSAGE_ATTACHMENTS} files.`);
        break;
      }
      // The same file picked twice is attached once.
      if ([...present, ...accepted].some((item) => item.name === file.name && item.size === file.size)) continue;
      const checked = checkAttachment(file, apiKind);
      if ("refusal" in checked) {
        refusals.push(checked.refusal);
        continue;
      }
      const id = crypto.randomUUID();
      files.current.set(id, file);
      accepted.push({
        id, name: file.name, mime: checked.mime, kind: checked.kind, size: file.size, status: "uploading",
        ...(checked.kind === "image" && typeof URL.createObjectURL === "function" ? { previewUrl: URL.createObjectURL(file) } : {}),
      });
    }
    setNotice(refusalNotice(refusals) || undefined);
    if (!accepted.length) return;
    setItems((current) => [...current, ...accepted]);
    for (const item of accepted) void upload(item.id, files.current.get(item.id)!);
  }, [apiKind, upload]);

  const remove = useCallback((id: string) => {
    uploads.current.get(id)?.abort();
    uploads.current.delete(id);
    files.current.delete(id);
    const item = itemsRef.current.find((candidate) => candidate.id === id);
    if (item?.previewUrl) URL.revokeObjectURL(item.previewUrl);
    setItems((current) => current.filter((candidate) => candidate.id !== id));
  }, []);

  const retry = useCallback((id: string) => {
    const file = files.current.get(id);
    if (file) void upload(id, file);
    else remove(id);
  }, [upload, remove]);

  /// Hands the ready attachments to a send and clears the composer without
  /// revoking their previews, which now belong to the transcript echo.
  const take = useCallback((): SentAttachment[] => {
    const sent = itemsRef.current.flatMap((item): SentAttachment[] => item.status === "ready" && item.blobRef
      ? [{ blobRef: item.blobRef, mime: item.mime, kind: item.kind, name: item.name, size: item.size, ...(item.previewUrl ? { previewUrl: item.previewUrl } : {}) }]
      : []);
    files.current.clear();
    itemsRef.current = [];
    setItems([]);
    setNotice(undefined);
    return sent;
  }, []);

  return {
    items,
    notice,
    dismissNotice: () => setNotice(undefined),
    add,
    remove,
    retry,
    take,
    uploading: items.some((item) => item.status === "uploading"),
    failed: items.some((item) => item.status === "failed"),
    ready: items.filter((item) => item.status === "ready").length,
  };
}

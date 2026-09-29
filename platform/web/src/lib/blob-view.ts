/// Blob pages: one address per stored blob in a universe,
/// `/u/<slug>/blobs/<hex digest>`. A digest is a capability within its
/// universe, so the page needs no other scope. Name, type, and the session a
/// link came from ride in the query as display hints; the store keeps bytes
/// only.

export interface BlobHints {
  name?: string;
  type?: string;
  /// The session the link was followed from, for a way back.
  session?: string;
}

const HEX = /^[a-f0-9]{64}$/;

/// The digest's hex for a `sha256:` reference, or null for anything else.
export function blobDigest(blobRef: string): string | null {
  const hex = blobRef.startsWith("sha256:") ? blobRef.slice("sha256:".length) : blobRef;
  return HEX.test(hex) ? hex : null;
}

export function blobHref(slug: string, blobRef: string, hints: BlobHints = {}): string | null {
  const digest = blobDigest(blobRef);
  if (!digest) return null;
  const query = new URLSearchParams();
  if (hints.name) query.set("name", hints.name);
  if (hints.type) query.set("type", hints.type);
  if (hints.session) query.set("session", hints.session);
  const search = query.toString();
  return `/u/${slug}/blobs/${digest}${search ? `?${search}` : ""}`;
}

/// A browser address for an app route. Router links add the app's base path
/// (`/app`, `/demo`) themselves; a plain link opened in a new tab must carry
/// it.
export function appHref(path: string): string {
  return `${import.meta.env.BASE_URL.replace(/\/$/, "")}${path}`;
}

export type BlobView =
  | { kind: "image"; mime: string }
  | { kind: "pdf"; mime: "application/pdf" }
  | { kind: "text"; mime: string; text: string }
  | { kind: "binary"; mime: string };

function startsWith(bytes: Uint8Array, signature: number[], offset = 0): boolean {
  return signature.every((byte, index) => bytes[offset + index] === byte);
}

/// The image type the bytes carry, whatever the link claims.
function sniffImage(bytes: Uint8Array): string | null {
  if (startsWith(bytes, [0x89, 0x50, 0x4e, 0x47])) return "image/png";
  if (startsWith(bytes, [0xff, 0xd8, 0xff])) return "image/jpeg";
  if (startsWith(bytes, [0x47, 0x49, 0x46, 0x38])) return "image/gif";
  if (startsWith(bytes, [0x52, 0x49, 0x46, 0x46]) && startsWith(bytes, [0x57, 0x45, 0x42, 0x50], 8)) return "image/webp";
  return null;
}

/// How to show a blob, decided by its bytes. Images and PDFs go to the
/// browser's own viewers; UTF-8 text is shown exactly as stored, with no
/// Markdown rendering or reformatting; anything else is download-only. Nothing
/// is ever shown as a web page, so stored HTML or SVG appears as its source.
export function blobView(bytes: Uint8Array, hint: string | undefined): BlobView {
  const image = sniffImage(bytes);
  if (image) return { kind: "image", mime: image };
  if (startsWith(bytes, [0x25, 0x50, 0x44, 0x46, 0x2d])) return { kind: "pdf", mime: "application/pdf" };
  const type = hint?.split(";")[0]?.trim().toLowerCase() || undefined;
  try {
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return { kind: "text", mime: type && !type.startsWith("image/") && type !== "application/pdf" ? type : "text/plain", text };
  } catch {
    return { kind: "binary", mime: "application/octet-stream" };
  }
}

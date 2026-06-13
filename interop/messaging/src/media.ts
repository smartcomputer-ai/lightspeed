/// Document support (P71 G3): mirrors the gateway admission allowlist.
/// PDF goes to providers natively; the text MIMEs are inlined as text.
const ALLOWED_DOCUMENT_MIMES = new Set([
  "application/pdf",
  "text/plain",
  "text/markdown",
  "text/csv",
  "application/json",
]);

/// Channels often report generic MIMEs (application/octet-stream) for text
/// files; the extension is the more reliable signal.
const DOCUMENT_MIME_BY_EXTENSION: Record<string, string> = {
  pdf: "application/pdf",
  txt: "text/plain",
  text: "text/plain",
  log: "text/plain",
  md: "text/markdown",
  markdown: "text/markdown",
  csv: "text/csv",
  json: "application/json",
};

export const MAX_PDF_BYTES = 10 * 1024 * 1024;
/// Text documents land in model context verbatim (gateway cap).
export const MAX_TEXT_DOCUMENT_BYTES = 1024 * 1024;

/// Resolves an inbound attachment to a gateway-admissible document MIME, or
/// null when the type is unsupported.
export function documentMime(
  fileName: string | null | undefined,
  reportedMime: string | null | undefined,
): string | null {
  const extension = fileName?.toLowerCase().split(".").at(-1);
  const byExtension = extension ? DOCUMENT_MIME_BY_EXTENSION[extension] : undefined;
  if (byExtension) {
    return byExtension;
  }
  const reported = reportedMime?.toLowerCase().split(";")[0]?.trim();
  return reported && ALLOWED_DOCUMENT_MIMES.has(reported) ? reported : null;
}

export function documentByteLimit(mime: string): number {
  return mime === "application/pdf" ? MAX_PDF_BYTES : MAX_TEXT_DOCUMENT_BYTES;
}

export function mediaKindForMime(mime: string): "image" | "document" {
  return mime.startsWith("image/") ? "image" : "document";
}

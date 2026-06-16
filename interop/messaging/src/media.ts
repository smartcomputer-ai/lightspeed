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

/// Audio support (P72 G1): mirrors the gateway admission allowlist.
const ALLOWED_AUDIO_MIMES = new Set([
  "audio/mpeg",
  "audio/mp4",
  "audio/wav",
  "audio/webm",
  "audio/ogg",
]);

/// Channels may report container-specific or legacy audio MIME names. Normalize
/// to the gateway allowlist where the container is equivalent.
const AUDIO_MIME_BY_EXTENSION: Record<string, string> = {
  mp3: "audio/mpeg",
  mpeg: "audio/mpeg",
  mpga: "audio/mpeg",
  m4a: "audio/mp4",
  mp4: "audio/mp4",
  wav: "audio/wav",
  wave: "audio/wav",
  webm: "audio/webm",
  oga: "audio/ogg",
  ogg: "audio/ogg",
  opus: "audio/ogg",
};

const AUDIO_MIME_ALIASES: Record<string, string> = {
  "audio/mp3": "audio/mpeg",
  "audio/x-m4a": "audio/mp4",
  "audio/m4a": "audio/mp4",
  "audio/x-wav": "audio/wav",
  "audio/wave": "audio/wav",
  "audio/vnd.wave": "audio/wav",
  "audio/oga": "audio/ogg",
  "audio/opus": "audio/ogg",
};

export const MAX_PDF_BYTES = 10 * 1024 * 1024;
/// Text documents land in model context verbatim (gateway cap).
export const MAX_TEXT_DOCUMENT_BYTES = 1024 * 1024;
export const MAX_AUDIO_BYTES = 25 * 1024 * 1024;

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

/// Resolves inbound voice/audio to a gateway-admissible audio MIME, or null
/// when the type is unsupported.
export function audioMime(
  fileName: string | null | undefined,
  reportedMime: string | null | undefined,
): string | null {
  const extension = fileName?.toLowerCase().split(".").at(-1);
  const byExtension = extension ? AUDIO_MIME_BY_EXTENSION[extension] : undefined;
  if (byExtension) {
    return byExtension;
  }
  const reported = reportedMime?.toLowerCase().split(";")[0]?.trim();
  if (!reported) {
    return null;
  }
  const normalized = AUDIO_MIME_ALIASES[reported] ?? reported;
  return ALLOWED_AUDIO_MIMES.has(normalized) ? normalized : null;
}

export function mediaKindForMime(mime: string): "image" | "audio" | "document" {
  return mime.startsWith("image/") ? "image" : mime.startsWith("audio/") ? "audio" : "document";
}

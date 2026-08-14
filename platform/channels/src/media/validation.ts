import type { ChannelMediaKind } from "../contracts/media.js";

const IMAGE_MIMES = new Set(["image/jpeg", "image/png", "image/webp", "image/gif"]);
const DOCUMENT_MIMES = new Set([
  "application/pdf",
  "text/plain",
  "text/markdown",
  "text/csv",
  "application/json",
]);
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
const AUDIO_MIMES = new Set([
  "audio/mpeg",
  "audio/mp4",
  "audio/wav",
  "audio/webm",
  "audio/ogg",
  "audio/aac",
  "audio/amr",
  "audio/3gpp",
  "audio/3gpp2",
]);
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
  aac: "audio/aac",
  amr: "audio/amr",
  "3gp": "audio/3gpp",
  "3gpp": "audio/3gpp",
  "3g2": "audio/3gpp2",
  "3gpp2": "audio/3gpp2",
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
  "audio/x-aac": "audio/aac",
  "audio/3gp": "audio/3gpp",
  "audio/3g2": "audio/3gpp2",
};

export const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
export const MAX_PDF_BYTES = 10 * 1024 * 1024;
export const MAX_TEXT_DOCUMENT_BYTES = 1024 * 1024;
export const MAX_AUDIO_BYTES = 25 * 1024 * 1024;

export function imageMime(reportedMime: string | null | undefined): string | null {
  const mime = cleanMime(reportedMime);
  return mime !== null && IMAGE_MIMES.has(mime) ? mime : null;
}

export function documentMime(
  fileName: string | null | undefined,
  reportedMime: string | null | undefined,
): string | null {
  const byExtension = extensionOf(fileName)
    ? DOCUMENT_MIME_BY_EXTENSION[extensionOf(fileName) as string]
    : undefined;
  if (byExtension !== undefined) {
    return byExtension;
  }
  const mime = cleanMime(reportedMime);
  return mime !== null && DOCUMENT_MIMES.has(mime) ? mime : null;
}

export function audioMime(
  fileName: string | null | undefined,
  reportedMime: string | null | undefined,
): string | null {
  const extension = extensionOf(fileName);
  const byExtension = extension === null ? undefined : AUDIO_MIME_BY_EXTENSION[extension];
  if (byExtension !== undefined) {
    return byExtension;
  }
  const reported = cleanMime(reportedMime);
  if (reported === null) {
    return null;
  }
  const normalized = AUDIO_MIME_ALIASES[reported] ?? reported;
  return AUDIO_MIMES.has(normalized) ? normalized : null;
}

export function mediaByteLimit(kind: ChannelMediaKind, mime: string): number {
  if (kind === "image") return MAX_IMAGE_BYTES;
  if (kind === "audio") return MAX_AUDIO_BYTES;
  return mime === "application/pdf" ? MAX_PDF_BYTES : MAX_TEXT_DOCUMENT_BYTES;
}

function cleanMime(value: string | null | undefined): string | null {
  const mime = value?.toLowerCase().split(";", 1)[0]?.trim();
  return mime ? mime : null;
}

function extensionOf(fileName: string | null | undefined): string | null {
  const extension = fileName?.toLowerCase().split(".").at(-1);
  return extension && extension !== fileName?.toLowerCase() ? extension : null;
}

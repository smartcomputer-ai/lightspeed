import type { FileFacts } from "./types";

/// Media types by extension, for files the workspace recorded none for and
/// for the type a saved or new file is written with.
const MEDIA_TYPES: Record<string, string> = {
  md: "text/markdown",
  markdown: "text/markdown",
  txt: "text/plain",
  json: "application/json",
  yaml: "application/yaml",
  yml: "application/yaml",
  toml: "application/toml",
  ts: "text/typescript",
  js: "text/javascript",
  py: "text/x-python",
  sh: "text/x-shellscript",
  css: "text/css",
  html: "text/html",
  pdf: "application/pdf",
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  svg: "image/svg+xml",
};

/// The extension of a path, lower case and without the dot.
export function extensionOf(path: string): string {
  const name = path.split("/").at(-1) ?? path;
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/// The media type a path's extension stands for, if it is one we know.
export function mediaTypeFor(path: string): string | undefined {
  return MEDIA_TYPES[extensionOf(path)];
}

/// The file's media type: the recorded one, else the extension's; lower
/// case, without parameters.
export function effectiveMediaType(file: Pick<FileFacts, "mediaType" | "path">): string | undefined {
  const recorded = file.mediaType?.split(";")[0]?.trim().toLowerCase();
  return recorded || mediaTypeFor(file.path);
}

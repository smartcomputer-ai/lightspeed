import { useMemo } from "react";
import { PreviewIcon } from "@/icons";
import type { FileModeProps, FileProvider } from "../types";
import { effectiveMediaType } from "../media-types";

/// Bytes as base64, in chunks so a large image does not overflow the
/// argument list.
function base64Of(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

/// The image as an image (an SVG too: never inline, so its markup does not
/// run as part of the app).
function ImagePreview({ file, content }: FileModeProps) {
  const src = useMemo(
    () => (content.kind === "bytes" ? `data:${content.mediaType};base64,${base64Of(content.bytes)}` : null),
    [content],
  );
  if (!src) return null;
  return (
    <div className="flex items-start p-4">
      <img alt={file.path} className="max-w-full rounded-lg border" src={src} />
    </div>
  );
}

/// Images: a preview only, so no toggle.
export const imageProvider: FileProvider = {
  id: "image",
  label: "Image",
  match: (file) => (effectiveMediaType(file)?.startsWith("image/") ? 90 : null),
  content: "bytes",
  modes: [{ id: "preview", label: "Preview", icon: PreviewIcon, editable: false, component: ImagePreview }],
};

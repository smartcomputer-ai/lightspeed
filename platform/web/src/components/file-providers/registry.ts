import type { FileFacts, FileProvider } from "./types";
import { extensionOf } from "./media-types";
import { fallbackProvider } from "./providers/fallback";
import { imageProvider } from "./providers/image";
import { markdownProvider } from "./providers/markdown";
import { pdfProvider } from "./providers/pdf";
import { textProvider } from "./providers/text";

/// Every provider, in no particular order: the best match wins, and the
/// fallback matches everything with the lowest score.
export const FILE_PROVIDERS: readonly FileProvider[] = [
  pdfProvider,
  imageProvider,
  markdownProvider,
  textProvider,
  fallbackProvider,
];

/// The provider that handles a file best.
export function providerFor(file: FileFacts, providers: readonly FileProvider[] = FILE_PROVIDERS): FileProvider {
  let best = fallbackProvider;
  let bestScore = -1;
  for (const provider of providers) {
    const score = provider.match(file);
    if (score !== null && score > bestScore) {
      best = provider;
      bestScore = score;
    }
  }
  return best;
}

/// The facts a provider decides on, from a workspace file and its first bytes.
export function fileFacts(
  path: string,
  file: { media_type?: string | null; size_bytes: number } | null | undefined,
  head: Uint8Array,
): FileFacts {
  return {
    path,
    name: path.split("/").at(-1) ?? path,
    extension: extensionOf(path),
    ...(file?.media_type ? { mediaType: file.media_type } : {}),
    sizeBytes: file?.size_bytes ?? head.length,
    head,
  };
}

/// Whether a provider has a mode that changes the file.
export const canEdit = (provider: FileProvider) => provider.modes.some((mode) => mode.editable);

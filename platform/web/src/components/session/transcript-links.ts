import { createContext } from "react";
import type { TranscriptMedia } from "@/lib/sessions/transcript";

/// Links the transcript cannot derive on its own: bot display names for
/// Emit rows, where a sub-agent's child session opens, and how media bytes
/// are read and resolved by handle.
export interface TranscriptLinks {
  botName?: (botId: string) => string | undefined;
  sessionHref?: (sessionId: string) => string;
  navigate?: (href: string) => void;
  /// Read a media blob and return a URL an `img` or link can use.
  loadMedia?: (blobRef: string, mime: string) => Promise<string>;
  /// Every media item the transcript has folded, by `media:` handle, for
  /// resolving the handles assistant text references.
  mediaByHandle?: Map<string, TranscriptMedia>;
}

export const TranscriptLinksContext = createContext<TranscriptLinks>({});

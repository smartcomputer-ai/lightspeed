import { Loader2, TriangleAlert } from "lucide-react";
import { Bubble, BubbleContent } from "@/components/ui/bubble";
import { Marker, MarkerContent, MarkerIcon } from "@/components/ui/marker";
import { Message, MessageContent } from "@/components/ui/message";
import { MarkdownContent } from "@/components/session/markdown-content";
import { ReasoningTrace, ToolGroupTrace } from "@/components/session/tool-trace";
import {
  type ActiveRun,
  type TranscriptEntry,
} from "@/lib/sessions/transcript";
import { cn } from "@/lib/utils";

/// Coding-bot transcript idiom: no avatars, full-width rows. User input
/// is a tinted band, assistant output plain rendered text, tool activity
/// and lifecycle notes are compact marker rows.

export function TranscriptEntryView({ entry }: { entry: TranscriptEntry }) {
  switch (entry.kind) {
    case "message":
      return entry.role === "user" ? (
        <UserBand text={entry.text} />
      ) : (
        <Message>
          <MessageContent>
            <Bubble variant="ghost" className="max-w-full">
              <BubbleContent>
                <MarkdownContent>{entry.text}</MarkdownContent>
              </BubbleContent>
            </Bubble>
          </MessageContent>
        </Message>
      );
    case "system":
      return (
        <Marker variant="separator">
          <MarkerContent>{entry.text}</MarkerContent>
        </Marker>
      );
    case "reasoning":
      return <ReasoningTrace text={entry.text} />;
    case "tool-group":
      return <ToolGroupTrace group={entry} />;
    case "marker":
      return entry.tone === "error" ? (
        <Marker className="text-destructive">
          <MarkerIcon>
            <TriangleAlert />
          </MarkerIcon>
          <MarkerContent>{entry.text}</MarkerContent>
        </Marker>
      ) : (
        <Marker variant="separator">
          <MarkerContent>{entry.text}</MarkerContent>
        </Marker>
      );
  }
}

export function UserBand({ text, pending = false }: { text: string; pending?: boolean }) {
  return (
    <Message>
      <MessageContent>
        <Bubble variant="muted" className={cn("w-full max-w-full", pending && "opacity-60")}>
          <BubbleContent className="w-full whitespace-pre-wrap">{text}</BubbleContent>
        </Bubble>
      </MessageContent>
    </Message>
  );
}

/// Live spinner row while a run is in flight, with the TUI's status
/// vocabulary (queued / running / thinking / running tools / …).
export function ActiveRunMarker({ run }: { run: ActiveRun }) {
  return (
    <Marker role="status">
      <MarkerIcon>
        <Loader2 className="animate-spin" />
      </MarkerIcon>
      <MarkerContent>{run.label}…</MarkerContent>
    </Marker>
  );
}

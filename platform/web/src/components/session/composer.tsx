import { useState, type KeyboardEvent, type ReactNode } from "react";
import { ArrowUp, LoaderCircle, Square } from "lucide-react";
import { Button } from "@/components/ui/button";
import { readSessionDraft, writeSessionDraft } from "@/lib/sessions/draft";

/// How a message sent while a run is in progress is delivered.
/// - `queue`: starts the next run once the active one (and anything
///   already queued) has finished. Enter.
/// - `steer`: injected into the active run; the model sees it at its next
///   turn boundary without interrupting the in-flight turn. ⌘/Ctrl+Enter.
export type ComposerMode = "steer" | "queue";

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
const steerKeyLabel = isMac ? "⌘↵" : "Ctrl+↵";

/// Chat input pinned under the transcript. Enter sends, Shift+Enter adds a
/// newline. While a run is in progress the input stays live: Enter queues
/// the message as the next run, ⌘/Ctrl+Enter steers it into the current
/// run, and a Stop button cancels the active run. Closed sessions render
/// the composer read-only for transcript inspection.
export function SessionComposer({
  draftKey,
  runActive,
  canSteer,
  stopping = false,
  disabled = false,
  disabledReason,
  banner,
  error,
  onSend,
  onStop,
}: {
  /// Stable universe + session storage key; also used as the React key.
  draftKey: string;
  /// A run is running, cancelling, or queued: Enter queues, ⌘/Ctrl+Enter
  /// steers.
  runActive: boolean;
  /// The active run accepts steering (it is running or parked, not
  /// cancelling and not merely queued).
  canSteer: boolean;
  /// A cancel is in flight for the active run.
  stopping?: boolean;
  disabled?: boolean;
  disabledReason?: string;
  /// Rendered above the input, inside the composer block (e.g. the
  /// managed-session direct-input override).
  banner?: ReactNode;
  error: string | null;
  onSend: (text: string, mode: ComposerMode | null) => void;
  onStop: () => void;
}) {
  const [text, setText] = useState(() => readSessionDraft(draftKey));
  const updateText = (value: string) => {
    setText(value);
    // Write on input rather than on unmount, so navigation cannot lose edits.
    writeSessionDraft(draftKey, value);
  };

  const submit = (steer: boolean) => {
    const trimmed = text.trim();
    if (!trimmed || disabled) {
      return;
    }
    // ⌘/Ctrl+Enter while nothing can be steered is reported by the page
    // (the text stays in the box) rather than silently queued — the
    // difference matters to the reader.
    const mode: ComposerMode | null = !runActive ? null : steer ? "steer" : "queue";
    onSend(trimmed, mode);
    if (mode !== "steer" || canSteer) {
      updateText("");
    }
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) {
      return;
    }
    event.preventDefault();
    submit(event.metaKey || event.ctrlKey);
  };

  const placeholder = disabled
    ? disabledReason ?? "This session is closed"
    : runActive
      ? canSteer
        ? `Enter queues the next message · ${steerKeyLabel} steers the current run…`
        : "Enter queues the next message…"
      : "Message the agent…";

  return (
    <div className="shrink-0 border-t py-3">
      <div className="mx-auto w-full max-w-5xl px-4 md:px-8">
        {banner}
        {error && <p className="pb-2 text-xs text-destructive">{error}</p>}
        {disabled && disabledReason && !banner && (
          <p className="pb-2 text-xs text-muted-foreground">{disabledReason}</p>
        )}
        <div className="flex items-center gap-2">
        <textarea
          disabled={disabled}
          value={text}
          onChange={(event) => updateText(event.target.value)}
          onKeyDown={onKeyDown}
          placeholder={placeholder}
          aria-label="Message"
          rows={1}
          className="field-sizing-content max-h-40 min-h-9 min-w-0 flex-1 resize-none rounded-md border bg-background px-3 py-2 text-base md:text-sm [@media(pointer:coarse)]:text-base outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
        />
        {runActive && !disabled && (
          <Button
            variant="outline"
            size="icon"
            onClick={onStop}
            disabled={stopping}
            aria-label={stopping ? "Stopping run" : "Stop run"}
            title={stopping ? "Stopping the active run…" : "Stop the active run"}
          >
            {stopping ? <LoaderCircle className="animate-spin" /> : <Square />}
          </Button>
        )}
        <Button
          size="icon"
          onClick={() => submit(false)}
          disabled={disabled || !text.trim()}
          aria-label={runActive ? "Queue message" : "Send message"}
          title={runActive
            ? canSteer
              ? `Queue as the next run (${steerKeyLabel} in the box steers the current run)`
              : "Queue as the next run"
            : "Send"}
        >
          <ArrowUp />
        </Button>
        </div>
      </div>
    </div>
  );
}

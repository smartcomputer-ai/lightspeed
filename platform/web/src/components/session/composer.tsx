import { useState, type KeyboardEvent } from "react";
import { ArrowUp, Square } from "lucide-react";
import { Button } from "@/components/ui/button";

/// Chat input pinned under the transcript. Enter sends, Shift+Enter adds
/// a newline. While a run is active, submitting is blocked (engine
/// semantics: one run at a time) and the send button becomes Stop —
/// typing stays possible so the next message can be drafted.
/// Closed sessions render the composer read-only for transcript inspection.
export function SessionComposer({
  runActive,
  disabled = false,
  disabledReason,
  error,
  onSend,
  onStop,
}: {
  runActive: boolean;
  disabled?: boolean;
  disabledReason?: string;
  error: string | null;
  onSend: (text: string) => void;
  onStop: () => void;
}) {
  const [text, setText] = useState("");

  const submit = () => {
    const trimmed = text.trim();
    if (!trimmed || runActive || disabled) {
      return;
    }
    onSend(trimmed);
    setText("");
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      submit();
    }
  };

  return (
    <div className="shrink-0 border-t px-4 py-3 md:px-8">
      {error && <p className="pb-2 text-xs text-destructive">{error}</p>}
      {disabled && disabledReason && (
        <p className="pb-2 text-xs text-muted-foreground">{disabledReason}</p>
      )}
      <div className="flex items-end gap-2">
        <textarea
          disabled={disabled}
          value={text}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={onKeyDown}
          placeholder={disabled
            ? disabledReason ?? "This session is closed"
            : runActive
              ? "Run in progress — Stop to interrupt"
              : "Message the agent…"}
          aria-label="Message"
          rows={1}
          className="field-sizing-content max-h-40 min-h-9 flex-1 resize-none rounded-md border bg-background px-3 py-2 text-sm outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
        />
        {runActive && !disabled ? (
          <Button
            variant="outline"
            size="icon"
            onClick={onStop}
            aria-label="Stop run"
            title="Stop the active run"
          >
            <Square />
          </Button>
        ) : (
          <Button
            size="icon"
            onClick={submit}
            disabled={disabled || !text.trim()}
            aria-label="Send message"
          >
            <ArrowUp />
          </Button>
        )}
      </div>
    </div>
  );
}

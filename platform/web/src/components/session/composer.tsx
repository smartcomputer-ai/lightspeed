import { useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { ArrowUp, LoaderCircle, Mic, Square, X } from "lucide-react";
import { Link } from "react-router-dom";
import { useDictation } from "@/lib/use-dictation";
import { isDemoDictation } from "@/lib/audio-capture";
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
  dictation,
  runActive,
  canSteer,
  canStop = false,
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
  dictation?: { universeId: string; disabledReason?: string; settingsHref?: string };
  /// A run is running, cancelling, or queued: Enter queues, ⌘/Ctrl+Enter
  /// steers.
  runActive: boolean;
  /// The active run accepts steering (it is running or parked, not
  /// cancelling and not merely queued).
  canSteer: boolean;
  /** Stopping is independent of permission to send or steer messages. */
  canStop?: boolean;
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
  const textRef = useRef(text);
  const textarea = useRef<HTMLTextAreaElement>(null);
  const [dictationNotice, setDictationNotice] = useState<string>();
  const updateText = (value: string) => {
    textRef.current = value;
    setDictationNotice(undefined);
    setText(value);
    // Write on input rather than on unmount, so navigation cannot lose edits.
    writeSessionDraft(draftKey, value);
  };

  const voice = useDictation(dictation?.universeId, Boolean(dictation && !dictation.disabledReason && !disabled), (transcript) => {
    const current = textRef.current;
    updateText(`${current}${current && !/\s$/.test(current) ? " " : ""}${transcript}`);
    setDictationNotice("Dictation added. Review and edit before sending.");
    textarea.current?.focus();
  });
  const voiceBusy = ["requesting", "recording", "transcribing"].includes(voice.phase);

  const submit = (steer: boolean) => {
    const trimmed = text.trim();
    if (!trimmed || disabled) {
      return;
    }
    // ⌘/Ctrl+Enter while nothing can be steered is reported by the page
    // (the text stays in the box) rather than silently queued — the
    // difference matters to the reader.
    const mode: ComposerMode | null = !runActive ? null : steer ? "steer" : "queue";
    if (mode !== "steer" || canSteer) voice.cancel();
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
          ref={textarea}
          disabled={disabled}
          value={text}
          onChange={(event) => updateText(event.target.value)}
          onKeyDown={onKeyDown}
          placeholder={placeholder}
          aria-label="Message"
          rows={1}
          className="field-sizing-content max-h-40 min-h-9 min-w-0 flex-1 resize-none rounded-md border bg-background px-3 py-2 text-base md:text-sm [@media(pointer:coarse)]:text-base outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
        />
        {dictation && <span title={dictation.disabledReason ?? "Dictate a message, then review before sending"}>
          <Button variant="outline" size="icon"
            disabled={disabled || !!dictation.disabledReason || voice.phase === "requesting" || voice.phase === "transcribing"}
            aria-label={voice.phase === "recording" ? "Stop recording" : "Dictate message"}
            onClick={() => { setDictationNotice(undefined); void (voice.phase === "recording" ? voice.stop() : voice.start()); }}>
            {voice.phase === "recording" ? <Square className="text-destructive" /> : voiceBusy ? <LoaderCircle className="animate-spin" /> : <Mic />}
          </Button>
        </span>}
        {runActive && canStop && (
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
        {dictation && !disabled && <div className="mt-2 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
          <span role="status" aria-live="polite">{dictation.disabledReason ?? (
            voice.phase === "recording" ? `${isDemoDictation ? "Demo recording" : "Recording"} ${Math.floor(voice.seconds / 60)}:${String(voice.seconds % 60).padStart(2, "0")} · Stop to transcribe`
            : voice.phase === "requesting" ? "Waiting for microphone permission…"
            : voice.phase === "transcribing" ? "Transcribing… You can keep editing."
            : dictationNotice ?? (isDemoDictation ? "Demo dictation adds a sample transcript." : "")
          )}</span>
          {dictation.disabledReason && dictation.settingsHref && <Link className="underline underline-offset-2" to={dictation.settingsHref}>Models</Link>}
          {voice.error && <span role="alert" className="text-destructive">{voice.error}</span>}
          {voice.canRetry && <Button variant="outline" size="sm" onClick={voice.retry}>Retry transcription</Button>}
          {(voiceBusy || voice.phase === "error") && <Button variant="ghost" size="sm" onClick={voice.cancel}><X />Cancel dictation</Button>}
        </div>}
      </div>
    </div>
  );
}

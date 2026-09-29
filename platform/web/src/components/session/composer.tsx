import { useEffect, useRef, useState, type DragEvent, type KeyboardEvent, type ReactNode } from "react";
import { ArrowUp, ChevronDown, CornerDownRight, ListPlus, LoaderCircle, Lock, Plus, Square, TriangleAlert, X } from "lucide-react";
import { ATTACHMENT_ACCEPT, ATTACHMENT_SUMMARY, type MessageRunOptions } from "@lightspeed/platform-shared";
import type { ModelOption } from "@/api";
import { useDictation } from "@/lib/use-dictation";
import { isDemoDictation } from "@/lib/audio-capture";
import { useComposerAttachments, type SentAttachment } from "@/lib/composer-attachments";
import {
  composerModelChoice,
  configWithRunChoice,
  normalizeRunChoice,
  readRunChoice,
  writeRunChoice,
  type RunChoice,
} from "@/lib/composer-model";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuShortcut, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { readSessionDraft, writeSessionDraft } from "@/lib/sessions/draft";
import { cn } from "@/lib/utils";
import { AttachmentStrip } from "./composer-attachments";
import { ModelPicker } from "./composer-model-picker";
import { VoiceControl } from "./composer-voice";

/// How a message sent while a run is in progress is delivered.
/// - `queue`: starts the next run once the active one (and anything
///   already queued) has finished. Enter.
/// - `steer`: injected into the active run; the model sees it at its next
///   turn boundary without interrupting the in-flight turn. ⌘/Ctrl+Enter.
export type ComposerMode = "steer" | "queue";

/// What one send carries. Options are per-message model overrides; steering
/// never carries them because it joins a run that already chose.
export interface ComposerMessage {
  text: string;
  attachments: SentAttachment[];
  options?: MessageRunOptions;
}

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
const steerKeyLabel = isMac ? "⌘↵" : "Ctrl+↵";
const VOICE_BUSY = new Set(["requesting", "recording", "transcribing", "error"]);

/// Chat input pinned under the transcript: one capsule that holds the
/// attachments, the message, and a toolbar with attach, the model pill,
/// dictation, and send. Every status the composer has (uploads, recording,
/// transcription, errors) is shown inside the capsule on the control that
/// owns it. Enter sends, Shift+Enter adds a newline. While a run is in
/// progress Enter queues the message as the next run, ⌘/Ctrl+Enter steers
/// it into the current run, and Stop cancels the active run. Closed
/// sessions render the composer read-only for transcript inspection.
export function SessionComposer({
  draftKey,
  dictation,
  attachments: attachmentOptions,
  model,
  runActive,
  canSteer,
  canStop = false,
  stopping = false,
  disabled = false,
  disabledReason,
  banner,
  error,
  onDismissError,
  onSend,
  onStop,
}: {
  /// Stable universe + session storage key; also used as the React key.
  draftKey: string;
  dictation?: { universeId: string; disabledReason?: string; settingsHref?: string };
  /// Enables images and documents. `apiKind` is the session's agent API,
  /// which decides some size limits.
  attachments?: { universeId: string; apiKind?: string };
  /// Enables the model pill for the session's configured route.
  model?: {
    config: unknown;
    models?: readonly ModelOption[];
    canSaveDefault: boolean;
    /// Stores a complete session config with the pill's choice folded in.
    onSaveDefault: (config: Record<string, unknown>) => Promise<void>;
  };
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
  /// Rendered above the capsule (e.g. the managed-session direct-input
  /// override).
  banner?: ReactNode;
  error: string | null;
  onDismissError?: () => void;
  onSend: (message: ComposerMessage, mode: ComposerMode | null) => void;
  onStop: () => void;
}) {
  const [text, setText] = useState(() => readSessionDraft(draftKey));
  const textRef = useRef(text);
  const textarea = useRef<HTMLTextAreaElement>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  /// Where the caret was when the field last lost focus, so dictation lands
  /// there if the text has not changed since.
  const selection = useRef<{ start: number; end: number; text: string } | null>(null);
  const caretAfterInsert = useRef<number | null>(null);
  const [announcement, setAnnouncement] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [flash, setFlash] = useState(false);
  const [dragging, setDragging] = useState(false);
  const dragDepth = useRef(0);
  const [pendingSubmit, setPendingSubmit] = useState<"send" | "steer" | null>(null);

  const updateText = (value: string) => {
    textRef.current = value;
    setAnnouncement(undefined);
    setText(value);
    // Write on input rather than on unmount, so navigation cannot lose edits.
    writeSessionDraft(draftKey, value);
  };

  const files = useComposerAttachments(attachmentOptions?.universeId, attachmentOptions?.apiKind, `${draftKey}:attachments`);
  const canAttach = Boolean(attachmentOptions) && !disabled;

  const runKey = `${draftKey}:run`;
  const [storedChoice, setStoredChoice] = useState<RunChoice>(() => readRunChoice(runKey));
  const modelChoice = model ? composerModelChoice(model.config, model.models, storedChoice) : null;
  const changeChoice = (next: RunChoice) => {
    const normalized = modelChoice ? normalizeRunChoice(next, modelChoice.session) : next;
    setStoredChoice(normalized);
    writeRunChoice(runKey, normalized);
  };

  const voice = useDictation(dictation?.universeId, Boolean(dictation && !dictation.disabledReason && !disabled), (transcript) => {
    const current = textRef.current;
    const at = selection.current;
    let next: string;
    let caret: number;
    if (at && at.text === current && at.start < current.length) {
      const before = current.slice(0, at.start);
      const after = current.slice(at.end);
      const lead = before && !/\s$/.test(before) ? " " : "";
      const trail = after && !/^\s/.test(after) ? " " : "";
      next = `${before}${lead}${transcript}${trail}${after}`;
      caret = before.length + lead.length + transcript.length;
    } else {
      next = `${current}${current && !/\s$/.test(current) ? " " : ""}${transcript}`;
      caret = next.length;
    }
    updateText(next);
    selection.current = null;
    caretAfterInsert.current = caret;
    setAnnouncement("Dictation added. Review and edit before sending.");
    setFlash(true);
  });

  useEffect(() => {
    const caret = caretAfterInsert.current;
    if (caret === null) return;
    caretAfterInsert.current = null;
    const input = textarea.current;
    input?.focus();
    input?.setSelectionRange(caret, caret);
  }, [text]);
  useEffect(() => {
    if (!flash) return;
    const timer = setTimeout(() => setFlash(false), 1400);
    return () => clearTimeout(timer);
  }, [flash]);

  const hasContent = text.trim().length > 0 || files.items.length > 0;

  const submit = (steer: boolean) => {
    if (disabled) return;
    // Enter while recording finishes the recording; the transcript still
    // needs a review before anything is sent.
    if (voice.phase === "recording") {
      void voice.stop();
      return;
    }
    if (!hasContent) return;
    if (files.failed) {
      setNotice("Remove or retry the attachments that failed to upload.");
      return;
    }
    if (files.uploading) {
      setPendingSubmit(steer ? "steer" : "send");
      return;
    }
    const mode: ComposerMode | null = !runActive ? null : steer ? "steer" : "queue";
    if (mode === "steer" && !canSteer) {
      setNotice(`There is no run to steer right now. Press Enter to queue the message instead.`);
      return;
    }
    voice.cancel();
    setPendingSubmit(null);
    setNotice(undefined);
    const attachments = files.take();
    onSend({
      text: text.trim(),
      attachments,
      ...(mode !== "steer" && modelChoice?.options ? { options: modelChoice.options } : {}),
    }, mode);
    updateText("");
  };

  // A send asked for while uploads were running goes out once they finish.
  useEffect(() => {
    if (!pendingSubmit || files.uploading) return;
    if (!hasContent) {
      setPendingSubmit(null);
      return;
    }
    submit(pendingSubmit === "steer");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingSubmit, files.uploading]);

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Escape" && VOICE_BUSY.has(voice.phase)) {
      event.preventDefault();
      voice.cancel();
      return;
    }
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) {
      return;
    }
    event.preventDefault();
    submit(event.metaKey || event.ctrlKey);
  };

  const startVoice = () => {
    setAnnouncement(undefined);
    void voice.start();
  };

  const hasFiles = (event: DragEvent) => canAttach && [...event.dataTransfer.types].includes("Files");
  const dropHandlers = {
    onDragEnter: (event: DragEvent) => {
      if (!hasFiles(event)) return;
      dragDepth.current += 1;
      setDragging(true);
    },
    onDragOver: (event: DragEvent) => {
      if (!hasFiles(event)) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = "copy";
    },
    onDragLeave: (event: DragEvent) => {
      if (!hasFiles(event)) return;
      dragDepth.current = Math.max(0, dragDepth.current - 1);
      if (dragDepth.current === 0) setDragging(false);
    },
    onDrop: (event: DragEvent) => {
      if (!hasFiles(event)) return;
      event.preventDefault();
      dragDepth.current = 0;
      setDragging(false);
      files.add([...event.dataTransfer.files]);
      textarea.current?.focus();
    },
  };

  // A disabled composer states its reason as text in the capsule, where the
  // toolbar would be; the banner states it instead when there is one.
  const reasonLine = disabled && disabledReason && !banner ? disabledReason : undefined;
  const placeholder = disabled
    ? reasonLine ? "" : disabledReason ?? "This session is closed"
    : runActive
      ? canSteer
        ? `Enter queues a follow-up · ${steerKeyLabel} steers the current run…`
        : "Enter queues a follow-up…"
      : "Message the agent…";
  const sendLabel = runActive ? "Queue message" : "Send message";
  const sendTitle = voice.phase === "recording"
    ? "Enter stops the recording; review the transcript before sending"
    : pendingSubmit
      ? "Sends when the uploads finish"
      : runActive
        ? canSteer ? `Queue as the next run (↵) · steer with ${steerKeyLabel}` : "Queue as the next run (↵)"
        : "Send (↵)";
  const notices = [
    error ? { key: "error", tone: "error" as const, text: error, dismiss: onDismissError } : null,
    notice ? { key: "notice", tone: "warn" as const, text: notice, dismiss: () => setNotice(undefined) } : null,
    files.notice ? { key: "files", tone: "warn" as const, text: files.notice, dismiss: files.dismissNotice } : null,
  ].filter((value) => value !== null);
  const showToolbar = !disabled || (runActive && canStop);

  return (
    <div className="shrink-0 pt-1 pb-3 md:pb-4">
      <div className="mx-auto w-full max-w-5xl px-4 md:px-8">
        {banner}
        <div
          {...dropHandlers}
          className={cn(
            "relative flex flex-col rounded-2xl border bg-background shadow-xs transition-[border-color,box-shadow] duration-300 dark:bg-input/30",
            !disabled && "focus-within:border-ring/70 focus-within:ring-3 focus-within:ring-ring/20",
            disabled && "bg-muted/40 shadow-none dark:bg-input/10",
            flash && "border-ring ring-3 ring-ring/45",
          )}
        >
          {notices.map((item) => (
            <div key={item.key} role={item.tone === "error" ? "alert" : "status"}
              className={cn(
                "mx-2 mt-2 flex items-start gap-2 rounded-lg py-1.5 pr-1 pl-2.5 text-xs",
                item.tone === "error" ? "bg-destructive/10 text-destructive" : "bg-amber-500/10 text-amber-800 dark:text-amber-300",
              )}>
              <TriangleAlert className="mt-px size-3.5 shrink-0" aria-hidden />
              <span className="min-w-0 flex-1 py-px wrap-break-word">{item.text}</span>
              {item.dismiss && (
                <Button type="button" variant="ghost" size="icon-xs" aria-label="Dismiss" className="text-current hover:bg-foreground/10 hover:text-current"
                  onClick={item.dismiss}>
                  <X />
                </Button>
              )}
            </div>
          ))}
          <AttachmentStrip items={files.items} disabled={disabled} onRemove={files.remove} onRetry={files.retry} />
          <textarea
            ref={textarea}
            disabled={disabled}
            value={text}
            onChange={(event) => updateText(event.target.value)}
            onKeyDown={onKeyDown}
            onBlur={(event) => {
              selection.current = { start: event.target.selectionStart, end: event.target.selectionEnd, text: event.target.value };
            }}
            onPaste={(event) => {
              if (!canAttach) return;
              const pasted = [...event.clipboardData.files];
              // Rich copies often carry both text and an image of it; the
              // text is what was meant.
              if (!pasted.length || event.clipboardData.getData("text/plain")) return;
              event.preventDefault();
              files.add(pasted);
            }}
            placeholder={placeholder}
            aria-label="Message"
            rows={1}
            className="field-sizing-content max-h-52 min-h-11 w-full resize-none bg-transparent px-3.5 pt-3 pb-1.5 text-base outline-none placeholder:text-muted-foreground disabled:cursor-not-allowed md:text-sm pointer-coarse:text-base"
          />
          {showToolbar && (
            <div className="flex min-w-0 items-center gap-1 px-2 pb-2">
              {canAttach && (
                <>
                  <input ref={fileInput} type="file" multiple accept={ATTACHMENT_ACCEPT} className="hidden" tabIndex={-1} aria-hidden
                    onChange={(event) => {
                      files.add([...(event.target.files ?? [])]);
                      event.target.value = "";
                      textarea.current?.focus();
                    }} />
                  <Button type="button" variant="ghost" size="icon-sm" aria-label="Attach files"
                    title={`Attach files. ${ATTACHMENT_SUMMARY}. You can also drop or paste them.`}
                    className="text-muted-foreground hover:text-foreground" onClick={() => fileInput.current?.click()}>
                    <Plus />
                  </Button>
                </>
              )}
              {modelChoice && model && !disabled && (
                <ModelPicker choice={modelChoice} stored={storedChoice} runActive={runActive}
                  canSaveDefault={model.canSaveDefault} onChange={changeChoice}
                  onSaveDefault={async () => {
                    await model.onSaveDefault(configWithRunChoice(
                      model.config && typeof model.config === "object" ? model.config as Record<string, unknown> : {},
                      modelChoice,
                    ));
                    changeChoice({});
                  }} />
              )}
              <div className="min-w-2 flex-1" />
              {dictation && !disabled && (
                <VoiceControl voice={voice} unavailableReason={dictation.disabledReason} settingsHref={dictation.settingsHref}
                  demo={isDemoDictation} onStart={startVoice} />
              )}
              {runActive && canStop && (
                <Button type="button" variant="outline" size="icon-sm" onClick={onStop} disabled={stopping}
                  aria-label={stopping ? "Stopping run" : "Stop run"}
                  title={stopping ? "Stopping the active run…" : "Stop the active run"}>
                  {stopping ? <LoaderCircle className="animate-spin" /> : <Square className="size-3 fill-current" />}
                </Button>
              )}
              {!disabled && (
                <div className="flex shrink-0">
                  <Button type="button" size="icon-sm" onClick={() => submit(false)}
                    disabled={!hasContent && voice.phase !== "recording"}
                    aria-label={sendLabel} title={sendTitle}
                    className={cn(runActive && "rounded-r-none")}>
                    {pendingSubmit ? <LoaderCircle className="animate-spin" /> : <ArrowUp />}
                  </Button>
                  {runActive && (
                    <DropdownMenu>
                      <DropdownMenuTrigger render={
                        <Button type="button" size="icon-sm" aria-label="Queue or steer" disabled={!hasContent}
                          className="w-5 rounded-l-none border-l border-l-primary-foreground/20" />
                      }>
                        <ChevronDown className="size-3" />
                      </DropdownMenuTrigger>
                      <DropdownMenuContent side="top" align="end" className="w-72">
                        <DropdownMenuItem disabled={!hasContent} onClick={() => submit(false)}>
                          <ListPlus />
                          <span>Queue as the next run</span>
                          <DropdownMenuShortcut>↵</DropdownMenuShortcut>
                        </DropdownMenuItem>
                        <DropdownMenuItem disabled={!hasContent || !canSteer} onClick={() => submit(true)}>
                          <CornerDownRight />
                          <span className="flex flex-col">
                            <span>Steer the current run</span>
                            <span className="text-xs text-muted-foreground">
                              {canSteer ? "Read at its next turn, without interrupting it" : "Nothing can be steered right now"}
                            </span>
                          </span>
                          <DropdownMenuShortcut>{steerKeyLabel}</DropdownMenuShortcut>
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  )}
                </div>
              )}
            </div>
          )}
          {reasonLine && (
            <p className="flex items-center gap-1.5 px-3.5 pb-2.5 text-xs text-muted-foreground">
              <Lock className="size-3 shrink-0" aria-hidden />
              <span>{reasonLine}</span>
            </p>
          )}
          {dragging && (
            <div className="pointer-events-none absolute inset-0 flex items-center justify-center rounded-[inherit] border-2 border-dashed border-ring bg-background/85 text-sm font-medium text-foreground backdrop-blur-sm">
              Drop images, PDFs, or text files
            </div>
          )}
          <span role="status" aria-live="polite" className="sr-only">
            {voice.phase === "recording" ? "Recording. Enter stops and transcribes; Escape discards."
              : voice.phase === "requesting" ? "Waiting for microphone permission…"
              : voice.phase === "transcribing" ? "Transcribing… You can keep editing."
              : announcement ?? ""}
          </span>
        </div>
      </div>
    </div>
  );
}

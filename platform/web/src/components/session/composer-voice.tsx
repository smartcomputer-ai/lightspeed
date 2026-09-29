import { useEffect, useRef } from "react";
import { Link } from "react-router-dom";
import { LoaderCircle, Mic, MicOff, RotateCw, Square, X } from "lucide-react";
import { MAX_DICTATION_SECONDS } from "@lightspeed/platform-shared";
import type { useDictation } from "@/lib/use-dictation";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";

type Voice = ReturnType<typeof useDictation>;

const clock = (seconds: number) => `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
/// The timer turns amber in the last half minute before the automatic stop.
const WARN_AT = MAX_DICTATION_SECONDS - 30;
const BARS = [0.55, 0.85, 1, 0.75, 0.5];

/// Dictation lives in one control. Idle, it is a microphone icon; while
/// recording, transcribing, or failed, the icon grows into a pill that
/// carries that state and its actions. Nothing renders outside it.
export function VoiceControl({ voice, unavailableReason, settingsHref, demo, onStart }: {
  voice: Voice;
  /// Why dictation cannot start (no speech default, insecure origin, …).
  unavailableReason?: string;
  settingsHref?: string;
  demo?: boolean;
  onStart: () => void;
}) {
  if (voice.phase === "requesting") {
    return (
      <Pill tone="muted" label="Waiting for microphone permission">
        <Mic className="size-3.5 animate-pulse" aria-hidden />
        <span className="hidden sm:inline">Allow the microphone…</span>
        <PillButton label="Cancel dictation" onClick={voice.cancel}><X /></PillButton>
      </Pill>
    );
  }
  if (voice.phase === "recording") {
    const warn = voice.seconds >= WARN_AT;
    return (
      <Pill tone="recording" label={`${demo ? "Demo recording" : "Recording"} ${clock(voice.seconds)}`}>
        <span className="size-2 shrink-0 animate-pulse rounded-full bg-destructive" aria-hidden />
        <span className={cn("tabular-nums", warn && "text-amber-600 dark:text-amber-400")}
          title={warn ? `Recording stops at ${clock(MAX_DICTATION_SECONDS)}` : undefined}>
          {clock(voice.seconds)}
        </span>
        <LevelMeter level={voice.level} />
        <PillButton label="Cancel dictation" title="Discard recording (Esc)" onClick={voice.cancel}><X /></PillButton>
        <PillButton label="Stop recording" title="Stop and transcribe (Enter)" onClick={() => void voice.stop()}>
          <Square className="fill-current" />
        </PillButton>
      </Pill>
    );
  }
  if (voice.phase === "transcribing") {
    return (
      <Pill tone="muted" label="Transcribing">
        <LoaderCircle className="size-3.5 animate-spin" aria-hidden />
        <span>Transcribing…</span>
        <PillButton label="Cancel dictation" title="Cancel transcription (Esc)" onClick={voice.cancel}><X /></PillButton>
      </Pill>
    );
  }
  if (voice.phase === "error") {
    return (
      <Pill tone="error" label="Dictation failed">
        <MicOff className="size-3.5 shrink-0" aria-hidden />
        <span role="alert" className="max-w-28 truncate sm:max-w-64" title={voice.error}>{voice.error ?? "Dictation failed."}</span>
        {voice.canRetry
          ? <PillButton label="Retry transcription" title="Retry transcription" onClick={voice.retry}><RotateCw /></PillButton>
          : <PillButton label="Dictate message" title="Try again" onClick={onStart}><RotateCw /></PillButton>}
        <PillButton label="Cancel dictation" title="Dismiss" onClick={voice.cancel}><X /></PillButton>
      </Pill>
    );
  }
  if (unavailableReason) {
    return (
      <Popover>
        <PopoverTrigger render={
          <Button type="button" variant="ghost" size="icon-sm" aria-label="Dictate message" aria-disabled="true"
            className="text-muted-foreground/50 hover:text-muted-foreground" />
        }>
          <Mic />
        </PopoverTrigger>
        <PopoverContent side="top" align="end" className="w-72 p-3 text-sm">
          <p className="font-medium">Dictation is unavailable</p>
          <p className="mt-1 text-xs text-muted-foreground">{unavailableReason}</p>
          {settingsHref && (
            <Link to={settingsHref} className="mt-2 inline-block text-xs font-medium underline underline-offset-2">Open Models</Link>
          )}
        </PopoverContent>
      </Popover>
    );
  }
  return (
    <Button type="button" variant="ghost" size="icon-sm" aria-label="Dictate message"
      title={demo ? "Dictate (demo inserts a sample transcript)" : "Dictate. Enter stops, Esc discards; you review the text before sending."}
      className="text-muted-foreground hover:text-foreground" onClick={onStart}>
      <Mic />
    </Button>
  );
}

function Pill({ tone, label, children }: { tone: "muted" | "recording" | "error"; label: string; children: React.ReactNode }) {
  return (
    <div role="group" aria-label={label}
      className={cn(
        "flex h-8 shrink-0 items-center gap-1.5 rounded-lg pr-0.5 pl-2 sm:gap-2 sm:pl-2.5 text-xs font-medium animate-in fade-in zoom-in-95 duration-150",
        tone === "muted" && "bg-muted text-muted-foreground",
        tone === "recording" && "bg-destructive/10 text-destructive ring-1 ring-destructive/25 ring-inset",
        tone === "error" && "bg-destructive/10 text-destructive",
      )}>
      {children}
    </div>
  );
}

function PillButton({ label, title, onClick, children }: { label: string; title?: string; onClick: () => void; children: React.ReactNode }) {
  return (
    <Button type="button" variant="ghost" size="icon-xs" aria-label={label} title={title ?? label}
      className="text-current hover:bg-foreground/10 hover:text-current" onClick={onClick}>
      {children}
    </Button>
  );
}

/// Five bars that follow the input level. Updated per animation frame
/// through refs, so a recording does not re-render the composer.
function LevelMeter({ level }: { level: () => number }) {
  const bars = useRef<Array<HTMLSpanElement | null>>([]);
  useEffect(() => {
    if (typeof requestAnimationFrame !== "function") return;
    let frame = 0;
    let smoothed = 0;
    const tick = () => {
      smoothed = smoothed * 0.6 + level() * 0.4;
      bars.current.forEach((bar, index) => {
        if (bar) bar.style.transform = `scaleY(${Math.max(0.18, Math.min(1, smoothed * BARS[index]! * 1.6))})`;
      });
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [level]);
  return (
    <span className="flex h-3.5 items-center gap-0.5" aria-hidden>
      {BARS.map((_, index) => (
        <span key={index} ref={(node) => { bars.current[index] = node; }} style={{ transform: "scaleY(0.18)" }}
          className="h-full w-0.5 origin-center rounded-full bg-current transition-transform duration-75" />
      ))}
    </span>
  );
}

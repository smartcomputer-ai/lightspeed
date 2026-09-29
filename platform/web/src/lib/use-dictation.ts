import { useCallback, useEffect, useRef, useState } from "react";
import { MAX_DICTATION_SECONDS } from "@lightspeed/platform-shared";
import { useModelDefaults, useModelDiscovery } from "./model-defaults";
import { summarizeProviderReadiness } from "./provider-readiness";
import { audioCaptureUnavailableReason, startAudioCapture, type AudioCapture } from "./audio-capture";
import { cancelRecording, transcribeRecording, type DictationRecording } from "./dictation";

export function useDictationAvailability(universeId: string) {
  const defaults = useModelDefaults(universeId);
  const discovery = useModelDiscovery(universeId);
  const model = defaults.data?.speechToText;
  let disabledReason: string | undefined;
  if (defaults.isLoading) disabledReason = "Checking dictation settings…";
  else if (defaults.error) disabledReason = "Dictation settings are unavailable. Reload to try again.";
  else if (!model) disabledReason = "Set a speech-to-text default in Models to enable dictation.";
  else {
    const readiness = summarizeProviderReadiness(model, discovery.error ? undefined : discovery.data?.providers, "speechToText");
    if (readiness.blocked) disabledReason = readiness.message;
    else disabledReason = audioCaptureUnavailableReason();
  }
  return { universeId, disabledReason };
}

type Phase = "idle" | "requesting" | "recording" | "transcribing" | "error";
interface Attempt { universeId: string; stopping?: boolean; controller: AbortController; capture?: AudioCapture; timer?: ReturnType<typeof setInterval>; }

export function useDictation(universeId: string | undefined, enabled: boolean, onTranscript: (text: string) => void) {
  const [phase, setPhase] = useState<Phase>("idle");
  const [error, setError] = useState<string>();
  const [seconds, setSeconds] = useState(0);
  const active = useRef<Attempt | null>(null);
  const recording = useRef<DictationRecording | null>(null);
  const latest = useRef({ enabled, onTranscript });
  latest.current = { enabled, onTranscript };
  const dispose = useCallback(() => {
    const attempt = active.current;
    active.current = null;
    if (attempt && recording.current) cancelRecording(attempt.universeId, recording.current);
    recording.current = null;
    if (attempt) { clearInterval(attempt.timer); attempt.controller.abort(); attempt.capture?.stop(); }
  }, []);
  const cancel = useCallback(() => { dispose(); setPhase("idle"); setError(undefined); }, [dispose]);
  useEffect(() => {
    const hide = () => cancel();
    window.addEventListener("pagehide", hide);
    return () => { window.removeEventListener("pagehide", hide); dispose(); };
  }, [universeId, cancel, dispose]);
  useEffect(() => { if (!enabled) cancel(); }, [enabled, cancel]);

  const valid = (attempt: Attempt) => active.current === attempt && !attempt.controller.signal.aborted && latest.current.enabled;
  const fail = (attempt: Attempt, cause: unknown) => {
    if (!valid(attempt)) return;
    clearInterval(attempt.timer);
    setPhase("error");
    setError(cause instanceof DOMException && cause.name === "NotAllowedError"
      ? "Microphone access was denied. Allow access in your browser and try again."
      : cause instanceof Error ? cause.message : "Dictation failed. Try again.");
  };
  const transcribe = async (attempt: Attempt) => {
    if (!universeId || !recording.current || !valid(attempt)) return;
    setPhase("transcribing");
    setError(undefined);
    try {
      const text = await transcribeRecording(universeId, recording.current, attempt.controller.signal);
      if (!valid(attempt)) return;
      latest.current.onTranscript(text);
      recording.current = null;
      active.current = null;
      setPhase("idle");
    } catch (cause) { fail(attempt, cause); }
  };
  const stop = async () => {
    const attempt = active.current;
    if (!attempt?.capture || attempt.stopping || !valid(attempt)) return;
    attempt.stopping = true;
    clearInterval(attempt.timer);
    setPhase("transcribing");
    try {
      attempt.capture.stop();
      const blob = await attempt.capture.result;
      if (!valid(attempt)) return;
      recording.current = { blob, name: attempt.capture.name, idempotencyKey: crypto.randomUUID() };
      await transcribe(attempt);
    } catch (cause) { fail(attempt, cause); }
  };
  const start = async () => {
    if (!enabled || !universeId) return;
    dispose();
    const attempt: Attempt = { universeId, controller: new AbortController() };
    active.current = attempt;
    setError(undefined);
    setSeconds(0);
    setPhase("requesting");
    try {
      attempt.capture = await startAudioCapture(attempt.controller.signal, (cause) => fail(attempt, cause));
      if (!valid(attempt)) { attempt.capture.stop(); return; }
      setPhase("recording");
      const started = Date.now();
      attempt.timer = setInterval(() => {
        const elapsed = Math.floor((Date.now() - started) / 1000);
        setSeconds(elapsed);
        if (elapsed >= MAX_DICTATION_SECONDS) void stop();
      }, 250);
    } catch (cause) { fail(attempt, cause); }
  };
  const level = useCallback(() => active.current?.capture?.level?.() ?? 0, []);
  return { phase, error, seconds, start, stop, cancel, level,
    retry: () => { if (active.current) void transcribe(active.current); },
    canRetry: phase === "error" && recording.current !== null,
  };
}

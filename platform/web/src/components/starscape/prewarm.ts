import { Starscape as StarscapeRenderer, type StarscapeFade } from "./lib/starscape.js";
import { SITE_CONFIG, starscapeWindow } from "./geometry";

/// The large starscape behind a creating dialog (DialogContent `starscape`):
/// its settings, a ready-drawn copy kept for it, and its say over the other
/// starscapes on the page.
///
/// Its first frame is expensive (a fractal nebula, the star field and
/// full-size cached layers, a few hundred milliseconds), so one is drawn
/// ahead: after the page has loaded, a short delay and an idle moment, at
/// the window's size, and drawn again when the window settles at another.
/// A dialog takes it when it opens and gives it back when it closes; while
/// it is out (or not ready yet) a dialog draws its own. It is dropped after
/// the tab has been hidden a while. (With reduced motion it is kept too:
/// the dialog shows it still.)
///
/// While a dialog's starscape runs, every other starscape holds still (its
/// frame stays), and goes on once the dialog's is gone.

/// How the dialog's starscape is drawn: over the backdrop's tint, faded out
/// toward the edges around the dialog.
export const DIALOG_STARSCAPE = {
  fade: { start: 160, end: 760 } satisfies StarscapeFade,
  transparentBackground: true,
} as const;

/// After the page has loaded: how long to wait before drawing it, and how
/// long an idle moment may take to come.
export const PREWARM_DELAY_MS = 2_000;
const IDLE_TIMEOUT_MS = 3_000;
/// How long the window must keep a size before it is drawn at it.
export const PREWARM_RESIZE_MS = 500;
/// How long the tab may stay hidden before it is dropped.
export const PREWARM_HIDDEN_DROP_MS = 5 * 60_000;

/// A drawn dialog starscape, at the window size in `key`.
export interface WarmStarscape {
  canvas: HTMLCanvasElement;
  renderer: StarscapeRenderer;
  key: string;
}

let warm: WarmStarscape | null = null;
let claimed = false;

/// The window's size and density, which a drawn starscape is for.
export function windowKey(): string {
  return `${window.innerWidth}x${window.innerHeight}@${window.devicePixelRatio >= 1.5 ? 2 : 1}`;
}

/// Lays a starscape out as the dialog's backdrop does (the window, its
/// vanishing point in the middle) and draws its frame.
function draw(entry: WarmStarscape) {
  const width = window.innerWidth;
  const height = window.innerHeight;
  const ratio = window.devicePixelRatio >= 1.5 ? 2 : 1;
  const { fade } = DIALOG_STARSCAPE;
  const area = starscapeWindow({ width, height, cx: width / 2, cy: height / 2, fade, scale: 1, ratio, config: entry.renderer.config });
  entry.key = windowKey();
  if (!area) return;
  Object.assign(entry.canvas.style, { left: `${area.left}px`, top: `${area.top}px`, width: `${area.width}px`, height: `${area.height}px` });
  entry.renderer
    .setViewport(area.viewport)
    .setFade({ start: fade.start * ratio, end: fade.end * ratio })
    .render();
}

/// Draws it, or again at a new window size; nothing while a dialog has it.
export function warmDialogStarscape() {
  if (claimed || typeof document === "undefined" || typeof CanvasRenderingContext2D === "undefined") return;
  if (warm) {
    if (warm.key !== windowKey()) draw(warm);
    return;
  }
  const canvas = document.createElement("canvas");
  canvas.className = "absolute";
  canvas.dataset.prewarmed = "";
  try {
    const renderer = new StarscapeRenderer(
      canvas,
      { ...SITE_CONFIG, seed: Math.floor(Math.random() * 1_000_000) },
      { alpha: true, transparentBackground: DIALOG_STARSCAPE.transparentBackground },
    );
    warm = { canvas, renderer, key: "" };
    draw(warm);
  } catch {
    warm = null;
  }
}

/// Lets it go, to be drawn again when wanted.
export function dropDialogStarscape() {
  if (!claimed) warm = null;
}

/// The drawn starscape for a dialog that opens now, at the window's size;
/// null when it is not ready or already taken (the dialog draws its own).
export function claimDialogStarscape(): WarmStarscape | null {
  if (!warm || claimed) return null;
  claimed = true;
  return warm;
}

/// Gives it back once its dialog is gone, still drawn; a size change while
/// it was out is drawn at the next idle moment.
export function releaseDialogStarscape(entry: WarmStarscape) {
  if (entry !== warm) return;
  claimed = false;
  if (entry.key !== windowKey()) whenIdle(warmDialogStarscape);
}

/// Runs `task` when the browser is idle (or soon, without idle callbacks).
function whenIdle(task: () => void): () => void {
  if (typeof window.requestIdleCallback === "function") {
    const id = window.requestIdleCallback(task, { timeout: IDLE_TIMEOUT_MS });
    return () => window.cancelIdleCallback(id);
  }
  const id = window.setTimeout(task, 0);
  return () => window.clearTimeout(id);
}

/// Keeps a dialog starscape ready while it runs: drawn once the page has
/// loaded (and a delay, and an idle moment), again when the window settles
/// at another size, dropped while the tab stays hidden and drawn again on
/// the way back. Returns the stop, which also drops it.
export function keepDialogStarscapeWarm(): () => void {
  if (typeof window === "undefined" || typeof CanvasRenderingContext2D === "undefined") return () => {};
  const cancels = new Set<() => void>();
  const later = (ms: number, task: () => void) => {
    const id = window.setTimeout(() => {
      cancels.delete(cancel);
      cancels.add(whenIdle(task));
    }, ms);
    const cancel = () => window.clearTimeout(id);
    cancels.add(cancel);
  };
  let hiddenTimer = 0;
  const onLoad = () => later(PREWARM_DELAY_MS, warmDialogStarscape);
  let resizeTimer = 0;
  const onResize = () => {
    window.clearTimeout(resizeTimer);
    // Only one that is kept is drawn again; none is drawn for a resize.
    resizeTimer = window.setTimeout(() => { if (warm) cancels.add(whenIdle(warmDialogStarscape)); }, PREWARM_RESIZE_MS);
  };
  const onVisibility = () => {
    window.clearTimeout(hiddenTimer);
    if (document.hidden) {
      hiddenTimer = window.setTimeout(dropDialogStarscape, PREWARM_HIDDEN_DROP_MS);
    } else if (!warm) {
      later(PREWARM_DELAY_MS, warmDialogStarscape);
    }
  };
  if (document.readyState === "complete") onLoad();
  else window.addEventListener("load", onLoad, { once: true });
  window.addEventListener("resize", onResize);
  document.addEventListener("visibilitychange", onVisibility);
  return () => {
    window.removeEventListener("load", onLoad);
    window.removeEventListener("resize", onResize);
    document.removeEventListener("visibilitychange", onVisibility);
    window.clearTimeout(resizeTimer);
    window.clearTimeout(hiddenTimer);
    for (const cancel of cancels) cancel();
    dropDialogStarscape();
  };
}

// While a dialog's starscape runs, the others hold still.

let dialogsRunning = 0;
const pauseListeners = new Set<() => void>();

/// Whether a dialog's starscape is running, so the others hold still.
export function dialogStarscapeRunning(): boolean {
  return dialogsRunning > 0;
}

/// Calls `listener` when a dialog's starscape starts or stops; returns the
/// unsubscribe.
export function onDialogStarscapeChange(listener: () => void): () => void {
  pauseListeners.add(listener);
  return () => pauseListeners.delete(listener);
}

/// Marks a dialog's starscape running until the returned stop is called.
export function runDialogStarscape(): () => void {
  dialogsRunning += 1;
  pauseListeners.forEach((listener) => listener());
  let stopped = false;
  return () => {
    if (stopped) return;
    stopped = true;
    dialogsRunning -= 1;
    pauseListeners.forEach((listener) => listener());
  };
}

/// Forgets the kept starscape and the running count (tests).
export function resetDialogStarscapeForTests() {
  warm = null;
  claimed = false;
  dialogsRunning = 0;
  pauseListeners.clear();
}

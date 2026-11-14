import { useEffect, useRef, useState, type RefObject } from "react";
import { cn } from "@/lib/utils";
import { useReducedMotion } from "@/lib/reduced-motion";
import { Starscape as StarscapeRenderer, type StarscapeConfig, type StarscapeFade } from "./lib/starscape.js";

/// The frame the Lightspeed site renders its hero starscape into, in frame
/// units. The component shows a window onto a frame of this shape, so a
/// small element shows the same sky the site does, not a squeezed copy.
export const SITE_FRAME = { width: 887, height: 1252 } as const;

/// The site's settings on top of the renderer defaults: 30 frames a second
/// and a fresh seed per page load.
export const SITE_CONFIG: Partial<StarscapeConfig> = { fps: 30 };

/// What a host of `width` by `height` CSS px with its vanishing point at
/// (`cx`, `cy`) needs drawn: the canvas rectangle in CSS px (the host, cut
/// down to the fade's outer radius) and the viewport onto the site's frame
/// in device px, or null when nothing is visible.
export function starscapeWindow({ width, height, cx, cy, fade, scale, ratio, config }: {
  width: number;
  height: number;
  cx: number;
  cy: number;
  fade?: StarscapeFade;
  scale: number;
  ratio: number;
  config: Pick<StarscapeConfig, "centerX" | "centerY">;
}) {
  const left = Math.max(0, Math.floor(fade ? cx - fade.end : 0));
  const top = Math.max(0, Math.floor(fade ? cy - fade.end : 0));
  const right = Math.min(width, Math.ceil(fade ? cx + fade.end : width));
  const bottom = Math.min(height, Math.ceil(fade ? cy + fade.end : height));
  if (right <= left || bottom <= top) return null;
  const frameWidth = SITE_FRAME.width * scale * ratio;
  const frameHeight = SITE_FRAME.height * scale * ratio;
  return {
    left,
    top,
    width: right - left,
    height: bottom - top,
    viewport: {
      frameWidth,
      frameHeight,
      // Place the frame's vanishing point on (cx, cy).
      x: frameWidth * config.centerX - (cx - left) * ratio,
      y: frameHeight * config.centerY - (cy - top) * ratio,
      width: (right - left) * ratio,
      height: (bottom - top) * ratio,
    },
  };
}

export interface StarscapeProps {
  className?: string;
  /// Centres the vanishing point on this element; it wins over `center`.
  anchor?: RefObject<HTMLElement | null>;
  /// Vanishing point in CSS px from the host's top-left corner. Defaults to
  /// the host's centre.
  center?: { x: number; y: number };
  /// Fades the starscape out with distance from the vanishing point, in CSS
  /// px: fully visible within `start`, gone beyond `end`. Only the part
  /// within `end` is rendered. `inner` also clears the centre, e.g. behind
  /// an icon: gone within `inner.start`, fully visible again by `inner.end`.
  fade?: StarscapeFade;
  /// CSS px per frame unit: how large the sky is drawn. 1 draws the site's
  /// frame at 887 by 1252 CSS px.
  scale?: number;
  /// Renderer settings over the site's.
  config?: Partial<StarscapeConfig>;
  /// Draw the stars, streaks and nebula with no black sky behind them, so
  /// they sit directly on whatever is behind the host.
  transparentBackground?: boolean;
  /// Animate the warp. Off, a single frame is drawn. (With reduced motion
  /// requested there is no starscape at all.)
  animate?: boolean;
  /// Go to warp speed: the stars rush past faster with longer, brighter
  /// trails, ramping up over WARP_RAMP_MS (and back down when it ends).
  warp?: boolean;
}

/// How long the starscape takes to reach warp speed.
export const WARP_RAMP_MS = 400;
/// How long it takes to slow back down.
const WARP_SETTLE_MS = 400;
/// The settings at full warp (each its control's maximum), which the warp
/// ramps to from the sky's own.
const WARP_CONFIG = { flowSpeed: 4, streakSpeed: 4, streakMaxLength: 1.5, streakBrightness: 2 } as const;
type WarpKey = keyof typeof WARP_CONFIG;

/// A realtime starscape filling its host element, which should be
/// positioned (e.g. `absolute inset-0`). It renders only the visible part
/// of the host, at 1x or 2x depending on the screen's pixel density, and
/// pauses while off screen or in a hidden tab.
/// With reduced motion requested, nothing at all: not even a still sky.
export function Starscape(props: StarscapeProps) {
  return useReducedMotion() ? null : <StarscapeCanvas {...props} />;
}

function StarscapeCanvas({
  className,
  anchor,
  center,
  fade,
  scale = 1,
  config,
  transparentBackground = false,
  animate = true,
  warp = false,
}: StarscapeProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  // Math.random rather than crypto, so seeded test runs draw the same sky.
  const [seed] = useState(() => Math.floor(Math.random() * 1_000_000));
  const configKey = JSON.stringify(config ?? {});
  // The renderer, the sky's own warp settings, and how far into warp it is
  // (0 to 1), for the warp to ramp between.
  const renderer = useRef<StarscapeRenderer | null>(null);
  const cruise = useRef<Record<WarpKey, number> | null>(null);
  const warpLevel = useRef(0);

  useEffect(() => {
    const host = hostRef.current;
    const canvas = canvasRef.current;
    // No 2D canvas (jsdom): leave the layer empty.
    if (!host || !canvas || typeof CanvasRenderingContext2D === "undefined") return;

    const starscape = new StarscapeRenderer(
      canvas,
      { ...SITE_CONFIG, seed, ...(JSON.parse(configKey) as Partial<StarscapeConfig>) },
      { alpha: true, transparentBackground },
    );
    renderer.current = starscape;
    cruise.current = Object.fromEntries(
      Object.keys(WARP_CONFIG).map((key) => [key, starscape.config[key as WarpKey]]),
    ) as Record<WarpKey, number>;
    warpLevel.current = 0;
    let visible = true;
    let animationFrame = 0;
    let lastFrame = 0;

    const layout = () => {
      const width = host.clientWidth;
      const height = host.clientHeight;
      const hostBox = host.getBoundingClientRect();
      const anchorBox = anchor?.current?.getBoundingClientRect();
      // The boxes are on screen, the canvas in the host's own pixels: under
      // a scale transform the two differ by the scale.
      const zoom = width > 0 && hostBox.width > 0 ? hostBox.width / width : 1;
      const cx = anchorBox ? (anchorBox.left + anchorBox.width / 2 - hostBox.left) / zoom : (center?.x ?? width / 2);
      const cy = anchorBox ? (anchorBox.top + anchorBox.height / 2 - hostBox.top) / zoom : (center?.y ?? height / 2);

      const ratio = window.devicePixelRatio >= 1.5 ? 2 : 1;
      const area = starscapeWindow({ width, height, cx, cy, fade, scale, ratio, config: starscape.config });
      canvas.hidden = area === null;
      if (!area) return false;
      Object.assign(canvas.style, {
        left: `${area.left}px`,
        top: `${area.top}px`,
        width: `${area.width}px`,
        height: `${area.height}px`,
      });
      starscape
        .setViewport(area.viewport)
        .setFade(fade
          ? {
              start: fade.start * ratio,
              end: fade.end * ratio,
              inner: fade.inner && { start: fade.inner.start * ratio, end: fade.inner.end * ratio },
            }
          : null)
        .render();
      return true;
    };

    const moving = () => animate && visible && !document.hidden && !canvas.hidden;
    const tick = (now: number) => {
      animationFrame = 0;
      if (!moving()) return;
      const duration = 1000 / starscape.config.fps;
      if (lastFrame === 0) lastFrame = now;
      const elapsed = now - lastFrame;
      if (elapsed >= duration) {
        lastFrame = now - (elapsed % duration);
        starscape.advance(Math.min(4, Math.floor(elapsed / duration))).render();
      }
      animationFrame = requestAnimationFrame(tick);
    };
    const sync = () => {
      if (!moving()) {
        cancelAnimationFrame(animationFrame);
        animationFrame = 0;
      } else if (animationFrame === 0) {
        lastFrame = 0;
        animationFrame = requestAnimationFrame(tick);
      }
    };
    const relayout = () => {
      layout();
      sync();
    };

    const resizeObserver = new ResizeObserver(relayout);
    resizeObserver.observe(host);
    if (anchor?.current) resizeObserver.observe(anchor.current);
    const intersectionObserver = new IntersectionObserver(([entry]) => {
      visible = entry?.isIntersecting ?? true;
      sync();
    });
    intersectionObserver.observe(host);
    // Moving between screens of different density changes the ratio.
    const density = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
    density.addEventListener("change", relayout);
    document.addEventListener("visibilitychange", sync);
    relayout();

    return () => {
      cancelAnimationFrame(animationFrame);
      resizeObserver.disconnect();
      intersectionObserver.disconnect();
      density.removeEventListener("change", relayout);
      document.removeEventListener("visibilitychange", sync);
    };
  }, [anchor, center?.x, center?.y, fade?.start, fade?.end, fade?.inner?.start, fade?.inner?.end, scale, transparentBackground, animate, seed, configKey]);

  // Ramp the live settings into warp (easing out, so the rush starts at
  // once) or back down; the running animation picks them up.
  useEffect(() => {
    const starscape = renderer.current;
    const base = cruise.current;
    if (!starscape || !base) return;
    const from = warpLevel.current;
    const to = warp ? 1 : 0;
    if (from === to) return;
    const duration = (warp ? WARP_RAMP_MS : WARP_SETTLE_MS) * Math.abs(to - from);
    const started = performance.now();
    let frame = 0;
    const step = (now: number) => {
      const t = Math.min(1, (now - started) / Math.max(1, duration));
      const eased = 1 - (1 - t) ** (warp ? 2 : 3);
      const level = from + (to - from) * eased;
      warpLevel.current = level;
      starscape.update(
        Object.fromEntries(
          (Object.keys(WARP_CONFIG) as WarpKey[]).map((key) => [key, base[key] + (WARP_CONFIG[key] - base[key]) * level]),
        ),
      );
      if (t < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  }, [warp]);

  return (
    <div ref={hostRef} aria-hidden="true" data-starscape="" className={cn("pointer-events-none overflow-hidden", className)}>
      <canvas ref={canvasRef} className="absolute" />
    </div>
  );
}

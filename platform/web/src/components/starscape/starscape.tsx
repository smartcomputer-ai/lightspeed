import { useEffect, useRef, useState, type RefObject } from "react";
import { cn } from "@/lib/utils";
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
  /// Animate the warp. Off, or with reduced motion requested, a single frame
  /// is drawn.
  animate?: boolean;
}

/// A realtime starscape filling its host element, which should be
/// positioned (e.g. `absolute inset-0`). It renders only the visible part
/// of the host, at 1x or 2x depending on the screen's pixel density, and
/// pauses while off screen or in a hidden tab.
export function Starscape({
  className,
  anchor,
  center,
  fade,
  scale = 1,
  config,
  transparentBackground = false,
  animate = true,
}: StarscapeProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  // Math.random rather than crypto, so seeded test runs draw the same sky.
  const [seed] = useState(() => Math.floor(Math.random() * 1_000_000));
  const configKey = JSON.stringify(config ?? {});

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
    const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
    let visible = true;
    let animationFrame = 0;
    let lastFrame = 0;

    const layout = () => {
      const width = host.clientWidth;
      const height = host.clientHeight;
      const hostBox = host.getBoundingClientRect();
      const anchorBox = anchor?.current?.getBoundingClientRect();
      const cx = anchorBox ? anchorBox.left + anchorBox.width / 2 - hostBox.left : (center?.x ?? width / 2);
      const cy = anchorBox ? anchorBox.top + anchorBox.height / 2 - hostBox.top : (center?.y ?? height / 2);

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

    const moving = () => animate && !reducedMotion.matches && visible && !document.hidden && !canvas.hidden;
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
    reducedMotion.addEventListener("change", sync);
    document.addEventListener("visibilitychange", sync);
    relayout();

    return () => {
      cancelAnimationFrame(animationFrame);
      resizeObserver.disconnect();
      intersectionObserver.disconnect();
      density.removeEventListener("change", relayout);
      reducedMotion.removeEventListener("change", sync);
      document.removeEventListener("visibilitychange", sync);
    };
  }, [anchor, center?.x, center?.y, fade?.start, fade?.end, fade?.inner?.start, fade?.inner?.end, scale, transparentBackground, animate, seed, configKey]);

  return (
    <div ref={hostRef} aria-hidden="true" data-starscape="" className={cn("pointer-events-none overflow-hidden", className)}>
      <canvas ref={canvasRef} className="absolute" />
    </div>
  );
}

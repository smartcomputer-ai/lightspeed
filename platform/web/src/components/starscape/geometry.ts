import type { StarscapeConfig, StarscapeFade } from "./lib/starscape.js";

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

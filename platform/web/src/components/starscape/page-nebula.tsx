import { useEffect, useState } from "react";
import { cn } from "@/lib/utils";
import { Starscape as StarscapeRenderer } from "./lib/starscape.js";

/// The pages' background: the starscape's nebula alone (no stars, streaks
/// or sky), drawn once per page load and kept in memory, then shown as a
/// still image behind the content. The same sky everywhere, for now.

/// Which nebula: one seed for the whole app.
export const PAGE_NEBULA_SEED = 972191;
/// The image's size: landscape, like the content areas; cover scales it.
const PAGE_NEBULA_WIDTH = 960;
const PAGE_NEBULA_HEIGHT = 600;

/// The app's nebula, kept once drawn. Other seeds (the UI overview's
/// Regenerate) are drawn each time and not kept.
let cached: string | null = null;

/// One seed's nebula as an image URL (null without a canvas, e.g. in tests).
function drawNebula(seed: number): string | null {
  if (typeof document === "undefined" || typeof CanvasRenderingContext2D === "undefined") return null;
  try {
    const canvas = document.createElement("canvas");
    new StarscapeRenderer(
      canvas,
      { seed, starCount: 0, streakCount: 0, bloomAmount: 0, grain: 0, vignette: 0 },
      { transparentBackground: true },
    )
      .resize(PAGE_NEBULA_WIDTH, PAGE_NEBULA_HEIGHT)
      .render();
    return canvas.toDataURL("image/png");
  } catch {
    return null;
  }
}

function pageNebula(seed: number): string | null {
  if (seed !== PAGE_NEBULA_SEED) return drawNebula(seed);
  cached ??= drawNebula(seed);
  return cached;
}

/// The nebula covering its (positioned) host, subtly: --page-nebula-opacity,
/// and drawn dark in the light theme. Drawn just after the first paint, so
/// it never holds the page up. `seed` picks another nebula (the app uses
/// its own); a new seed keeps the old image until the new one is drawn.
export function PageNebula({ className, seed = PAGE_NEBULA_SEED }: { className?: string; seed?: number }) {
  // `seed` is the one last drawn (or tried, without a canvas); null before any.
  const [drawn, setDrawn] = useState<{ seed: number | null; image: string | null }>(() =>
    seed === PAGE_NEBULA_SEED && cached ? { seed, image: cached } : { seed: null, image: null });
  useEffect(() => {
    if (drawn.seed === seed) return;
    const timer = window.setTimeout(() => setDrawn({ seed, image: pageNebula(seed) }), 0);
    return () => window.clearTimeout(timer);
  }, [drawn, seed]);
  const image = drawn.image;
  if (!image) return null;
  return (
    <div
      aria-hidden
      data-page-nebula=""
      className={cn("pointer-events-none bg-cover bg-center opacity-(--page-nebula-opacity) invert hue-rotate-180 dark:invert-0 dark:hue-rotate-0", className)}
      style={{ backgroundImage: `url(${image})` }}
    />
  );
}

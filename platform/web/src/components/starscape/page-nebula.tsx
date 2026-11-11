import { useEffect, useState } from "react";
import { cn } from "@/lib/utils";
import { Starscape as StarscapeRenderer } from "./lib/starscape.js";

/// The pages' background: the starscape's nebula alone (no stars, streaks
/// or sky), drawn once per page load and kept in memory, then shown as a
/// still image behind the content. The same sky everywhere, for now.

/// Which nebula: one seed for the whole app.
const PAGE_NEBULA_SEED = 972191;
/// The image's size: landscape, like the content areas; cover scales it.
const PAGE_NEBULA_WIDTH = 960;
const PAGE_NEBULA_HEIGHT = 600;

let cached: string | null = null;

/// The nebula as an image URL, drawn on first use (null without a canvas,
/// e.g. in tests).
function pageNebula(): string | null {
  if (cached) return cached;
  if (typeof document === "undefined" || typeof CanvasRenderingContext2D === "undefined") return null;
  try {
    const canvas = document.createElement("canvas");
    new StarscapeRenderer(
      canvas,
      { seed: PAGE_NEBULA_SEED, starCount: 0, streakCount: 0, bloomAmount: 0, grain: 0, vignette: 0 },
      { transparentBackground: true },
    )
      .resize(PAGE_NEBULA_WIDTH, PAGE_NEBULA_HEIGHT)
      .render();
    cached = canvas.toDataURL("image/png");
  } catch {
    return null;
  }
  return cached;
}

/// The nebula covering its (positioned) host, subtly: --page-nebula-opacity,
/// and drawn dark in the light theme. Drawn just after the first paint, so
/// it never holds the page up.
export function PageNebula({ className }: { className?: string }) {
  const [image, setImage] = useState(cached);
  useEffect(() => {
    if (image) return;
    const timer = window.setTimeout(() => setImage(pageNebula()), 0);
    return () => window.clearTimeout(timer);
  }, [image]);
  if (!image) return null;
  return (
    <div
      aria-hidden
      data-page-nebula=""
      className={cn("pointer-events-none bg-cover bg-center opacity-(--page-nebula-opacity) invert dark:invert-0", className)}
      style={{ backgroundImage: `url(${image})` }}
    />
  );
}

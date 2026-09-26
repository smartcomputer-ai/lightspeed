import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import { cn } from "@/lib/utils";

/** How far one arrow key moves a handle. */
const KEY_STEP = 16;

export function clampWidth(width: number, min: number, max: number): number {
  return Math.round(Math.min(Math.max(width, min), Math.max(min, max)));
}

/**
 * A column's width while it may be dragged: the stored width (or the
 * default) until a drag starts, then the live width until it ends and is
 * committed. `resizing` lets a column switch off its width transition.
 */
export function useResizableWidth({
  stored,
  fallback,
  min,
  max,
  room,
  onCommit,
}: {
  stored: number | null;
  fallback: number;
  min: number;
  max: number;
  /** The most the page can spare now, read when a drag starts. */
  room?: () => number;
  /** The new width, or null to go back to the default. */
  onCommit: (width: number | null) => void;
}) {
  const [live, setLive] = useState<number | null>(null);
  const width = live ?? clampWidth(stored ?? fallback, min, max);
  const limit = () => Math.min(max, room?.() ?? max);
  return {
    width,
    resizing: live !== null,
    handle: { width, min, max, limit, onLive: setLive, onCommit },
  };
}

/**
 * The drag strip on a column's right edge. It sits over the column's border
 * (the parent must be positioned), shows a line while hovered, focused or
 * dragged, and works by pointer and by arrow keys. Double-click resets.
 */
export function ResizeHandle({
  label,
  handle,
  className,
}: {
  label: string;
  handle: ReturnType<typeof useResizableWidth>["handle"];
  className?: string;
}) {
  const { width, min, max, limit, onLive, onCommit } = handle;
  const drag = useRef<{ startX: number; startWidth: number; max: number; width: number } | null>(null);
  // Whether the last drag moved: its two clicks are then no double-click.
  const moved = useRef(false);
  const [dragging, setDragging] = useState(false);
  const commit = useRef(onCommit);
  commit.current = onCommit;

  // A column leaving mid-drag (the page changed) keeps the width it showed.
  useEffect(() => () => {
    const current = drag.current;
    if (!current) return;
    drag.current = null;
    restoreBody();
    commit.current(current.width);
  }, []);

  // Every way a drag can stop ends it once: release, cancel, or the browser
  // taking the pointer away without either.
  const end = (event: PointerEvent<HTMLDivElement>) => {
    const current = drag.current;
    if (!current) return;
    drag.current = null;
    setDragging(false);
    if (event.currentTarget.hasPointerCapture?.(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    restoreBody();
    onLive(null);
    onCommit(current.width);
  };

  const key = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = event.key === "ArrowLeft" ? -KEY_STEP : event.key === "ArrowRight" ? KEY_STEP : 0;
    if (!step) return;
    event.preventDefault();
    onCommit(clampWidth(width + step, min, limit()));
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={width}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      title="Drag to resize; double-click to reset"
      data-dragging={dragging || undefined}
      className={cn(
        "group/resize absolute inset-y-0 -right-1 z-20 hidden w-2 cursor-col-resize touch-none outline-none md:block",
        className,
      )}
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        event.currentTarget.setPointerCapture?.(event.pointerId);
        drag.current = { startX: event.clientX, startWidth: width, max: limit(), width };
        moved.current = false;
        setDragging(true);
        document.body.style.cursor = "col-resize";
        document.body.style.userSelect = "none";
      }}
      onPointerMove={(event) => {
        const current = drag.current;
        if (!current) return;
        if (event.clientX !== current.startX) moved.current = true;
        current.width = clampWidth(current.startWidth + event.clientX - current.startX, min, current.max);
        onLive(current.width);
      }}
      onPointerUp={end}
      onPointerCancel={end}
      onLostPointerCapture={end}
      onDoubleClick={() => {
        if (!moved.current) onCommit(null);
      }}
      onKeyDown={key}
    >
      <span
        className={cn(
          "absolute inset-y-0 left-1/2 w-0.5 -translate-x-1/2 transition-colors",
          "group-hover/resize:bg-ring/60 group-focus-visible/resize:bg-ring group-data-dragging/resize:bg-ring",
        )}
      />
    </div>
  );
}

function restoreBody() {
  document.body.style.removeProperty("cursor");
  document.body.style.removeProperty("user-select");
}

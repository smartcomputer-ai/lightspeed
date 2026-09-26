import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import { cn } from "@/lib/utils";

/** How far one arrow key moves a handle. */
const KEY_STEP = 16;
/** A double-click this soon after a click expanded the column is that click's twin. */
const EXPAND_DOUBLE_CLICK_MS = 600;

export function clampWidth(width: number, min: number, max: number): number {
  return Math.round(Math.min(Math.max(width, min), Math.max(min, max)));
}

/**
 * A column that can also collapse (to icons, say): dragged narrower than
 * `below` it snaps collapsed, and dragged out past it again it expands.
 * Collapsing keeps the width it had, to come back to.
 */
export interface Collapsible {
  collapsed: boolean;
  below: number;
  /** The collapsed column's width, where a drag from it starts. */
  collapsedWidth: number;
  onCollapse: (collapsed: boolean) => void;
}

type Placement = { width: number; collapsed: boolean };

/**
 * A column's width while it may be dragged: the stored width (or the
 * default) until a drag starts, then the live placement until it ends and is
 * committed. `resizing` lets a column switch off its width transition.
 */
export function useResizableWidth({
  stored,
  fallback,
  min,
  max,
  room,
  collapse,
  onCommit,
}: {
  stored: number | null;
  fallback: number;
  min: number;
  max: number;
  /** The most the page can spare now, read when a drag starts. */
  room?: () => number;
  collapse?: Collapsible;
  /** The new width, or null to go back to the default. */
  onCommit: (width: number | null) => void;
}) {
  const [live, setLive] = useState<Placement | null>(null);
  const width = live?.width ?? clampWidth(stored ?? fallback, min, max);
  const collapsed = live?.collapsed ?? collapse?.collapsed ?? false;
  const limit = () => Math.min(max, room?.() ?? max);
  return {
    width,
    collapsed,
    resizing: live !== null,
    handle: { width, collapsed, min, max, limit, collapse, onLive: setLive, onCommit },
  };
}

type Drag = {
  startX: number;
  /** Where the edge was: the collapsed width when starting collapsed. */
  startEdge: number;
  startCollapsed: boolean;
  /** The expanded width before the drag, which collapsing keeps. */
  startWidth: number;
  max: number;
  moved: boolean;
  placement: Placement;
};

/**
 * The drag strip on a column's right edge. It sits over the column's border
 * (the parent must be positioned), shows a line while hovered, focused or
 * dragged, and works by pointer and by arrow keys. Double-click resets the
 * width; on a collapsed column a click expands it.
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
  const { width, collapsed, min, max, limit, collapse, onLive, onCommit } = handle;
  const drag = useRef<Drag | null>(null);
  // Whether the last drag moved: its two clicks are then no double-click.
  const lastMoved = useRef(false);
  const expandedAt = useRef(0);
  const [dragging, setDragging] = useState(false);
  const settle = useRef<(current: Drag) => void>(() => {});

  // What a finished drag leaves: a click on a collapsed column opens it; a
  // drag commits where it ended, collapsed or at its width.
  settle.current = (current: Drag) => {
    if (!current.moved) {
      if (current.startCollapsed && collapse) {
        expandedAt.current = Date.now();
        collapse.onCollapse(false);
      }
      return;
    }
    if (current.placement.collapsed) {
      if (!current.startCollapsed) collapse?.onCollapse(true);
      return;
    }
    onCommit(current.placement.width);
    if (current.startCollapsed) collapse?.onCollapse(false);
  };

  // A column leaving mid-drag (the page changed) keeps what it showed.
  useEffect(() => () => {
    const current = drag.current;
    if (!current) return;
    drag.current = null;
    restoreBody();
    settle.current(current);
  }, []);

  // Every way a drag can stop ends it once: release, cancel, or the browser
  // taking the pointer away without either.
  const end = (event: PointerEvent<HTMLDivElement>) => {
    const current = drag.current;
    if (!current) return;
    drag.current = null;
    lastMoved.current = current.moved;
    setDragging(false);
    if (event.currentTarget.hasPointerCapture?.(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    restoreBody();
    onLive(null);
    settle.current(current);
  };

  const place = (current: Drag, clientX: number): Placement => {
    const edge = current.startEdge + clientX - current.startX;
    if (collapse && edge < collapse.below) return { width: current.startWidth, collapsed: true };
    return { width: clampWidth(edge, min, current.max), collapsed: false };
  };

  const key = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = event.key === "ArrowLeft" ? -KEY_STEP : event.key === "ArrowRight" ? KEY_STEP : 0;
    if (!step) return;
    event.preventDefault();
    if (collapsed) {
      if (step > 0) collapse?.onCollapse(false);
      return;
    }
    // At its narrowest, one more step left collapses.
    if (collapse && step < 0 && width <= min) {
      collapse.onCollapse(true);
      return;
    }
    onCommit(clampWidth(width + step, min, limit()));
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={collapsed ? collapse?.collapsedWidth : width}
      aria-valuemin={collapse ? collapse.collapsedWidth : min}
      aria-valuemax={max}
      tabIndex={0}
      title={collapsed ? "Click or drag to expand" : "Drag to resize; double-click to reset"}
      data-dragging={dragging || undefined}
      className={cn(
        "group/resize absolute inset-y-0 -right-1 z-20 hidden w-2 cursor-col-resize touch-none outline-none md:block",
        className,
      )}
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        event.currentTarget.setPointerCapture?.(event.pointerId);
        drag.current = {
          startX: event.clientX,
          startEdge: collapsed && collapse ? collapse.collapsedWidth : width,
          startCollapsed: collapsed,
          startWidth: width,
          max: limit(),
          moved: false,
          placement: { width, collapsed },
        };
        setDragging(true);
        document.body.style.cursor = "col-resize";
        document.body.style.userSelect = "none";
      }}
      onPointerMove={(event) => {
        const current = drag.current;
        if (!current) return;
        if (event.clientX !== current.startX) current.moved = true;
        if (!current.moved) return;
        current.placement = place(current, event.clientX);
        onLive(current.placement);
      }}
      onPointerUp={end}
      onPointerCancel={end}
      onLostPointerCapture={end}
      onDoubleClick={() => {
        if (collapsed || lastMoved.current) return;
        if (Date.now() - expandedAt.current < EXPAND_DOUBLE_CLICK_MS) return;
        onCommit(null);
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

import type { CSSProperties, ReactNode } from "react";
import { ResizeHandle, useResizableWidth } from "@/components/resize-handle";
import { useUserPreferences } from "@/lib/user-preferences";
import { cn } from "@/lib/utils";

const DEFAULT_WIDTH = 320;
const MIN_WIDTH = 256;
const MAX_WIDTH = 576;
/** What the detail beside the list always keeps. */
const DETAIL_MIN_WIDTH = 384;

/**
 * The list column of a list-and-detail page (sessions, bots, profiles,
 * workspaces). Its width can be dragged and is one preference for every
 * such page. On narrow screens it is the whole page, hidden while a detail
 * is open.
 */
export function ListPane({
  detailOpen,
  children,
}: {
  /** A detail is open: narrow screens show it instead of the list. */
  detailOpen: boolean;
  children: ReactNode;
}) {
  const { listWidth, setListWidth } = useUserPreferences();
  const resizable = useResizableWidth({
    stored: listWidth,
    fallback: DEFAULT_WIDTH,
    min: MIN_WIDTH,
    max: MAX_WIDTH,
    room: () => {
      const sidebar = document.querySelector('[data-slot="sidebar-container"]')?.getBoundingClientRect().width ?? 0;
      return window.innerWidth - sidebar - DETAIL_MIN_WIDTH;
    },
    onCommit: setListWidth,
  });
  return (
    <aside
      style={{ "--list-width": `${resizable.width}px` } as CSSProperties}
      className={cn(
        // The CSS bound keeps the detail its room when the window narrows
        // after a drag.
        "relative w-full shrink-0 flex-col border-r md:flex md:w-[min(var(--list-width),max(16rem,calc(100%_-_24rem)))]",
        detailOpen ? "hidden" : "flex",
      )}
    >
      {children}
      <ResizeHandle label="Resize list" handle={resizable.handle} />
    </aside>
  );
}

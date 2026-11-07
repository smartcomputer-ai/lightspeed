import type { CSSProperties, ReactNode } from "react";
import { NavLink } from "react-router-dom";
import { ResizeHandle, useResizableWidth } from "@/components/resize-handle";
import { ListNote } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { Checkbox } from "@/components/ui/checkbox";
import { useUserPreferences } from "@/lib/user-preferences";
import { cn } from "@/lib/utils";

/// The parts of a list-and-detail page (sessions, bots, profiles,
/// workspaces):
///
///   ListPane            the resizable list column
///     ListPaneHeader    PaneBar with the title, a count and actions
///     ListPaneToolbar   optional strips under it (pickers, selection)
///     ListPaneBody      the scrolling list, with ListPaneState and
///                       ListGroupLabel, holding ListRows
///     ListPaneFooter    optional (load more)
///
/// PaneBar is the one header bar: the detail header beside the list builds
/// on it too, so both headers keep the same height.

const DEFAULT_WIDTH = 320;
const MIN_WIDTH = 256;
const MAX_WIDTH = 576;
/** What the detail beside the list always keeps. */
const DETAIL_MIN_WIDTH = 384;

/**
 * The list column of a list-and-detail page. Its width can be dragged and
 * is one preference for every such page. On narrow screens it is the whole
 * page, hidden while a detail is open.
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
      data-slot="list-pane"
      style={{ "--list-width": `${resizable.width}px` } as CSSProperties}
      className={cn(
        // The CSS bound keeps the detail its room when the window narrows
        // after a drag.
        "relative w-full shrink-0 flex-col border-r sheen-list md:flex md:w-[min(var(--list-width),max(16rem,calc(100%_-_24rem)))]",
        detailOpen ? "hidden" : "flex",
      )}
    >
      {children}
      <ResizeHandle label="Resize list" handle={resizable.handle} />
    </aside>
  );
}

/// The header bar of a pane: one fixed height for the list's header and
/// the detail's beside it, so their bottom borders line up.
export function PaneBar({
  as: Tag = "div",
  className,
  children,
}: {
  as?: "div" | "header";
  className?: string;
  children: ReactNode;
}) {
  return (
    <Tag data-slot="pane-bar" className={cn("flex h-12 min-w-0 shrink-0 items-center gap-2 border-b px-4", className)}>
      {children}
    </Tag>
  );
}

/// The list's title, how many it holds and its actions (new, filter, …),
/// right-aligned.
export function ListPaneHeader({
  title,
  count,
  actions,
}: {
  title: ReactNode;
  /** Shown beside the title once known, e.g. `12` or `50+`. */
  count?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <PaneBar>
      <h1 className="min-w-0 truncate text-sm font-semibold">{title}</h1>
      {count !== undefined && <span className="shrink-0 text-xs text-muted-foreground">{count}</span>}
      {actions && <div className="ml-auto flex shrink-0 items-center gap-1">{actions}</div>}
    </PaneBar>
  );
}

/// A strip under the header: a picker, a selection bar, a notice.
export function ListPaneToolbar({ className, children }: { className?: string; children: ReactNode }) {
  return (
    <div className={cn("flex min-w-0 shrink-0 flex-wrap items-center gap-2 border-b px-4 py-2 text-xs", className)}>
      {children}
    </div>
  );
}

/// The scrolling part of the pane.
export function ListPaneBody({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn("min-h-0 min-w-0 flex-1 overflow-y-auto", className)}>{children}</div>;
}

/// Pinned under the list, e.g. to load more.
export function ListPaneFooter({ children }: { children: ReactNode }) {
  return <div className="shrink-0 border-t p-2">{children}</div>;
}

/// Loading, the read error and the empty note of a list's query.
export function ListPaneState({
  loading,
  error,
  hasData,
  empty,
}: {
  loading: boolean;
  error: unknown;
  hasData: boolean;
  /** The note shown once loaded with nothing to list; omit while there are rows. */
  empty?: ReactNode;
}) {
  return (
    <>
      {loading && <p className="p-4 text-sm text-muted-foreground">Loading…</p>}
      {Boolean(error) && <ReadError error={error} loading={!hasData} className="p-4" />}
      {hasData && empty && <ListNote>{empty}</ListNote>}
    </>
  );
}

/// The heading over a group of rows (Active, Paused, …).
export function ListGroupLabel({ children }: { children: ReactNode }) {
  return (
    <div className="px-4 pt-3 pb-1 text-[10px] font-semibold tracking-wider text-muted-foreground uppercase">
      {children}
    </div>
  );
}

export interface ListRowSelection {
  checked: boolean;
  onToggle: () => void;
  /** Accessible name of the checkbox. */
  label: string;
}

/// One row of a list pane: a link to its detail, or, while selecting, a
/// checkbox label. The first line is the indicator (a status dot), the
/// title, badges and the meta (a time, a count) on the right; children are
/// further lines, usually ListRowLines.
export function ListRow({
  to,
  active = false,
  selection,
  leading,
  indicator,
  title,
  titleHint,
  badges,
  meta,
  depth = 0,
  children,
}: {
  to: string;
  active?: boolean;
  /** Present while the list is selecting: the row toggles instead of opening. */
  selection?: ListRowSelection;
  /** Beside every line, e.g. an avatar. */
  leading?: ReactNode;
  indicator?: ReactNode;
  title: ReactNode;
  /** Tooltip on the title, e.g. the full id behind a display name. */
  titleHint?: string;
  badges?: ReactNode;
  meta?: ReactNode;
  /** Nesting under a parent row: indents and marks the row. */
  depth?: number;
  children?: ReactNode;
}) {
  const highlighted = active || selection?.checked;
  const className = cn(
    "flex min-w-0 items-center gap-2.5 border-b px-4 py-2.5 text-sm hover:bg-muted/50",
    highlighted && "bg-muted",
    selection && "cursor-pointer items-start",
  );
  const style = depth > 0 ? { paddingLeft: `${1 + depth * 1.25}rem` } : undefined;
  const content = (
    <>
      {selection && (
        <Checkbox
          checked={selection.checked}
          onCheckedChange={selection.onToggle}
          className="mt-0.5"
          aria-label={selection.label}
        />
      )}
      {leading && <span className="flex shrink-0 items-center">{leading}</span>}
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex min-w-0 items-center gap-2">
          {depth > 0 && <span className="shrink-0 text-muted-foreground">↳</span>}
          {indicator}
          <span className="min-w-0 flex-1 truncate font-medium" title={titleHint}>{title}</span>
          {badges}
          {meta !== undefined && meta !== null && meta !== false && (
            <span className="ml-auto shrink-0 text-xs text-muted-foreground">{meta}</span>
          )}
        </span>
        {children}
      </span>
    </>
  );
  return (
    <li>
      {selection ? (
        <label className={className} style={style}>{content}</label>
      ) : (
        <NavLink to={to} className={className} style={style}>{content}</NavLink>
      )}
    </li>
  );
}

/// A secondary line of a ListRow: muted, one line, truncated.
export function ListRowLine({
  mono = false,
  tone,
  title,
  className,
  children,
}: {
  mono?: boolean;
  tone?: "destructive";
  title?: string;
  className?: string;
  children: ReactNode;
}) {
  return (
    <span
      title={title}
      className={cn(
        "min-w-0 truncate text-xs text-muted-foreground",
        mono && "font-mono",
        tone === "destructive" && "text-destructive",
        className,
      )}
    >
      {children}
    </span>
  );
}

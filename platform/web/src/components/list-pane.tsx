import type { CSSProperties, ReactNode } from "react";
import { NavLink } from "react-router-dom";
import { ResizeHandle, useResizableWidth } from "@/components/resize-handle";
import { ListNote } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { Checkbox } from "@/components/ui/checkbox";
import { useUserPreferences } from "@/lib/user-preferences";
import { cn } from "@/lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { MetaToolbar, type MetaItem } from "@/components/meta-toolbar";

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
        "relative w-full shrink-0 flex-col border-r bg-(--surface-list) sheen-list md:flex md:w-[min(var(--list-width),max(16rem,calc(100%_-_24rem)))]",
        detailOpen ? "hidden" : "flex",
      )}
    >
      {children}
      <ResizeHandle label="Resize list" handle={resizable.handle} />
    </aside>
  );
}

/// The header bar of a pane: the list's header and the detail's beside it.
/// Its height is the sidebar header's (`--pane-bar-height` in index.css),
/// so the titles sit on the universe name's line and the bottom edges meet.
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
    <Tag data-slot="pane-bar" className={cn("flex h-(--pane-bar-height) min-w-0 shrink-0 items-center gap-2 border-b px-4", className)}>
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

/// A row's place in a tree of rows (a session and its sub-agents, nested
/// to any depth).
export interface ListRowTree {
  /** 0 for a root, 1 for its children, and so on. */
  depth: number;
  /** The last of its siblings: its branch curves off the line, which ends. */
  last?: boolean;
  /**
   * For each ancestor level from 1 to depth - 1: whether that ancestor has a
   * later sibling, so the line its parent drops runs on past this row.
   */
  lines?: boolean[];
  /** Child rows follow right below: a line drops from this row to them. */
  children?: boolean;
}

/// Where lines run. A root starts its line at a dot on its bottom edge (where
/// a separator would be), centred on the margin its title and subtitle start
/// at; a nested parent drops its line from its own branch dot,
/// just left of its content, where each child's branch also ends. Each level
/// indents so every branch is TREE_BRANCH_REM long: the first level from the
/// root's line, deeper ones from their parent's dot.
const TREE_ANCHOR_REM = 0;
const TREE_DOT_REM = -0.375;
const TREE_BRANCH_REM = 0.875;
const TREE_FIRST_INDENT_REM = TREE_ANCHOR_REM - TREE_DOT_REM + TREE_BRANCH_REM;
const TREE_INDENT_REM = TREE_BRANCH_REM;
/// What a selection checkbox (size-4) and the row's gap after it add before the content.
const TREE_SELECTION_REM = 1.625;

/// How far a row at this depth is indented.
const treeIndent = (depth: number) => (depth > 0 ? TREE_FIRST_INDENT_REM + (depth - 1) * TREE_INDENT_REM : 0);
/// Where a row at this depth drops its line, from its content start.
const treeAnchor = (depth: number) => (depth === 0 ? TREE_ANCHOR_REM : TREE_DOT_REM);

const TREE_VERTICAL = "absolute w-px -translate-x-1/2 bg-(--list-tree-line)";
const TREE_DOT = "absolute size-[5px] -translate-x-1/2 -translate-y-1/2 rounded-full bg-(--list-tree-dot)";

/// The lines of a nested row, laid over the row (out of its flow, so nothing
/// shifts) from its top edge to its bottom one, with its branch and dot at
/// its vertical centre: the line from its parent (curving off when it is the
/// last child, else running on), the lines of ancestors that run past it,
/// and, when it has children, the line it drops to them from its dot.
/// `contentLeft` is where the row's content starts. Lines and dots are
/// solid colors, so where they meet nothing shows through darker.
function TreeLines({ tree, contentLeft }: { tree: ListRowTree; contentLeft: string }) {
  const { depth, last = false, lines = [], children = false } = tree;
  // Where the line of the parent at level - 1 runs, from this row's content start.
  const trunk = (level: number) => treeAnchor(level - 1) + treeIndent(level - 1) - treeIndent(depth);
  const own = trunk(depth);
  const branch = `${TREE_DOT_REM - own}rem`;
  return (
    <span aria-hidden className="pointer-events-none absolute inset-y-0" style={{ left: contentLeft }}>
      {lines.map((runs, index) => runs && (
        <span key={index} className={TREE_VERTICAL} style={{ left: `${trunk(index + 1)}rem`, top: 0, bottom: 0 }} />
      ))}
      {last ? (
        <span
          className="absolute top-0 h-[calc(50%+0.5px)] rounded-bl-[7px] border-b border-l border-(--list-tree-line)"
          style={{ left: `calc(${own}rem - 0.5px)`, width: branch }}
        />
      ) : (
        <>
          <span className={TREE_VERTICAL} style={{ left: `${own}rem`, top: 0, bottom: 0 }} />
          <span className="absolute top-[calc(50%-0.5px)] h-px bg-(--list-tree-line)" style={{ left: `${own}rem`, width: branch }} />
        </>
      )}
      {children && <span className={TREE_VERTICAL} style={{ left: `${TREE_DOT_REM}rem`, top: "50%", bottom: 0 }} />}
      <span className={cn(TREE_DOT, "top-1/2")} style={{ left: `${TREE_DOT_REM}rem` }} />
    </span>
  );
}

/// Where a root's line starts: a dot on the row's bottom edge, where a
/// separator would be, a little into its content, and a dotted line from it
/// to the right, ending as far from the right edge as the dot is from the
/// left. The line down begins in the first child row. Drawn above the next
/// row, whose background would otherwise cover the dot's lower half.
/// `contentLeft` is where the row's content starts.
function TreeStart({ contentLeft }: { contentLeft: string }) {
  const inset = `calc(${contentLeft} + ${TREE_ANCHOR_REM}rem)`;
  return (
    <span aria-hidden className="pointer-events-none absolute inset-0 z-10">
      {/* Dashes of 2px every 5px: a drawn pattern, longer and sparser than a dotted border. */}
      <span
        className="absolute top-[calc(100%-0.5px)] h-px bg-[linear-gradient(to_right,var(--list-tree-line)_2px,transparent_2px)] bg-size-[5px_1px] bg-repeat-x"
        style={{ left: inset, right: inset }}
      />
      <span className={cn(TREE_DOT, "top-full")} style={{ left: inset }} />
    </span>
  );
}

/// Whether a tree row's next row belongs to the same tree, so no separator
/// is drawn between them: it has children, or it is a child that is not the
/// last row of its root's tree.
function joinedBelow(tree: ListRowTree | undefined): boolean {
  if (!tree) return false;
  if (tree.children) return true;
  if (tree.depth === 0) return false;
  return !tree.last || (tree.lines ?? []).some(Boolean);
}

export interface ListRowSelection {
  checked: boolean;
  onToggle: () => void;
  /** Accessible name of the checkbox. */
  label: string;
}

/// One row of a list pane: a link to its detail, or, while selecting, a
/// checkbox label. The first line is the indicator (a status dot), the
/// title, its marks (the entity's meta items, compact), badges and the
/// meta (a time, a count) on the right; children are
/// further lines, usually ListRowLines. Every row is at least
/// --list-row-height tall with its content centred, so one-line and
/// two-line lists share one rhythm with the pane bar above them.
export function ListRow({
  to,
  active = false,
  selection,
  leading,
  indicator,
  title,
  titleHint,
  marks,
  badges,
  meta,
  tree,
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
  /** The entity's meta items, drawn compact. They take at most half the line and give way by priority, so the title keeps its room; inside the row link they only explain themselves, and show their status colors only while the row is hovered, focused or selected. */
  marks?: MetaItem[];
  badges?: ReactNode;
  meta?: ReactNode;
  /** Its place in a tree of rows: nested rows indent and hang off their parent's line, with no separators inside the tree. */
  tree?: ListRowTree;
  children?: ReactNode;
}) {
  const highlighted = active || selection?.checked;
  const className = cn(
    "group/row flex min-h-(--list-row-height) min-w-0 items-center gap-2.5 border-b px-4 py-1.5 text-sm transition-colors duration-(--list-transition-duration) hover:bg-muted/50",
    highlighted && "bg-muted",
    selection && "cursor-action",
    // Tree lines are laid over the row. Inside a tree there is no separator
    // (min-height counts the border, so the row keeps its height) and the
    // lines run on into the next row unbroken.
    tree && "relative",
    joinedBelow(tree) && "border-b-0",
  );
  const depth = tree?.depth ?? 0;
  const style = depth > 0 ? { paddingLeft: `${1 + treeIndent(depth)}rem` } : undefined;
  // Where the content starts: the padding, and a selection checkbox. (A
  // leading avatar is not counted: lists that nest have none.)
  const contentLeft = `${1 + treeIndent(depth) + (selection ? TREE_SELECTION_REM : 0)}rem`;
  const content = (
    <>
      {tree && depth > 0 && <TreeLines tree={tree} contentLeft={contentLeft} />}
      {tree?.children && depth === 0 && <TreeStart contentLeft={contentLeft} />}
      {selection && (
        <Checkbox
          checked={selection.checked}
          onCheckedChange={selection.onToggle}
          aria-label={selection.label}
        />
      )}
      {leading && <span className="flex shrink-0 items-center">{leading}</span>}
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex min-w-0 items-center gap-2">
          {indicator}
          {/* Slightly dimmer at rest; full strength on hover, focus or selection. */}
          <span
            className="min-w-0 flex-1 truncate font-(--list-title-font-weight) text-(--item-foreground-rest) group-data-highlighted/row:font-(--list-title-font-weight-selected) transition-[color,font-weight] duration-(--list-transition-duration) group-hover/row:text-foreground group-focus-visible/row:text-foreground group-data-highlighted/row:text-foreground"
            title={titleHint}
          >
            {title}
          </span>
          {marks && marks.length > 0 && (
            <MetaToolbar items={marks} place="list" className="max-w-1/2" />
          )}
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
        <label className={className} style={style} data-highlighted={highlighted || undefined}>{content}</label>
      ) : (
        <NavLink to={to} className={className} style={style} data-highlighted={highlighted || undefined}>{content}</NavLink>
      )}
    </li>
  );
}

/// A small mark in a list row (an icon, perhaps with a count) with a
/// tooltip that says what it means. `label` names it for assistive tech.
export function ListMark({ label, tip, children }: { label: string; tip: ReactNode; children: ReactNode }) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={<span className="flex shrink-0 items-center gap-0.5 text-xs text-muted-foreground tabular-nums" aria-label={label} />}
      >
        {children}
      </TooltipTrigger>
      <TooltipContent>{tip}</TooltipContent>
    </Tooltip>
  );
}

/// A secondary line of a ListRow: one line, truncated, and dimmer than
/// muted text until its row is hovered, focused or selected, so the titles
/// lead while scanning.
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
        "min-w-0 truncate text-xs",
        // A toned line keeps its color; others brighten with their row.
        tone === "destructive"
          ? "text-destructive"
          : "text-muted-foreground/40 transition-colors duration-(--list-transition-duration) group-hover/row:text-muted-foreground group-focus-visible/row:text-muted-foreground group-data-highlighted/row:text-muted-foreground",
        mono && "font-mono",
        className,
      )}
    >
      {children}
    </span>
  );
}

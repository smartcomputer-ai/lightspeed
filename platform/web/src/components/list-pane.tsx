import { createContext, useContext, useState, type ComponentProps, type CSSProperties, type HTMLAttributes, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { NavLink } from "react-router-dom";
import { ResizeHandle, useResizableWidth } from "@/components/resize-handle";
import { ListNote } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { menuPoint, pointAnchor, type RowMenuAnchor } from "@/lib/row-menu";
import { useUserPreferences } from "@/lib/user-preferences";
import { cn } from "@/lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { MetaToolbar, type MetaItem } from "@/components/meta-toolbar";
import { fileIcon } from "@/components/file-icon";
import { FolderClosedIcon, FolderOpenIcon } from "@/icons";

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
    <PaneBar className="pl-6">
      <h1 className="min-w-0 truncate text-sm font-semibold">{title}</h1>
      {count !== undefined && <span className="shrink-0 text-xs text-muted-foreground">{count}</span>}
      {actions && <div className="ml-auto flex shrink-0 items-center gap-1">{actions}</div>}
    </PaneBar>
  );
}

/// A strip under the header: a picker, a selection bar, a notice.
export function ListPaneToolbar({ className, children }: { className?: string; children: ReactNode }) {
  return (
    <div className={cn("flex min-w-0 shrink-0 flex-wrap items-center gap-2 border-b py-2 pr-4 pl-6 text-xs", className)}>
      {children}
    </div>
  );
}

/// What a list holds, which tunes how its rows behave and look: `default`,
/// or `files` (a workspace's files and folders). Set on ListPaneBody, which
/// hands it to its rows; both carry it as data-list-mode.
export type ListMode = "default" | "files";

/// How the rows of a list are drawn: its mode, and whether it is compressed
/// (each row its first line alone, shorter, the title centred with its
/// marks and meta; the second lines are left out).
interface ListLayoutValue {
  mode: ListMode;
  compressed: boolean;
}

const ListLayoutContext = createContext<ListLayoutValue>({ mode: "default", compressed: false });

/// The mode of the list a row is in.
export function useListMode(): ListMode {
  return useContext(ListLayoutContext).mode;
}

/// Whether the list a row is in is compressed.
export function useListCompressed(): boolean {
  return useContext(ListLayoutContext).compressed;
}

/// A list's layout for the rows inside, for a list that is not in a
/// ListPaneBody (which sets it itself), e.g. inside its own scrolling area.
export function ListLayout({ mode = "default", compressed = false, children }: {
  mode?: ListMode;
  compressed?: boolean;
  children: ReactNode;
}) {
  return <ListLayoutContext.Provider value={{ mode, compressed }}>{children}</ListLayoutContext.Provider>;
}

/// The scrolling part of the pane, and the list's layout for its rows: its
/// mode, and whether it is compressed (data-list-compressed).
export function ListPaneBody({ mode = "default", compressed = false, className, children }: {
  mode?: ListMode;
  compressed?: boolean;
  className?: string;
  children: ReactNode;
}) {
  return (
    <ListLayoutContext.Provider value={{ mode, compressed }}>
      <div
        data-list-mode={mode}
        data-list-compressed={compressed || undefined}
        className={cn("min-h-0 min-w-0 flex-1 overflow-y-auto", className)}
      >
        {children}
      </div>
    </ListLayoutContext.Provider>
  );
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
    <div className="pt-3 pr-4 pb-1 pl-6 text-[10px] font-semibold tracking-wider text-muted-foreground uppercase">
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
  /** The next row is a folder whose children follow (in a files list, a line sets its block apart). */
  nextOpen?: boolean;
}

/// The list's left inset (pl-6): room for a row's dot before its content.
/// The header, toolbar and group labels share it.
const LIST_INSET_REM = 1.5;

/// Where a row's dot sits, from its content start (a root's in the middle
/// of the list's inset): its status dot (the indicator, in lists that show
/// one, as Sessions and Bots do), else a small plain dot in the lines'
/// color. It is the row's node in a tree:
/// every parent, a root as any other, drops its line to its children from
/// it, and each child's branch ends at it. Lines stop at its edge, so a
/// translucent or hollow status dot shows no line through it. Each level
/// indents by TREE_BRANCH_REM, so every branch is that long.
const ROW_DOT_REM = -LIST_INSET_REM / 2;
/// A status dot's radius, and a plain dot's: lines end at the dot's edge.
const ROW_DOT_RADIUS_PX = 4;
const PLAIN_DOT_RADIUS_PX = 1.5;
/// A nested row's plain dot sits this far right of its node: the lines keep
/// their places, so each branch reaches this much further. A root's dot
/// stays on its line.
const PLAIN_DOT_STUB_PX = 4;
const TREE_BRANCH_REM = 0.875;
/// A selection checkbox sits mostly in the list's inset (pulled left by
/// 1rem, so it starts 8px from the edge). Exported so a select-all checkbox
/// in a toolbar keeps the same column.
export const LIST_CHECKBOX_INSET_CLASS = "-ml-4";

/// What a selection checkbox (size-4, pulled into the inset), its margin and
/// the row's gap after it add before the content: the dot sits between it
/// and the content, 10px from the checkbox.
const TREE_SELECTION_REM = 1.625;

/// How far a row at this depth is indented.
const treeIndent = (depth: number) => depth * TREE_BRANCH_REM;

/// Where the line of the parent at `level` - 1 runs, from the content start of a row at `depth`.
const treeTrunk = (level: number, depth: number) => ROW_DOT_REM + treeIndent(level - 1) - treeIndent(depth);

const TREE_VERTICAL = "absolute w-px -translate-x-1/2 bg-(--list-tree-line)";

/// The lines of a tree row, laid over the row (out of its flow, so nothing
/// shifts) from its top edge to its bottom one, meeting its dot at its
/// vertical centre: the line from its parent (curving off when it is the
/// last child, else running on) and its branch to the dot, the lines of
/// ancestors that run past it, and, when it has children, the line it drops
/// to them from the dot. A root has no parent: only the line down. Without
/// a status dot it draws a small plain one, in the lines' color, so it reads
/// as part of them, a nested row's a little (PLAIN_DOT_STUB_PX) right of
/// where the lines meet. `contentLeft` is where the row's
/// content starts. Lines and dots are solid colors, so where they meet
/// nothing shows through darker.
function TreeLines({ tree, plainDot, contentLeft }: { tree: ListRowTree; plainDot: boolean; contentLeft: string }) {
  const { depth, last = false, lines = [], children = false } = tree;
  const trunk = (level: number) => treeTrunk(level, depth);
  const own = trunk(depth);
  const radius = plainDot ? PLAIN_DOT_RADIUS_PX : ROW_DOT_RADIUS_PX;
  // How far right of the node the dot sits (a nested row's plain dot only).
  const stub = plainDot && depth > 0 ? PLAIN_DOT_STUB_PX : 0;
  const branch = `calc(${ROW_DOT_REM - own}rem + ${stub - radius}px)`;
  return (
    <span aria-hidden className="pointer-events-none absolute inset-y-0" style={{ left: contentLeft }}>
      {lines.map((runs, index) => runs && (
        <span key={index} className={TREE_VERTICAL} style={{ left: `${trunk(index + 1)}rem`, top: 0, bottom: 0 }} />
      ))}
      {depth > 0 && (last ? (
        <span
          className="absolute top-0 h-[calc(50%+0.5px)] rounded-bl-[7px] border-b border-l border-(--list-tree-line)"
          style={{ left: `calc(${own}rem - 0.5px)`, width: branch }}
        />
      ) : (
        <>
          <span className={TREE_VERTICAL} style={{ left: `${own}rem`, top: 0, bottom: 0 }} />
          <span className="absolute top-[calc(50%-0.5px)] h-px bg-(--list-tree-line)" style={{ left: `${own}rem`, width: branch }} />
        </>
      ))}
      {children && (
        // Down from the dot, or, beside it, from where the branch passes the node.
        <span
          className={TREE_VERTICAL}
          style={{ left: `${ROW_DOT_REM}rem`, top: stub > 0 ? "calc(50% - 0.5px)" : `calc(50% + ${radius}px)`, bottom: 0 }}
        />
      )}
      {plainDot && (
        <span
          className="absolute top-1/2 -translate-x-1/2 -translate-y-1/2 rounded-full bg-(--list-tree-line)"
          style={{ left: `calc(${ROW_DOT_REM}rem + ${stub}px)`, width: PLAIN_DOT_RADIUS_PX * 2, height: PLAIN_DOT_RADIUS_PX * 2 }}
        />
      )}
    </span>
  );
}

/// Where the dotted separator on a tree row's bottom edge starts, from its
/// content start: at the branch line the next row hangs from, the rightmost
/// line running across that edge, so it never crosses one. A parent's own
/// line to its children; else its own parent's line when that runs on; else
/// the nearest ancestor's that does. Null when no line runs on: the tree
/// ends there, and the row keeps its separator.
function treeSeparatorStart(tree: ListRowTree | undefined): number | null {
  if (!tree) return null;
  const { depth, last = false, lines = [], children = false } = tree;
  if (children) return ROW_DOT_REM;
  if (depth === 0) return null;
  if (!last) return treeTrunk(depth, depth);
  const level = lines.lastIndexOf(true);
  return level < 0 ? null : treeTrunk(level + 1, depth);
}

/// The dotted separator on a tree row's bottom edge, in place of the solid
/// one: from `start` (see treeSeparatorStart) to the right, ending as far
/// from the right edge as a root's line is from the left without a
/// selection checkbox, so every boundary's ends at the same place. Drawn
/// above the next row, whose background would otherwise cover it.
/// `contentLeft` is where the row's content starts.
function TreeSeparator({ start, contentLeft }: { start: number; contentLeft: string }) {
  return (
    <span aria-hidden className="pointer-events-none absolute inset-0 z-10">
      {/* Dashes of 2px every 5px: a drawn pattern, longer and sparser than a dotted border. */}
      <span
        className="absolute top-[calc(100%-0.5px)] h-px bg-[linear-gradient(to_right,var(--list-tree-line)_2px,transparent_2px)] bg-size-[5px_1px] bg-repeat-x"
        style={{ left: `calc(${contentLeft} + ${start}rem)`, right: `${LIST_INSET_REM + ROW_DOT_REM}rem` }}
      />
    </span>
  );
}

/// A row that opens and closes its children (a folder) instead of opening a
/// detail: it is a button, aria-expanded while `open`. The list draws only
/// the rows it is given, so the caller leaves the children out while closed.
export interface ListRowToggle {
  open: boolean;
  onToggle: () => void;
}

/// Attributes for a row's link or button (the entry the row is drawn on):
/// handlers, focus order and data attributes, for lists that add behavior
/// (a keyboard tree, dragging). Its handlers run beside the row's own.
export type ListRowEntryProps = HTMLAttributes<HTMLElement> & {
  draggable?: boolean;
  [data: `data-${string}`]: string | number | boolean | undefined;
};

export interface ListRowSelection {
  checked: boolean;
  onToggle: () => void;
  /** Accessible name of the checkbox. */
  label: string;
  /** What can be done with the selected rows: a checked row's right-click menu. */
  actions?: readonly ListSelectionAction[];
}

/// Something a list can do with its selected rows, e.g. close or delete
/// them. One list of these is drawn twice: as the selection bar's buttons
/// (ListSelectionButtons) and as the menu a right-click on a selected row
/// opens. The action decides what selecting it does, typically opening its
/// confirmation, which the page owns.
export interface ListSelectionAction {
  key: string;
  /** The verb, e.g. "Close"; shown with the count. */
  label: string;
  icon?: ReactNode;
  /** How many of the selected rows it applies to; with none it is disabled. */
  count: number;
  destructive?: boolean;
  /** Unavailable for now, e.g. while another bulk action runs. */
  disabled?: boolean;
  onSelect: () => void;
}

const actionText = (action: ListSelectionAction) => `${action.label} ${action.count}`;
const actionDisabled = (action: ListSelectionAction) => action.disabled || action.count === 0;

/// The selection bar's buttons for its actions.
export function ListSelectionButtons({ actions }: { actions: readonly ListSelectionAction[] }) {
  return (
    <div className="ml-auto flex items-center gap-1">
      {actions.map((action) => (
        <Button
          key={action.key}
          variant="outline"
          size="sm"
          className={cn(action.destructive && "text-destructive")}
          disabled={actionDisabled(action)}
          onClick={action.onSelect}
        >
          {action.icon}
          {actionText(action)}
        </Button>
      ))}
    </div>
  );
}

/// The actions as a menu at a point: a selected row's right-click.
function ListSelectionMenu({ actions, anchor, onClose }: {
  actions: readonly ListSelectionAction[];
  anchor: RowMenuAnchor;
  onClose: () => void;
}) {
  return (
    <DropdownMenu open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DropdownMenuContent anchor={anchor} className="min-w-44">
        {actions.map((action) => (
          <DropdownMenuItem
            key={action.key}
            variant={action.destructive ? "destructive" : "default"}
            disabled={actionDisabled(action)}
            onClick={action.onSelect}
          >
            {action.icon}
            {actionText(action)}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/// One row of a list pane: a link to its detail, a button that opens and
/// closes its children (`toggle`), or, while selecting, a checkbox label. The first line is the indicator (a status dot), the
/// title, its marks (the entity's meta items, compact), badges and the
/// meta (a time, a count) on the right; children are
/// further lines, usually ListRowLines. Every row is at least
/// --list-row-height tall with its content centred, so one-line and
/// two-line lists share one rhythm with the pane bar above them; in a
/// compressed list (ListPaneBody compressed) the further lines are left out
/// and rows are --list-row-height-compressed tall.
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
  toggle,
  fileName,
  actions,
  itemProps,
  entryProps,
  children,
}: {
  /** Its detail. A toggle row has none. */
  to?: string;
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
  /** Opens and closes its children instead of opening a detail. */
  toggle?: ListRowToggle;
  /** In a files list: the file's name with its extension, which picks its icon. */
  fileName?: string;
  /**
   * Its actions, e.g. a ⋯ menu: beside the row's link (never inside it), at
   * its right, shown in place of the meta while the row is hovered or has
   * keyboard focus (not focus a click leaves behind) or its menu is open.
   * Not while selecting.
   */
  actions?: ReactNode;
  /** Attributes for the row's list item (e.g. a tree item's role, focus order and data). */
  itemProps?: Omit<ComponentProps<"li">, "children"> & Record<`data-${string}`, string | number | boolean | undefined>;
  /** Attributes for the row's link or button (see ListRowEntryProps). */
  entryProps?: ListRowEntryProps;
  children?: ReactNode;
}) {
  const mode = useListMode();
  // In a files list a row draws its own icon unless given one: a folder
  // (a toggle row) open or closed, a file by its type.
  const FileTypeIcon = mode === "files" && !leading
    ? toggle ? (toggle.open ? FolderOpenIcon : FolderClosedIcon) : fileName ? fileIcon(fileName) : undefined
    : undefined;
  const lead = FileTypeIcon ? <FileTypeIcon className="size-4 text-muted-foreground" aria-hidden /> : leading;
  // Compressed, a row is its first line alone: the further lines are left out.
  const compressed = useListCompressed();
  const highlighted = active || selection?.checked;
  const rowActions = selection ? undefined : actions;
  // Inside a tree, the next row belongs to the same tree: a dotted separator
  // from the branch line replaces the solid one, and the lines run on unbroken.
  const separatorStart = treeSeparatorStart(tree);
  // A files list's lines go only around each open folder and what it holds
  // (above the folder, below its last row), and under the list's last row. A row draws the line below it when the
  // next row opens a folder, or when it ends a folder (or the list).
  const filesTree = mode === "files" ? tree : undefined;
  const lineBelow = filesTree
    ? Boolean(filesTree.nextOpen || (!filesTree.children && filesTree.last))
    : separatorStart === null;
  // A files list's line that meets a branch line is drawn dotted from that
  // line (it never runs left past the tree); without one it is the row's
  // full, solid border.
  const ruleStart = filesTree && lineBelow ? separatorStart : null;
  const className = cn(
    "group/row flex min-w-0 items-center gap-2.5 border-b py-1.5 pr-4 pl-6 text-sm transition-colors duration-(--list-transition-duration) hover:bg-muted/50",
    compressed ? "min-h-(--list-row-height-compressed)" : "min-h-(--list-row-height)",
    highlighted && "bg-muted",
    (selection || toggle) && "cursor-action",
    // Over its actions (beside the link) the row stays hovered.
    rowActions && "[li:hover>&]:bg-muted/50",
    toggle && !selection && "w-full text-left",
    // With nothing on the right to give way (marks stay, for their
    // tooltips), the row leaves room for the actions (always shown on touch
    // screens), which then sit beside its marks.
    rowActions && (meta === undefined || meta === null || meta === false) && "pr-11",
    // The dot and tree lines are laid over the row.
    (tree || indicator) && "relative",
    // Inside a tree there is no solid separator; a pixel more padding takes
    // its place, so the row keeps its height and its content sits where it
    // does above a separator (a folder that opens and joins its children
    // stays put).
    (!lineBelow || ruleStart !== null) && "border-b-0 pb-[calc(0.375rem+1px)]",
    entryProps?.className,
  );
  const depth = tree?.depth ?? 0;
  // Nested rows indent their content; while selecting, after the checkbox,
  // so the checkboxes keep one column.
  const indent = treeIndent(depth);
  const style = depth > 0 && !selection ? { paddingLeft: `${LIST_INSET_REM + indent}rem` } : undefined;
  // Where the content starts: the padding, and a selection checkbox. (A
  // leading avatar is not counted: lists that nest have none.)
  const contentLeft = `${LIST_INSET_REM + treeIndent(depth) + (selection ? TREE_SELECTION_REM : 0)}rem`;
  const content = (
    <>
      {tree && (depth > 0 || tree.children) && <TreeLines tree={tree} plainDot={!indicator} contentLeft={contentLeft} />}
      {separatorStart !== null && !filesTree && (
        <TreeSeparator start={separatorStart} contentLeft={contentLeft} />
      )}
      {ruleStart !== null && <TreeSeparator start={ruleStart} contentLeft={contentLeft} />}
      {/* The status dot, in the inset left of the content at the row's centre: its node when in a tree. */}
      {indicator && (
        <span
          className="absolute top-1/2 z-10 flex -translate-x-1/2 -translate-y-1/2 items-center justify-center"
          style={{ left: `calc(${contentLeft} + ${ROW_DOT_REM}rem)` }}
        >
          {indicator}
        </span>
      )}
      {selection && (
        <Checkbox
          checked={selection.checked}
          onCheckedChange={selection.onToggle}
          aria-label={selection.label}
          // Room for the dot (and a nested row's indent) between it and the content.
          className={cn(LIST_CHECKBOX_INSET_CLASS, "mr-4")}
          style={indent > 0 ? { marginRight: `${1 + indent}rem` } : undefined}
        />
      )}
      {lead && <span className="flex shrink-0 items-center">{lead}</span>}
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex min-w-0 items-center gap-2">
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
            <span
              className={cn(
                "ml-auto shrink-0 text-xs text-muted-foreground",
                // Its actions take its place while they show.
                rowActions && "group-focus-visible/row:invisible group-has-[:focus-visible]/row:invisible group-hover/row:invisible group-has-[[data-popup-open]]/row:invisible",
              )}
            >
              {meta}
            </span>
          )}
        </span>
        {!compressed && children}
      </span>
    </>
  );
  const { className: _entryClassName, onClick: entryClick, onContextMenu: entryContextMenu, ...entry } = entryProps ?? {};
  // While selecting, a right-click on a selected row opens what can be done
  // with the selection; on any other row it does what it does outside
  // selection mode (the row's own menu, if it has one).
  const selectionActions = selection?.checked && selection.actions?.length ? selection.actions : undefined;
  const [selectionMenu, setSelectionMenu] = useState<RowMenuAnchor | null>(null);
  const contextMenu = (event: ReactMouseEvent<HTMLElement>) => {
    if (selectionActions) {
      event.preventDefault();
      setSelectionMenu(pointAnchor(menuPoint(event)));
      return;
    }
    entryContextMenu?.(event);
  };
  const { className: itemClassName, ...item } = itemProps ?? {};
  return (
    // With actions the item is the row's hover group too, so the row stays
    // lit (and its title bright) over them.
    <li {...item} className={cn(rowActions && "group/row relative", itemClassName)}>
      {selection ? (
        <label {...entry} onContextMenu={contextMenu} className={className} style={style} data-list-mode={mode} data-highlighted={highlighted || undefined}>{content}</label>
      ) : toggle ? (
        <button
          type="button"
          aria-expanded={toggle.open}
          {...entry}
          onContextMenu={entryContextMenu}
          className={className}
          style={style}
          onClick={(event) => {
            entryClick?.(event);
            toggle.onToggle();
          }}
          data-list-mode={mode}
          data-highlighted={highlighted || undefined}
        >
          {content}
        </button>
      ) : (
        <NavLink
          {...entry}
          onContextMenu={entryContextMenu}
          to={to ?? ""}
          className={className}
          style={style}
          onClick={entryClick}
          data-list-mode={mode}
          data-highlighted={highlighted || undefined}
        >
          {content}
        </NavLink>
      )}
      {selectionActions && selectionMenu && (
        <ListSelectionMenu actions={selectionActions} anchor={selectionMenu} onClose={() => setSelectionMenu(null)} />
      )}
      {rowActions && (
        <div className="absolute top-1/2 right-2 z-20 flex -translate-y-1/2 items-center opacity-0 transition-opacity duration-(--list-transition-duration) group-focus-visible/row:opacity-100 group-has-[:focus-visible]/row:opacity-100 group-hover/row:opacity-100 group-has-[[data-popup-open]]/row:opacity-100 [@media(hover:none)]:opacity-100">
          {rowActions}
        </div>
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

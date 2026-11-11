import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import type { VfsTreeEntry } from "@/api";
import { ListLayout, ListRow, type ListRowTree } from "@/components/list-pane";
import type { MetaItem } from "@/components/meta-toolbar";
import { ExecuteToolIcon } from "@/icons";
import { formatBytes } from "@/lib/composer-attachments";
import {
  WorkspaceContextMenu,
  useWorkspaceDropTarget,
  useWorkspaceEntryDrag,
} from "@/components/workspace-transfers";
import { cn } from "@/lib/utils";

type TreeProps = {
  entries: Record<string, VfsTreeEntry>;
  slug: string;
  workspaceId: string;
  activePath: string | undefined;
};

/// A workspace's files as a list in files mode: one compressed row per file
/// and folder, folders opening and closing in place (their rows hang off
/// tree lines), each with its type's icon and its actions menu. A keyboard
/// tree: the items are a flat list with their level, position and set, one
/// of them focusable at a time; arrows move, open and close, Enter opens.
/// Right-click (or a long press) opens an item's actions at the pointer, and
/// on empty space the workspace's; Shift+F10 or the context-menu key opens
/// the focused item's at it.
export function WorkspaceFileTree(props: TreeProps) {
  const tree = useRef<HTMLUListElement>(null);
  const [focusedPath, setFocusedPath] = useState<string | null>(
    props.activePath ?? null,
  );
  // Folders start closed, so a large workspace renders only its top level;
  // the folders holding the file being viewed open so it stays visible, also
  // when it changes later.
  const [open, setOpen] = useState<Set<string>>(() =>
    ancestors(props.activePath),
  );
  useEffect(() => {
    const holding = ancestors(props.activePath);
    if (holding.size === 0) return;
    setOpen((current) =>
      [...holding].every((path) => current.has(path))
        ? current
        : new Set([...current, ...holding]),
    );
  }, [props.activePath]);
  // The item the context menu was opened on (none: the workspace), lit
  // while the menu is open, and focused again when it closes.
  const [menuFor, setMenuFor] = useState<{ path: string; kind: "workspace" | "folder" | "file" }>({ path: "", kind: "workspace" });
  const [menuOpen, setMenuOpen] = useState(false);
  const menuItem = useRef<HTMLElement | null>(null);
  const aim = (target: EventTarget) => {
    const item = (target as Element).closest<HTMLElement>('[role="treeitem"]');
    menuItem.current = item;
    setMenuFor(
      item?.dataset.treePath
        ? { path: item.dataset.treePath, kind: item.hasAttribute("aria-expanded") ? "folder" : "file" }
        : { path: "", kind: "workspace" },
    );
  };
  const flip = (path: string) =>
    setOpen((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  useEffect(() => {
    const items = [
      ...tree.current!.querySelectorAll<HTMLElement>('[role="treeitem"]'),
    ];
    if (!items.some((item) => item.dataset.treePath === focusedPath)) {
      setFocusedPath(
        items.find((item) => item.dataset.treePath === props.activePath)
          ?.dataset.treePath ??
          items[0]?.dataset.treePath ??
          null,
      );
    }
  }, [props.entries, props.activePath, focusedPath, open]);

  const key = (event: KeyboardEvent<HTMLUListElement>) => {
    const target = event.target as HTMLElement;
    // Menu popups use a portal but still bubble through the React tree.
    if (
      !event.currentTarget.contains(target) ||
      event.altKey ||
      event.ctrlKey ||
      event.metaKey
    )
      return;
    const current = target.closest<HTMLElement>('[role="treeitem"]');
    if (!current) return;
    const items = [
      ...event.currentTarget.querySelectorAll<HTMLElement>('[role="treeitem"]'),
    ];
    const index = items.indexOf(current);
    let next: HTMLElement | undefined;
    const toggle = () =>
      current.querySelector<HTMLElement>("[data-tree-entry]")?.click();
    switch (event.key) {
      case "ArrowDown":
        next = items[Math.min(items.length - 1, index + 1)];
        break;
      case "ArrowUp":
        next = items[Math.max(0, index - 1)];
        break;
      case "Home":
        next = items[0];
        break;
      case "End":
        next = items.at(-1);
        break;
      case "ArrowRight":
        if (current.getAttribute("aria-expanded") === "false") toggle();
        else if (
          current.getAttribute("aria-expanded") === "true" &&
          items[index + 1]?.dataset.treeParent === current.dataset.treePath
        )
          next = items[index + 1];
        break;
      case "ArrowLeft":
        if (current.getAttribute("aria-expanded") === "true") toggle();
        else
          next = items.find(
            (item) => item.dataset.treePath === current.dataset.treeParent,
          );
        break;
      case "Enter":
      case " ":
        toggle();
        break;
      case "F10":
      case "ContextMenu": {
        if (event.key === "F10" && !event.shiftKey) return;
        // The item's context menu, opened at its row as a right-click there
        // would.
        const row = current.getBoundingClientRect();
        current.dispatchEvent(
          new MouseEvent("contextmenu", {
            bubbles: true,
            cancelable: true,
            button: 2,
            clientX: row.left + 24,
            clientY: row.bottom,
          }),
        );
        break;
      }
      default:
        return;
    }
    event.preventDefault();
    event.stopPropagation();
    next?.focus();
  };

  const rows = withNextOpen(visibleEntries(props.entries, open));
  return (
    <ListLayout mode="files" compressed>
      <WorkspaceContextMenu target={menuFor} onOpenChange={setMenuOpen} returnFocus={menuItem}>
      <ul
        ref={tree}
        role="tree"
        aria-label="Workspace files"
        className="min-w-0"
        onKeyDown={key}
        // What a right-click (or the long press before it) lands on.
        onPointerDownCapture={(event) => aim(event.target)}
        onContextMenuCapture={(event) => aim(event.target)}
        onFocus={(event) => {
          if (event.currentTarget.contains(event.target)) {
            const path = (event.target as HTMLElement).closest<HTMLElement>(
              '[role="treeitem"]',
            )?.dataset.treePath;
            if (path) setFocusedPath(path);
          }
        }}
      >
        {rows.map((row) => (
          <EntryRow
            key={row.path}
            {...row}
            slug={props.slug}
            workspaceId={props.workspaceId}
            active={props.activePath === row.path}
            focused={focusedPath === row.path}
            open={open.has(row.path)}
            onFlip={() => flip(row.path)}
            onFocusPath={setFocusedPath}
            menuOpen={menuOpen && menuFor.path === row.path}
          />
        ))}
      </ul>
      </WorkspaceContextMenu>
    </ListLayout>
  );
}

/// One shown file or folder: its path, its parent's, its place in the tree
/// (for the lines and the tree item's level and position).
type VisibleEntry = {
  name: string;
  path: string;
  parent: string;
  directory: boolean;
  entry: VfsTreeEntry;
  tree: ListRowTree;
  position: number;
  siblings: number;
};

/// The rows that show: each entry, folders first and then by name, and the
/// contents of the open folders after them.
function visibleEntries(
  entries: Record<string, VfsTreeEntry>,
  open: Set<string>,
  parent = "",
  depth = 0,
  lines: boolean[] = [],
): VisibleEntry[] {
  const names = Object.keys(entries).sort((a, b) => {
    const aDir = entries[a]!.kind === "directory";
    const bDir = entries[b]!.kind === "directory";
    return aDir !== bDir ? (aDir ? -1 : 1) : a.localeCompare(b);
  });
  return names.flatMap((name, index) => {
    const entry = entries[name]!;
    const path = parent ? `${parent}/${name}` : name;
    const last = index === names.length - 1;
    const shown =
      entry.kind === "directory" && open.has(path) ? entry.entries : {};
    const row: VisibleEntry = {
      name,
      path,
      parent,
      directory: entry.kind === "directory",
      entry,
      tree: { depth, last, lines, children: Object.keys(shown).length > 0 },
      position: index + 1,
      siblings: names.length,
    };
    return [
      row,
      ...visibleEntries(
        shown,
        open,
        path,
        depth + 1,
        depth === 0 ? [] : [...lines, !last],
      ),
    ];
  });
}

/// Marks each row whose next row is an open folder (its block's line).
function withNextOpen(rows: VisibleEntry[]): VisibleEntry[] {
  return rows.map((row, index) => ({
    ...row,
    tree: { ...row.tree, nextOpen: rows[index + 1]?.tree.children ?? false },
  }));
}

/// What a row's meta toolbar says: a file's size (and whether it runs), its
/// exact bytes and media type in the tooltip; a folder's item count, with
/// the files and bytes it holds in all in the tooltip. (The tree has no
/// times per file.)
function entryMarks(entry: VfsTreeEntry): MetaItem[] {
  if (entry.kind === "directory") {
    const items = Object.keys(entry.entries).length;
    const { files, bytes } = folderTotals(entry);
    return [{
      key: "items",
      label: "Contents",
      value: `${items} ${items === 1 ? "item" : "items"}`,
      tooltip: `${files} ${files === 1 ? "file" : "files"} · ${formatBytes(bytes)} in all`,
      priority: 10,
    }];
  }
  const marks: MetaItem[] = [];
  if (entry.executable) {
    marks.push({ key: "executable", label: "Executable", icon: ExecuteToolIcon, tooltip: "Executable", priority: 5 });
  }
  marks.push({
    key: "size",
    label: "Size",
    value: formatBytes(entry.size_bytes),
    tooltip: [`${entry.size_bytes.toLocaleString()} bytes`, entry.media_type].filter(Boolean).join(" · "),
    priority: 10,
  });
  return marks;
}

/// The files and bytes a folder holds, at any depth.
function folderTotals(entry: VfsTreeEntry): { files: number; bytes: number } {
  if (entry.kind === "file") return { files: 1, bytes: entry.size_bytes };
  return Object.values(entry.entries).reduce(
    (total, child) => {
      const inner = folderTotals(child);
      return { files: total.files + inner.files, bytes: total.bytes + inner.bytes };
    },
    { files: 0, bytes: 0 },
  );
}

/// The folders holding a path, outermost first.
function ancestors(path: string | undefined): Set<string> {
  const segments = path?.split("/").slice(0, -1) ?? [];
  return new Set(segments.map((_, index) => segments.slice(0, index + 1).join("/")));
}

function EntryRow({
  name,
  path,
  parent,
  directory,
  entry,
  tree,
  position,
  siblings,
  slug,
  workspaceId,
  active,
  focused,
  open,
  onFlip,
  onFocusPath,
  menuOpen,
}: VisibleEntry & {
  slug: string;
  workspaceId: string;
  active: boolean;
  focused: boolean;
  open: boolean;
  onFlip: () => void;
  onFocusPath: (path: string) => void;
  /** Its context menu is open: the row stays lit. */
  menuOpen: boolean;
}) {
  const drag = useWorkspaceEntryDrag();
  const dropTarget = useWorkspaceDropTarget() === path && directory;
  return (
    <ListRow
      title={name}
      titleHint={name}
      fileName={name}
      marks={entryMarks(entry)}
      tree={tree}
      active={active}
      to={
        directory
          ? undefined
          : `/u/${slug}/workspaces/${workspaceId}/files/${path.split("/").map(encodeURIComponent).join("/")}`
      }
      toggle={directory ? { open, onToggle: onFlip } : undefined}
      // The item is the tree item: focused by the keyboard, labelled, with
      // its level, position and set (the list is flat), and the folder a
      // drop on it lands in: a folder itself, a file its folder (the rows are
      // siblings, so a file cannot rely on its folder around it).
      itemProps={{
        role: "treeitem",
        tabIndex: focused ? 0 : -1,
        "aria-label": name,
        "aria-level": tree.depth + 1,
        "aria-posinset": position,
        "aria-setsize": siblings,
        "aria-expanded": directory ? open : undefined,
        "aria-selected": directory ? undefined : active,
        "data-tree-path": path,
        "data-tree-parent": parent,
        "data-workspace-folder": directory ? path : parent,
        className:
          "outline-none [&:focus-visible>:first-child]:ring-2 [&:focus-visible>:first-child]:ring-inset [&:focus-visible>:first-child]:ring-ring",
      }}
      // The entry is what is clicked and dragged; the item, not it, takes
      // keyboard focus (its expanded state is the item's).
      entryProps={{
        "data-tree-entry": true,
        "data-workspace-drop-target": dropTarget || undefined,
        "aria-expanded": undefined,
        tabIndex: -1,
        draggable: drag.draggable,
        onDragStart: (event) => drag.onDragStart(event, path),
        onClick: () => onFocusPath(path),
        "data-menu-open": menuOpen || undefined,
        className: cn(
          menuOpen && "bg-muted/50",
          dropTarget && "bg-primary/10! ring-2 ring-inset ring-primary/60",
        ),
      }}
    />
  );
}

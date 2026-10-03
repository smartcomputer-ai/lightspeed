import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { NavLink } from "react-router-dom";
import { ChevronRight, File, FolderOpen } from "lucide-react";
import type { VfsTreeEntry } from "@/api";
import {
  WorkspaceActionsMenu,
  useWorkspaceDropTarget,
} from "@/components/workspace-transfers";
import { cn } from "@/lib/utils";

type TreeProps = {
  entries: Record<string, VfsTreeEntry>;
  slug: string;
  workspaceId: string;
  activePath: string | undefined;
};
type EntriesProps = TreeProps & {
  basePath: string;
  focusedPath: string | null;
  onFocusPath: (path: string) => void;
};

export function WorkspaceFileTree(props: TreeProps) {
  const tree = useRef<HTMLUListElement>(null);
  const [focusedPath, setFocusedPath] = useState<string | null>(
    props.activePath ?? null,
  );
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
  }, [props.entries, props.activePath, focusedPath]);

  const key = (event: KeyboardEvent<HTMLUListElement>) => {
    const target = event.target as HTMLElement;
    // Menu popups use a portal but still bubble through the React tree.
    if (
      !event.currentTarget.contains(target) ||
      target.closest("[data-workspace-actions]") ||
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
        if (!event.shiftKey) return;
        current.querySelector<HTMLElement>("[data-workspace-actions]")?.click();
        break;
      default:
        return;
    }
    event.preventDefault();
    event.stopPropagation();
    next?.focus();
  };
  return (
    <ul
      ref={tree}
      role="tree"
      aria-label="Workspace files"
      className="grid min-w-0 grid-cols-1 gap-0.5"
      onKeyDown={key}
      onFocus={(event) => {
        if (event.currentTarget.contains(event.target)) {
          const path = (event.target as HTMLElement).closest<HTMLElement>(
            '[role="treeitem"]',
          )?.dataset.treePath;
          if (path) setFocusedPath(path);
        }
      }}
    >
      <Entries
        {...props}
        basePath=""
        focusedPath={focusedPath}
        onFocusPath={setFocusedPath}
      />
    </ul>
  );
}

function Entries({ entries, basePath, ...props }: EntriesProps) {
  const names = Object.keys(entries).sort((a, b) => {
    const aDir = entries[a]!.kind === "directory";
    const bDir = entries[b]!.kind === "directory";
    return aDir !== bDir ? (aDir ? -1 : 1) : a.localeCompare(b);
  });
  return names.map((name) => {
    const entry = entries[name]!;
    const path = basePath ? `${basePath}/${name}` : name;
    const focused = props.focusedPath === path;
    return entry.kind === "directory" ? (
      <Directory
        key={path}
        {...props}
        name={name}
        path={path}
        basePath={basePath}
        entries={entry.entries}
      />
    ) : (
      <li
        key={path}
        role="treeitem"
        tabIndex={focused ? 0 : -1}
        aria-label={name}
        aria-selected={props.activePath === path}
        data-tree-path={path}
        data-tree-parent={basePath}
        data-active={props.activePath === path || undefined}
        className={cn(
          "group/tree-row relative flex min-h-8 min-w-0 items-center rounded-md hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
          props.activePath === path && "bg-muted",
        )}
      >
        <NavLink
          data-tree-entry
          tabIndex={-1}
          to={`/u/${props.slug}/workspaces/${props.workspaceId}/files/${path.split("/").map(encodeURIComponent).join("/")}`}
          title={name}
          onClick={() => props.onFocusPath(path)}
          className={cn(
            "flex min-w-0 flex-1 items-center gap-1.5 rounded-md px-2 py-1 text-sm [@media(hover:none)]:pr-10 [@media(pointer:coarse)]:pr-10",
            props.activePath === path && "font-medium",
          )}
        >
          <File className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="truncate">{name}</span>
        </NavLink>
        <WorkspaceActionsMenu
          path={path}
          kind="file"
          tabIndex={focused ? 0 : -1}
        />
      </li>
    );
  });
}

function Directory({
  name,
  path,
  basePath,
  entries,
  ...props
}: Omit<EntriesProps, "entries"> & {
  name: string;
  path: string;
  entries: Record<string, VfsTreeEntry>;
}) {
  const [open, setOpen] = useState(true);
  const focused = props.focusedPath === path;
  const dropTarget = useWorkspaceDropTarget() === path;
  return (
    <li
      role="treeitem"
      tabIndex={focused ? 0 : -1}
      aria-label={name}
      aria-expanded={open}
      data-tree-path={path}
      data-tree-parent={basePath}
      data-workspace-folder={path}
      className="min-w-0 focus-visible:outline-none [&:focus-visible>div:first-child]:ring-2 [&:focus-visible>div:first-child]:ring-inset [&:focus-visible>div:first-child]:ring-ring"
    >
      <div
        data-workspace-drop-target={dropTarget || undefined}
        className={cn(
          "group/tree-row relative flex min-h-8 min-w-0 items-center rounded-md hover:bg-muted/50",
          dropTarget && "bg-primary/10! ring-2 ring-inset ring-primary/60",
        )}
      >
        <button
          data-tree-entry
          type="button"
          tabIndex={-1}
          className="flex min-w-0 flex-1 items-center gap-1.5 rounded-md px-2 py-1 text-sm [@media(hover:none)]:pr-10 [@media(pointer:coarse)]:pr-10"
          onClick={() => {
            props.onFocusPath(path);
            setOpen((value) => !value);
          }}
        >
          <ChevronRight
            className={cn(
              "size-3.5 shrink-0 transition-transform",
              open && "rotate-90",
            )}
          />
          <FolderOpen className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="truncate">{name}</span>
        </button>
        <WorkspaceActionsMenu
          path={path}
          kind="folder"
          tabIndex={focused ? 0 : -1}
        />
      </div>
      {open && (
        <div className="min-w-0 pl-4">
          <ul role="group" className="grid min-w-0 grid-cols-1 gap-0.5">
            <Entries {...props} entries={entries} basePath={path} />
          </ul>
        </div>
      )}
    </li>
  );
}

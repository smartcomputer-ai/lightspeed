import {
  createContext,
  useContext,
  useRef,
  useState,
  type ReactNode,
  type DragEvent,
} from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  SquarePen,
  Download,
  Ellipsis,
  LoaderCircle,
  FilePlus,
  FolderPlus,
  FolderUp,
  Trash2,
  Upload,
  Pencil,
} from "lucide-react";
import {
  validWorkspaceTransferPath,
  workspaceUploadConflicts,
  renameWorkspaceEntry,
} from "@lightspeed/platform-shared";
import { api, ApiError, type WorkspaceTree } from "@/api";
import { useActionPermissions } from "@/lib/permissions";
import {
  directoryEntries,
  droppedEntries,
  selectedFiles,
  uploadPayload,
  validateUploadEntries,
  workspaceBaseUrl,
  type UploadDirectoryHandle,
  type UploadEntry,
} from "@/lib/workspace-transfers";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";

type UploadReview = {
  entries: UploadEntry[];
  conflicts: string[];
  revision?: number;
  error?: string;
};
type DeleteReview = {
  path: string;
  directory: boolean;
  revision: number;
  error?: string;
};
const Transfers = createContext<{
  choose: (folder: boolean, target: string, replacement?: boolean) => void;
  download: (path: string, directory: boolean) => void;
  remove: (path: string, directory: boolean) => void;
  newFolder: (parent: string) => void;
  newFile: (parent: string) => void;
  rename: (path: string, directory: boolean) => void;
  dragTarget: string | null;
  canUpload: boolean;
  busy: boolean;
  downloading: boolean;
  activity: string | null;
} | null>(null);

export function WorkspaceTransfers({
  universeId,
  workspaceId,
  onRemoved,
  onRenamed,
  onNewFile,
  children,
}: {
  universeId: string;
  workspaceId?: string;
  onRemoved?: (path: string) => void;
  onRenamed?: (from: string, to: string) => void;
  onNewFile?: (parent: string, revision: number) => void;
  children: ReactNode;
}) {
  const permissions = useActionPermissions(universeId);
  const queryClient = useQueryClient();
  const baseUrl = workspaceBaseUrl(universeId, workspaceId ?? "");
  const treeKey = ["workspace-tree", universeId, workspaceId];
  const tree = useQuery({
    queryKey: treeKey,
    queryFn: () => api<WorkspaceTree>("GET", `${baseUrl}/tree`),
    enabled: !!workspaceId,
  });
  const canUpload =
    !!workspaceId && !!tree.data && permissions.can("use_resource");
  const filesInput = useRef<HTMLInputElement>(null);
  const folderInput = useRef<HTMLInputElement>(null);
  const replacementInput = useRef<HTMLInputElement>(null);
  const target = useRef("");
  const dragDepth = useRef(0);
  const uploadLock = useRef(false);
  const [dragTarget, setDragTarget] = useState<string | null>(null);
  const [review, setReview] = useState<UploadReview | null>(null);
  const [deletion, setDeletion] = useState<DeleteReview | null>(null);
  const [renaming, setRenaming] = useState<{
    path: string;
    directory: boolean;
    name: string;
    snapshot: WorkspaceTree;
    error?: string;
  } | null>(null);
  const [folder, setFolder] = useState<{
    parent: string;
    name: string;
    error?: string;
  } | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [downloading, setDownloading] = useState(false);
  const invalidate = () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: treeKey }),
      queryClient.invalidateQueries({
        queryKey: ["workspace-file", universeId, workspaceId],
      }),
      queryClient.invalidateQueries({ queryKey: ["workspaces", universeId] }),
    ]);

  // Paths are fixed by the invoked menu or drop target. Only collisions and
  // failures need a dialog; a normal upload goes straight to one atomic commit.
  const upload = async (
    readEntries: () => Promise<UploadEntry[]>,
    approvedRevision?: number,
  ) => {
    if (!canUpload || uploadLock.current) return;
    uploadLock.current = true;
    setBusy(true);
    setProgress("Preparing upload…");
    setError(null);
    let entries: UploadEntry[] = [];
    let attempted = false;
    try {
      entries = await readEntries();
      validateUploadEntries(entries);
      let revision = approvedRevision;
      if (revision === undefined) {
        const latest = await api<WorkspaceTree>("GET", `${baseUrl}/tree`);
        queryClient.setQueryData(treeKey, latest);
        revision = latest.workspace.revision;
        const conflicts = workspaceUploadConflicts(latest.manifest, entries);
        if (conflicts.length) {
          setReview({ entries, conflicts, revision });
          return;
        }
      }
      const body = await uploadPayload(
        entries,
        "",
        revision,
        approvedRevision !== undefined,
        (done) => setProgress(`Preparing ${done} of ${entries.length}…`),
      );
      setProgress("Uploading…");
      attempted = true;
      await api("POST", `${baseUrl}/upload`, body);
      setReview(null);
    } catch (error) {
      if ((error as Error).name !== "AbortError") {
        setReview({
          entries,
          conflicts: [],
          error:
            error instanceof ApiError && error.status === 409
              ? `${error.message} Retry to check the latest files before continuing.`
              : (error as Error).message,
        });
      }
    } finally {
      if (attempted) await invalidate();
      uploadLock.current = false;
      setBusy(false);
    }
  };
  const into = (entries: UploadEntry[], path: string) =>
    entries.map((entry) => ({
      ...entry,
      path: path ? `${path}/${entry.path}` : entry.path,
    }));
  const choose = (folder: boolean, path: string, replacement = false) => {
    if (!canUpload || uploadLock.current) return;
    const picker = (
      window as unknown as {
        showDirectoryPicker?: (options: {
          mode: "read";
        }) => Promise<UploadDirectoryHandle>;
      }
    ).showDirectoryPicker;
    if (folder && picker) {
      // Invoke the picker during the click's user activation, before awaiting.
      const selection = picker.call(window, { mode: "read" });
      void upload(async () =>
        into(await directoryEntries(await selection), path),
      );
      return;
    }
    target.current = path;
    (replacement
      ? replacementInput
      : folder
        ? folderInput
        : filesInput
    ).current?.click();
  };
  const dropTarget = (event: DragEvent) =>
    (event.target as Element).closest<HTMLElement>("[data-workspace-folder]")
      ?.dataset.workspaceFolder ?? "";
  const drop = (event: DragEvent) => {
    if (!event.dataTransfer.types.includes("Files")) return;
    event.preventDefault();
    dragDepth.current = 0;
    setDragTarget(null);
    if (
      !canUpload ||
      uploadLock.current ||
      review ||
      deletion ||
      folder ||
      renaming
    )
      return;
    const path = dropTarget(event);
    // Capture browser drag entries before the event's data store is cleared.
    const entries = droppedEntries(event.dataTransfer);
    void upload(async () => into(await entries, path));
  };
  const confirmDelete = async () => {
    if (!deletion || uploadLock.current) return;
    uploadLock.current = true;
    setBusy(true);
    setProgress("Deleting…");
    try {
      await api(
        "DELETE",
        `${baseUrl}/entries?${new URLSearchParams({ path: deletion.path, expectedRevision: String(deletion.revision) })}`,
      );
      onRemoved?.(deletion.path);
      setDeletion(null);
    } catch (error) {
      setDeletion({ ...deletion, error: (error as Error).message });
    } finally {
      await invalidate();
      uploadLock.current = false;
      setBusy(false);
    }
  };
  const createFolder = async () => {
    if (!folder || !canUpload || uploadLock.current) return;
    const name = folder.name.trim();
    if (!validWorkspaceTransferPath(name) || name.includes("/")) {
      setFolder({
        ...folder,
        error:
          "Enter a folder name without slashes or control characters. The names “.” and “..” aren’t allowed.",
      });
      return;
    }
    uploadLock.current = true;
    setBusy(true);
    setProgress("Creating folder…");
    let attempted = false;
    try {
      const latest = await api<WorkspaceTree>("GET", `${baseUrl}/tree`);
      queryClient.setQueryData(treeKey, latest);
      let entries = latest.manifest.root.entries;
      for (const segment of folder.parent ? folder.parent.split("/") : []) {
        const entry = Object.hasOwn(entries, segment)
          ? entries[segment]
          : undefined;
        if (entry?.kind !== "directory")
          throw new Error("The parent folder no longer exists.");
        entries = entry.entries;
      }
      if (Object.hasOwn(entries, name))
        throw new Error("A file or folder with this name already exists.");
      attempted = true;
      await api("POST", `${baseUrl}/upload`, {
        expectedRevision: latest.workspace.revision,
        replace: false,
        entries: [
          {
            kind: "directory",
            path: folder.parent ? `${folder.parent}/${name}` : name,
          },
        ],
      });
      setFolder(null);
    } catch (error) {
      setFolder({ ...folder, error: (error as Error).message });
    } finally {
      if (attempted) await invalidate();
      uploadLock.current = false;
      setBusy(false);
    }
  };
  const confirmRename = async () => {
    if (!renaming || !canUpload || uploadLock.current) return;
    uploadLock.current = true;
    setBusy(true);
    setProgress("Renaming…");
    let attempted = false;
    try {
      const { path, name, snapshot } = renaming;
      const manifest = renameWorkspaceEntry(snapshot.manifest, path, name);
      attempted = true;
      const result = await api<Pick<WorkspaceTree, "workspace">>(
        "POST",
        `${baseUrl}/rename`,
        {
          path,
          name,
          expectedRevision: snapshot.workspace.revision,
        },
      );
      const destination = [...path.split("/").slice(0, -1), name].join("/");
      // Keep the open editor's content available while its URL changes.
      for (const [key, data] of queryClient.getQueriesData({
        queryKey: ["workspace-file", universeId, workspaceId],
      })) {
        const oldPath = key[3];
        if (
          typeof oldPath === "string" &&
          (oldPath === path || oldPath.startsWith(`${path}/`))
        ) {
          queryClient.setQueryData(
            [
              ...key.slice(0, 3),
              destination + oldPath.slice(path.length),
              ...key.slice(4),
            ],
            data,
          );
        }
      }
      queryClient.setQueryData(treeKey, {
        ...snapshot,
        workspace: result.workspace,
        manifest,
      });
      onRenamed?.(path, destination);
      setRenaming(null);
    } catch (error) {
      setRenaming({ ...renaming, error: (error as Error).message });
    } finally {
      if (attempted) await invalidate();
      uploadLock.current = false;
      setBusy(false);
    }
  };
  const download = async (path: string, directory: boolean) => {
    if (downloading) return;
    setDownloading(true);
    setError(null);
    try {
      const response = await fetch(
        `${baseUrl}/download?${new URLSearchParams({ path })}`,
        { credentials: "same-origin" },
      );
      if (!response.ok)
        throw new ApiError(
          response.status,
          await response.json().catch(() => null),
        );
      const url = URL.createObjectURL(await response.blob());
      const link = document.createElement("a");
      link.href = url;
      link.download = `${path.split("/").at(-1) || workspaceId}${directory ? ".zip" : ""}`;
      document.body.append(link);
      link.click();
      link.remove();
      window.setTimeout(() => URL.revokeObjectURL(url), 60_000);
    } catch (error) {
      setError(`Download failed: ${(error as Error).message}`);
    } finally {
      setDownloading(false);
    }
  };
  const selected = (files: FileList | null, replacement = false) => {
    if (!files?.length) return;
    const path = target.current;
    const entries = replacement
      ? [{ kind: "file" as const, path, file: files[0]! }]
      : into(selectedFiles(files), path);
    void upload(async () => entries);
  };
  return (
    <Transfers.Provider
      value={{
        choose,
        download,
        dragTarget,
        newFolder: (parent) => {
          if (canUpload && !busy) setFolder({ parent, name: "" });
        },
        newFile: (parent) => {
          if (canUpload && !busy && tree.data)
            onNewFile?.(parent, tree.data.workspace.revision);
        },
        rename: (path, directory) => {
          if (canUpload && !busy && tree.data)
            setRenaming({
              path,
              directory,
              name: path.split("/").at(-1)!,
              snapshot: tree.data,
            });
        },
        canUpload,
        busy,
        downloading,
        activity: busy ? progress : downloading ? "Preparing download…" : null,
        remove: (path, directory) => {
          if (canUpload && !busy && tree.data) {
            setError(null);
            setDeletion({
              path,
              directory,
              revision: tree.data.workspace.revision,
            });
          }
        },
      }}
    >
      <div
        className="relative flex min-h-0 flex-1 flex-col"
        onDragEnter={(event) => {
          if (event.dataTransfer.types.includes("Files")) {
            event.preventDefault();
            dragDepth.current++;
            if (
              canUpload &&
              !busy &&
              !review &&
              !deletion &&
              !folder &&
              !renaming
            )
              setDragTarget(dropTarget(event));
          }
        }}
        onDragOver={(event) => {
          if (event.dataTransfer.types.includes("Files")) {
            event.preventDefault();
            const allowed =
              canUpload &&
              !busy &&
              !review &&
              !deletion &&
              !folder &&
              !renaming;
            event.dataTransfer.dropEffect = allowed ? "copy" : "none";
            setDragTarget(allowed ? dropTarget(event) : null);
          }
        }}
        onDragLeave={() => {
          dragDepth.current = Math.max(0, dragDepth.current - 1);
          if (!dragDepth.current) setDragTarget(null);
        }}
        onDrop={drop}
        onDragEnd={() => {
          dragDepth.current = 0;
          setDragTarget(null);
        }}
      >
        {children}
        {dragTarget !== null && (
          <div className="pointer-events-none absolute inset-x-3 bottom-3 z-20 flex justify-center">
            <div
              role="status"
              className="flex min-w-0 max-w-full items-center gap-2 rounded-lg border border-primary/40 bg-popover px-3 py-2 text-sm text-popover-foreground shadow-md"
            >
              <FolderUp className="size-4 shrink-0 text-primary" />
              <span className="min-w-0 break-words">
                Drop to upload into{" "}
                <span className="font-medium [overflow-wrap:anywhere]">
                  {dragTarget ? `/${dragTarget}` : "workspace root /"}
                </span>
              </span>
            </div>
          </div>
        )}
        <input
          ref={filesInput}
          type="file"
          multiple
          hidden
          aria-label="Select files to upload"
          onChange={(event) => {
            selected(event.target.files);
            event.target.value = "";
          }}
        />
        <input
          ref={folderInput}
          type="file"
          multiple
          {...{ webkitdirectory: "" }}
          hidden
          aria-label="Select folder to upload"
          onChange={(event) => {
            selected(event.target.files);
            event.target.value = "";
          }}
        />
        <input
          ref={replacementInput}
          type="file"
          hidden
          aria-label="Select replacement file"
          onChange={(event) => {
            selected(event.target.files, true);
            event.target.value = "";
          }}
        />
        <Dialog
          open={!!renaming}
          onOpenChange={(open) => {
            if (!open && !busy) setRenaming(null);
          }}
        >
          <DialogContent>
            <form
              className="grid gap-4"
              onSubmit={(event) => {
                event.preventDefault();
                void confirmRename();
              }}
            >
              <DialogHeader>
                <DialogTitle>
                  Rename {renaming?.directory ? "folder" : "file"}
                </DialogTitle>
                <DialogDescription>
                  Choose a new name for “{renaming?.path}”.
                </DialogDescription>
              </DialogHeader>
              <label className="grid gap-2 text-sm">
                {renaming?.directory ? "Folder name" : "File name"}
                <Input
                  autoFocus
                  required
                  value={renaming?.name ?? ""}
                  disabled={busy}
                  onFocus={(event) => {
                    const input = event.currentTarget;
                    const extension = input.value.lastIndexOf(".");
                    input.setSelectionRange(
                      0,
                      !renaming?.directory && extension > 0
                        ? extension
                        : input.value.length,
                    );
                  }}
                  onChange={(event) => {
                    if (renaming)
                      setRenaming({
                        ...renaming,
                        name: event.target.value,
                        error: undefined,
                      });
                  }}
                />
              </label>
              {renaming?.error && (
                <p role="alert" className="text-sm text-destructive">
                  {renaming.error}
                </p>
              )}
              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  disabled={busy}
                  onClick={() => setRenaming(null)}
                >
                  Cancel
                </Button>
                <Button
                  type="submit"
                  disabled={
                    busy ||
                    !renaming?.name.trim() ||
                    renaming.name === renaming.path.split("/").at(-1)
                  }
                >
                  {busy ? "Renaming…" : "Rename"}
                </Button>
              </DialogFooter>
            </form>
          </DialogContent>
        </Dialog>
        <Dialog
          open={!!folder}
          onOpenChange={(open) => {
            if (!open && !busy) setFolder(null);
          }}
        >
          <DialogContent>
            <form
              className="grid gap-4"
              onSubmit={(event) => {
                event.preventDefault();
                void createFolder();
              }}
            >
              <DialogHeader>
                <DialogTitle>New folder</DialogTitle>
                <DialogDescription>
                  Create a folder in {folder?.parent || "the workspace root"}.
                </DialogDescription>
              </DialogHeader>
              <label className="grid gap-2 text-sm">
                Folder name
                <Input
                  autoFocus
                  required
                  value={folder?.name ?? ""}
                  disabled={busy}
                  onChange={(event) => {
                    if (folder)
                      setFolder({
                        ...folder,
                        name: event.target.value,
                        error: undefined,
                      });
                  }}
                />
              </label>
              {folder?.error && (
                <p role="alert" className="text-sm text-destructive">
                  {folder.error}
                </p>
              )}
              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  disabled={busy}
                  onClick={() => setFolder(null)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={busy || !folder?.name.trim()}>
                  {busy ? "Creating…" : "Create folder"}
                </Button>
              </DialogFooter>
            </form>
          </DialogContent>
        </Dialog>
        <Dialog
          open={!!error}
          onOpenChange={(open) => {
            if (!open) setError(null);
          }}
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Download couldn’t finish</DialogTitle>
              <DialogDescription>{error}</DialogDescription>
            </DialogHeader>
            <DialogFooter>
              <Button onClick={() => setError(null)}>Close</Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
        <Dialog
          open={!!review}
          onOpenChange={(open) => {
            if (!open && !busy) setReview(null);
          }}
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>
                {review?.conflicts.length
                  ? "Replace existing files?"
                  : "Upload couldn’t finish"}
              </DialogTitle>
              <DialogDescription>
                {review?.conflicts.length
                  ? `${review.conflicts.length} ${review.conflicts.length === 1 ? "file already exists" : "files already exist"}. Replace them, or skip them and upload the remaining items. Nothing has been uploaded yet.`
                  : "Review the error before trying again. Retrying checks the latest workspace contents first."}
              </DialogDescription>
            </DialogHeader>
            {!!review?.conflicts.length && (
              <ul className="max-h-48 overflow-auto break-all text-sm">
                {review.conflicts.map((path) => (
                  <li key={path}>{path}</li>
                ))}
              </ul>
            )}
            {review?.error && (
              <p role="alert" className="text-sm text-destructive">
                {review.error}
              </p>
            )}
            <DialogFooter>
              <Button
                variant="outline"
                disabled={busy}
                onClick={() => setReview(null)}
              >
                {review?.error ? "Close" : "Cancel"}
              </Button>
              {!!review?.conflicts.length && (
                <Button
                  variant="outline"
                  disabled={busy}
                  onClick={() => {
                    const paths = new Set(review.conflicts);
                    const remaining = review.entries.filter(
                      (entry) => !paths.has(entry.path),
                    );
                    setReview(null);
                    if (remaining.length) void upload(async () => remaining);
                  }}
                >
                  Skip existing
                </Button>
              )}
              {!!review?.entries.length && (
                <Button
                  disabled={busy}
                  onClick={() =>
                    void upload(
                      async () => review.entries,
                      review.conflicts.length ? review.revision : undefined,
                    )
                  }
                >
                  {busy
                    ? progress
                    : review.conflicts.length
                      ? "Replace and upload"
                      : "Retry"}
                </Button>
              )}
            </DialogFooter>
          </DialogContent>
        </Dialog>
        <Dialog
          open={!!deletion}
          onOpenChange={(open) => {
            if (!open && !busy) setDeletion(null);
          }}
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>
                Delete {deletion?.directory ? "folder" : "file"}?
              </DialogTitle>
              <DialogDescription>
                {deletion?.directory
                  ? `“${deletion.path}” and all files and folders inside it will be removed from this workspace.`
                  : `“${deletion?.path}” will be removed from this workspace.`}
              </DialogDescription>
            </DialogHeader>
            {deletion?.error && (
              <p role="alert" className="text-sm text-destructive">
                {deletion.error}
              </p>
            )}
            <DialogFooter>
              <Button
                variant="outline"
                disabled={busy}
                onClick={() => setDeletion(null)}
              >
                {deletion?.error ? "Close" : "Cancel"}
              </Button>
              {!deletion?.error && (
                <Button
                  variant="destructive"
                  disabled={busy}
                  onClick={() => void confirmDelete()}
                >
                  {busy
                    ? "Deleting…"
                    : deletion?.directory
                      ? "Delete folder"
                      : "Delete file"}
                </Button>
              )}
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </div>
    </Transfers.Provider>
  );
}

export function useWorkspaceDropTarget() {
  return useContext(Transfers)?.dragTarget ?? null;
}

export function WorkspaceDropArea({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  const active = useWorkspaceDropTarget() === "";
  return (
    <div
      data-workspace-root
      data-workspace-drop-target={active || undefined}
      className={cn(
        className,
        active && "bg-primary/5 ring-2 ring-inset ring-primary/60",
      )}
    >
      {children}
    </div>
  );
}

export function WorkspaceActionsMenu({
  path = "",
  kind,
  disabled = false,
  tabIndex,
  fileCount,
}: {
  path?: string;
  kind: "workspace" | "folder" | "file";
  disabled?: boolean;
  tabIndex?: number;
  fileCount?: number;
}) {
  const transfers = useContext(Transfers);
  if (!transfers) return null;
  const directory = kind !== "file";
  const label =
    kind === "workspace"
      ? "Workspace actions"
      : `${kind === "folder" ? "Folder" : "File"} actions: ${path}`;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={label}
            data-workspace-actions
            tabIndex={tabIndex}
            title={label}
            className={cn(
              "shrink-0 text-muted-foreground",
              kind !== "workspace" &&
                "absolute right-0 top-1/2 -translate-y-1/2 bg-background group-hover/tree-row:bg-[color-mix(in_oklch,var(--muted)_50%,var(--background))] group-data-[active=true]/tree-row:bg-muted hover:bg-[color-mix(in_oklch,var(--foreground)_15%,var(--background))]! aria-expanded:bg-[color-mix(in_oklch,var(--foreground)_15%,var(--background))]! [@media(hover:hover)_and_(pointer:fine)]:opacity-0 group-hover/tree-row:opacity-100! data-popup-open:opacity-100! focus-visible:opacity-100!",
            )}
          />
        }
      >
        {kind === "workspace" ? (
          <>
            {transfers.activity ? (
              <>
                <LoaderCircle className="animate-spin" />
                <span className="sr-only" role="status">
                  {transfers.activity}
                </span>
              </>
            ) : (
              <SquarePen />
            )}
          </>
        ) : (
          <Ellipsis />
        )}
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-56">
        {transfers.canUpload && (
          <>
            {directory && (
              <DropdownMenuItem
                disabled={transfers.busy || disabled}
                onClick={() => transfers.newFile(path)}
              >
                <FilePlus />
                New file
              </DropdownMenuItem>
            )}
            {directory && (
              <DropdownMenuItem
                disabled={transfers.busy || disabled}
                onClick={() => transfers.newFolder(path)}
              >
                <FolderPlus />
                New folder
              </DropdownMenuItem>
            )}
            <DropdownMenuItem
              disabled={transfers.busy || disabled}
              onClick={() => transfers.choose(false, path, !directory)}
            >
              <Upload />
              {directory ? "Upload files" : "Upload replacement…"}
            </DropdownMenuItem>
            {directory && (
              <DropdownMenuItem
                disabled={transfers.busy || disabled}
                onClick={() => transfers.choose(true, path)}
              >
                <FolderUp />
                Upload folder
              </DropdownMenuItem>
            )}
          </>
        )}
        <DropdownMenuItem
          disabled={transfers.downloading}
          onClick={() => transfers.download(path, directory)}
        >
          <Download />
          {directory ? "Download as ZIP" : "Download"}
        </DropdownMenuItem>
        {path && transfers.canUpload && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              disabled={transfers.busy || disabled}
              onClick={() => transfers.rename(path, directory)}
            >
              <Pencil />
              Rename…
            </DropdownMenuItem>
            <DropdownMenuItem
              variant="destructive"
              disabled={transfers.busy || disabled}
              onClick={() => transfers.remove(path, directory)}
            >
              <Trash2 />
              {directory ? "Delete folder…" : "Delete file…"}
            </DropdownMenuItem>
          </>
        )}
        {kind === "workspace" && fileCount !== undefined && (
          <>
            <DropdownMenuSeparator />
            <dl
              aria-label="Workspace information"
              className="px-2 py-1.5 text-xs text-muted-foreground"
            >
              <div className="flex items-center justify-between gap-4">
                <dt>Files</dt>
                <dd>{fileCount.toLocaleString()}</dd>
              </div>
            </dl>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

import { useEffect, useMemo, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { NavLink, useNavigate, useParams } from "react-router-dom";
import { slugify, workspaceCreateSchema } from "@lightspeed/platform-shared";
import {
  ChevronRight,
  File,
  FilePlus,
  FolderGit2,
  FolderOpen,
  Plus,
  Trash2,
} from "lucide-react";
import {
  api,
  type BlobContent,
  type VfsFileEntry,
  type VfsTreeEntry,
  type WorkspaceRow,
  type WorkspaceTree,
} from "@/api";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { LoadingNote, UniverseNotFound } from "@/components/page";
import { canManage, useActiveUniverse } from "@/lib/universes";
import { cn } from "@/lib/utils";

/// U4b: workspace explorer + functional editor. Pane = workspace picker +
/// file tree of the head snapshot; detail = file editor (text), preview
/// (images), or metadata (binary). Writes run the full VFS dance
/// server-side (blob → manifest → snapshot → head advance) guarded by the
/// workspace revision the tree was loaded at.
export function WorkspacesPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const params = useParams<{ workspaceId: string; "*": string }>();
  const workspaceId = params.workspaceId;
  const filePath = params["*"] || undefined;

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !canManage(universe, admin)) {
    return (
      <div className="p-6">
        <UniverseNotFound slug={slug} />
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1">
      <aside
        className={cn(
          "w-full shrink-0 flex-col border-r md:flex md:w-80",
          filePath ? "hidden" : "flex",
        )}
      >
        <WorkspacePane
          universeId={universe.id}
          slug={slug!}
          workspaceId={workspaceId}
          filePath={filePath}
        />
      </aside>
      <section
        className={cn("min-w-0 flex-1 flex-col", filePath ? "flex" : "hidden md:flex")}
      >
        {workspaceId && filePath ? (
          <FileDetail
            universeId={universe.id}
            slug={slug!}
            workspaceId={workspaceId}
            filePath={filePath}
          />
        ) : (
          <div className="flex flex-1 items-center justify-center p-6 text-sm text-muted-foreground">
            {workspaceId
              ? "Select a file, or create one."
              : "Select a workspace, or create one."}
          </div>
        )}
      </section>
    </div>
  );
}

function WorkspacePane({
  universeId,
  slug,
  workspaceId,
  filePath,
}: {
  universeId: string;
  slug: string;
  workspaceId: string | undefined;
  filePath: string | undefined;
}) {
  const navigate = useNavigate();
  const workspaces = useQuery({
    queryKey: ["workspaces", universeId],
    queryFn: () =>
      api<WorkspaceRow[]>("GET", `/api/v1/universes/${universeId}/workspaces`),
  });
  const tree = useQuery({
    queryKey: ["workspace-tree", universeId, workspaceId],
    queryFn: () =>
      api<WorkspaceTree>(
        "GET",
        `/api/v1/universes/${universeId}/workspaces/${workspaceId}/tree`,
      ),
    enabled: workspaceId !== undefined,
  });
  const [createOpen, setCreateOpen] = useState(false);
  const [newFileOpen, setNewFileOpen] = useState(false);

  // Auto-select the first workspace when landing on bare /workspaces.
  useEffect(() => {
    if (!workspaceId && workspaces.data?.[0]) {
      navigate(`/u/${slug}/workspaces/${workspaces.data[0].workspaceId}`, {
        replace: true,
      });
    }
  }, [workspaceId, workspaces.data, navigate, slug]);

  return (
    <>
      <div className="flex h-12 shrink-0 items-center gap-2 border-b px-4">
        <h1 className="text-sm font-semibold">Workspaces</h1>
        <Button
          variant="ghost"
          size="icon-sm"
          className="ml-auto"
          onClick={() => setCreateOpen(true)}
          aria-label="New workspace"
        >
          <Plus />
        </Button>
      </div>
      <div className="grid gap-2 border-b p-3">
        {workspaces.data && workspaces.data.length > 0 ? (
          <Select
            value={workspaceId ?? ""}
            onValueChange={(value) => navigate(`/u/${slug}/workspaces/${value}`)}
          >
            <SelectTrigger className="w-full" aria-label="Workspace">
              <FolderGit2 className="size-4 shrink-0 text-muted-foreground" />
              <SelectValue>
                {(value: string) =>
                  workspaces.data?.find((w) => w.workspaceId === value)?.displayName ??
                  value
                }
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              {workspaces.data.map((workspace) => (
                <SelectItem key={workspace.workspaceId} value={workspace.workspaceId}>
                  {workspace.displayName ?? workspace.workspaceId}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        ) : (
          <p className="text-sm text-muted-foreground">
            {workspaces.isLoading ? "Loading…" : "No workspaces yet."}
          </p>
        )}
        {tree.data && (
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            {/* The id is what profile workspace links reference — keep it in sight. */}
            <code
              className="cursor-pointer rounded bg-muted px-1.5 py-0.5 font-mono hover:bg-accent"
              title="Workspace id (used by profile workspace links) — click to copy"
              onClick={() =>
                void navigator.clipboard.writeText(tree.data.workspace.workspaceId)
              }
            >
              {tree.data.workspace.workspaceId}
            </code>
            <span>
              {tree.data.workspace.files} file{tree.data.workspace.files === 1 ? "" : "s"} ·
              r{tree.data.workspace.revision}
            </span>
            <Button
              variant="ghost"
              size="xs"
              className="ml-auto"
              onClick={() => setNewFileOpen(true)}
            >
              <FilePlus data-icon="inline-start" />
              New file
            </Button>
          </div>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto p-2">
        {tree.error && (
          <p className="p-2 text-sm text-destructive">{tree.error.message}</p>
        )}
        {tree.data && Object.keys(tree.data.manifest.root.entries).length === 0 && (
          <p className="p-2 text-sm text-muted-foreground">
            Empty workspace — create a file to get started.
          </p>
        )}
        {tree.data && (
          <DirectoryEntries
            entries={tree.data.manifest.root.entries}
            basePath=""
            slug={slug}
            workspaceId={workspaceId!}
            activePath={filePath}
          />
        )}
      </div>
      <NewWorkspaceDialog
        universeId={universeId}
        slug={slug}
        open={createOpen}
        onOpenChange={setCreateOpen}
      />
      {workspaceId && tree.data && (
        <NewFileDialog
          universeId={universeId}
          slug={slug}
          workspaceId={workspaceId}
          revision={tree.data.workspace.revision}
          open={newFileOpen}
          onOpenChange={setNewFileOpen}
        />
      )}
    </>
  );
}

function DirectoryEntries({
  entries,
  basePath,
  slug,
  workspaceId,
  activePath,
}: {
  entries: Record<string, VfsTreeEntry>;
  basePath: string;
  slug: string;
  workspaceId: string;
  activePath: string | undefined;
}) {
  const names = Object.keys(entries).sort((a, b) => {
    const aDir = entries[a]!.kind === "directory";
    const bDir = entries[b]!.kind === "directory";
    if (aDir !== bDir) {
      return aDir ? -1 : 1;
    }
    return a.localeCompare(b);
  });
  return (
    <ul className="grid gap-0.5">
      {names.map((name) => {
        const entry = entries[name]!;
        const path = basePath ? `${basePath}/${name}` : name;
        return entry.kind === "directory" ? (
          <DirectoryNode
            key={path}
            name={name}
            entry={entry}
            path={path}
            slug={slug}
            workspaceId={workspaceId}
            activePath={activePath}
          />
        ) : (
          <li key={path}>
            <NavLink
              to={`/u/${slug}/workspaces/${workspaceId}/files/${path}`}
              className={cn(
                "flex items-center gap-1.5 rounded-md px-2 py-1 text-sm hover:bg-muted/50",
                activePath === path && "bg-muted font-medium",
              )}
            >
              <File className="size-3.5 shrink-0 text-muted-foreground" />
              <span className="truncate">{name}</span>
              <span className="ml-auto shrink-0 text-xs text-muted-foreground">
                {formatBytes(entry.size_bytes)}
              </span>
            </NavLink>
          </li>
        );
      })}
    </ul>
  );
}

function DirectoryNode({
  name,
  entry,
  path,
  slug,
  workspaceId,
  activePath,
}: {
  name: string;
  entry: { kind: "directory"; entries: Record<string, VfsTreeEntry> };
  path: string;
  slug: string;
  workspaceId: string;
  activePath: string | undefined;
}) {
  const [open, setOpen] = useState(true);
  return (
    <li>
      <button
        type="button"
        className="flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-sm hover:bg-muted/50"
        onClick={() => setOpen((v) => !v)}
      >
        <ChevronRight
          className={cn("size-3.5 shrink-0 transition-transform", open && "rotate-90")}
        />
        <FolderOpen className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="truncate">{name}</span>
      </button>
      {open && (
        <div className="pl-4">
          <DirectoryEntries
            entries={entry.entries}
            basePath={path}
            slug={slug}
            workspaceId={workspaceId}
            activePath={activePath}
          />
        </div>
      )}
    </li>
  );
}

function findFile(
  entries: Record<string, VfsTreeEntry>,
  path: string,
): VfsFileEntry | null {
  const segments = path.split("/");
  let current = entries;
  for (let i = 0; i < segments.length; i++) {
    const entry = current[segments[i]!];
    if (!entry) {
      return null;
    }
    if (i === segments.length - 1) {
      return entry.kind === "file" ? entry : null;
    }
    if (entry.kind !== "directory") {
      return null;
    }
    current = entry.entries;
  }
  return null;
}

function FileDetail({
  universeId,
  slug,
  workspaceId,
  filePath,
}: {
  universeId: string;
  slug: string;
  workspaceId: string;
  filePath: string;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const tree = useQuery({
    queryKey: ["workspace-tree", universeId, workspaceId],
    queryFn: () =>
      api<WorkspaceTree>(
        "GET",
        `/api/v1/universes/${universeId}/workspaces/${workspaceId}/tree`,
      ),
  });
  const file = tree.data ? findFile(tree.data.manifest.root.entries, filePath) : null;
  const blob = useQuery({
    queryKey: ["blob", universeId, file?.blob_ref],
    queryFn: () =>
      api<BlobContent>(
        "GET",
        `/api/v1/universes/${universeId}/blobs/${encodeURIComponent(file!.blob_ref)}`,
      ),
    enabled: !!file,
  });

  const decoded = useMemo(() => {
    if (!blob.data) {
      return null;
    }
    const bytes = Uint8Array.from(atob(blob.data.bytesBase64), (c) => c.charCodeAt(0));
    return { bytes, kind: classify(filePath, file?.media_type, bytes) };
  }, [blob.data, file?.media_type, filePath]);

  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Reset editor state when switching files or after a reload.
  useEffect(() => {
    setDraft(null);
    setError(null);
  }, [filePath, file?.blob_ref]);

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: ["workspace-tree", universeId, workspaceId] });
    void queryClient.invalidateQueries({ queryKey: ["workspaces", universeId] });
  };

  const save = useMutation({
    mutationFn: (contentText: string) =>
      api(
        "PUT",
        `/api/v1/universes/${universeId}/workspaces/${workspaceId}/files/${filePath}`,
        {
          contentText,
          expectedRevision: tree.data!.workspace.revision,
          ...(mediaTypeFor(filePath) ? { mediaType: mediaTypeFor(filePath) } : {}),
        },
      ),
    onSuccess: () => {
      setDraft(null);
      setError(null);
      invalidate();
    },
    onError: (err) => setError(err.message),
  });

  const remove = useMutation({
    mutationFn: () =>
      api(
        "DELETE",
        `/api/v1/universes/${universeId}/workspaces/${workspaceId}/files/${filePath}?expectedRevision=${tree.data!.workspace.revision}`,
      ),
    onSuccess: () => {
      invalidate();
      navigate(`/u/${slug}/workspaces/${workspaceId}`);
    },
    onError: (err) => setError(err.message),
  });

  if (tree.isLoading) {
    return <LoadingNote />;
  }
  if (tree.data && !file) {
    return (
      <div className="flex flex-1 items-center justify-center p-6 text-sm text-muted-foreground">
        File not found in this workspace.
      </div>
    );
  }

  const text =
    decoded?.kind === "text" ? new TextDecoder().decode(decoded.bytes) : null;
  const value = draft ?? text ?? "";
  const dirty = draft !== null && draft !== text;

  return (
    <>
      <header className="flex h-12 shrink-0 items-center gap-3 border-b px-4">
        <NavLink to={`/u/${slug}/workspaces/${workspaceId}`} className="md:hidden">
          <ChevronRight className="size-4 rotate-180" />
        </NavLink>
        <h1 className="truncate font-mono text-sm">{filePath}</h1>
        <span className="shrink-0 text-xs text-muted-foreground">
          {file ? formatBytes(file.size_bytes) : ""}
        </span>
        <div className="ml-auto flex shrink-0 items-center gap-1.5">
          {decoded?.kind === "text" && (
            <Button
              size="sm"
              disabled={!dirty || save.isPending}
              onClick={() => save.mutate(value)}
            >
              {save.isPending ? "Saving…" : dirty ? "Save" : "Saved"}
            </Button>
          )}
          <AlertDialog>
            <AlertDialogTrigger
              render={
                <Button
                  variant="ghost"
                  size="icon-sm"
                  className="text-destructive"
                  aria-label="Delete file"
                />
              }
            >
              <Trash2 />
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Delete {filePath}?</AlertDialogTitle>
                <AlertDialogDescription>
                  Commits a new snapshot without this file. Earlier snapshots keep it.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction
                  className="bg-destructive text-white hover:bg-destructive/90"
                  onClick={() => remove.mutate()}
                >
                  Delete
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </div>
      </header>
      {error && <p className="border-b px-4 py-2 text-sm text-destructive">{error}</p>}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {blob.isLoading && <div className="p-4"><LoadingNote /></div>}
        {blob.error && (
          <p className="p-4 text-sm text-destructive">{blob.error.message}</p>
        )}
        {decoded?.kind === "text" && (
          <textarea
            className="h-full w-full resize-none bg-transparent p-4 font-mono text-sm outline-none"
            value={value}
            onChange={(e) => setDraft(e.target.value)}
            spellCheck={false}
          />
        )}
        {decoded?.kind === "image" && (
          <div className="flex items-start p-4">
            <img
              alt={filePath}
              className="max-w-full rounded-lg border"
              src={`data:${file?.media_type ?? mediaTypeFor(filePath) ?? "image/png"};base64,${blob.data?.bytesBase64}`}
            />
          </div>
        )}
        {decoded?.kind === "binary" && (
          <p className="p-4 text-sm text-muted-foreground">
            Binary file ({formatBytes(file?.size_bytes ?? 0)}) — no preview.
          </p>
        )}
      </div>
    </>
  );
}

function NewWorkspaceDialog({
  universeId,
  slug,
  open,
  onOpenChange,
}: {
  universeId: string;
  slug: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [displayName, setDisplayName] = useState("");
  const [workspaceId, setWorkspaceId] = useState("");
  // Once the id is hand-edited it stops tracking the name; clearing it
  // resumes tracking.
  const [idTouched, setIdTouched] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const create = useMutation({
    mutationFn: (input: unknown) =>
      api<WorkspaceRow>("POST", `/api/v1/universes/${universeId}/workspaces`, input),
    onSuccess: async (created) => {
      await queryClient.invalidateQueries({ queryKey: ["workspaces", universeId] });
      onOpenChange(false);
      setDisplayName("");
      setWorkspaceId("");
      setIdTouched(false);
      setError(null);
      navigate(`/u/${slug}/workspaces/${created.workspaceId}`);
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!workspaceId) {
      setError("a name or id is required");
      return;
    }
    const parsed = workspaceCreateSchema.safeParse({
      workspaceId,
      ...(displayName ? { displayName } : {}),
    });
    if (!parsed.success) {
      const issue = parsed.error.issues[0];
      setError(issue ? `${issue.path.join(".")}: ${issue.message}` : "invalid input");
      return;
    }
    create.mutate(parsed.data);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New workspace</DialogTitle>
          <DialogDescription>
            Starts empty; profiles link it by workspace id.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="new-workspace-name">Display name</FieldLabel>
            <Input
              id="new-workspace-name"
              value={displayName}
              onChange={(e) => {
                setDisplayName(e.target.value);
                if (!idTouched) {
                  setWorkspaceId(e.target.value ? slugify(e.target.value) : "");
                }
              }}
              placeholder="Notes"
              autoFocus
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="new-workspace-id">Workspace id</FieldLabel>
            <Input
              id="new-workspace-id"
              value={workspaceId}
              onChange={(e) => {
                setWorkspaceId(e.target.value);
                setIdTouched(e.target.value.length > 0);
              }}
              placeholder="notes"
              className="font-mono"
            />
            <FieldDescription>
              What profile workspace links reference — cannot be changed later.
            </FieldDescription>
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={create.isPending}>
              {create.isPending ? "Creating…" : "Create"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function NewFileDialog({
  universeId,
  slug,
  workspaceId,
  revision,
  open,
  onOpenChange,
}: {
  universeId: string;
  slug: string;
  workspaceId: string;
  revision: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);

  const create = useMutation({
    mutationFn: () =>
      api(
        "PUT",
        `/api/v1/universes/${universeId}/workspaces/${workspaceId}/files/${path}`,
        {
          contentText: "",
          expectedRevision: revision,
          ...(mediaTypeFor(path) ? { mediaType: mediaTypeFor(path) } : {}),
        },
      ),
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: ["workspace-tree", universeId, workspaceId],
      });
      await queryClient.invalidateQueries({ queryKey: ["workspaces", universeId] });
      onOpenChange(false);
      const target = path;
      setPath("");
      setError(null);
      navigate(`/u/${slug}/workspaces/${workspaceId}/files/${target}`);
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const trimmed = path.replace(/^\/+|\/+$/g, "");
    if (!trimmed) {
      setError("path is required");
      return;
    }
    setPath(trimmed);
    create.mutate();
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New file</DialogTitle>
          <DialogDescription>
            Directories in the path are created as needed.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="new-file-path">Path</FieldLabel>
            <Input
              id="new-file-path"
              value={path}
              onChange={(e) => setPath(e.target.value)}
              placeholder="notes/todo.md"
              className="font-mono"
              autoFocus
              required
            />
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={create.isPending}>
              {create.isPending ? "Creating…" : "Create"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

const TEXT_MIMES = /^(text\/|application\/(json|toml|yaml|x-yaml|xml|javascript))/;
const IMAGE_EXTENSIONS: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  svg: "image/svg+xml",
};
const TEXT_EXTENSIONS: Record<string, string> = {
  md: "text/markdown",
  txt: "text/plain",
  json: "application/json",
  yaml: "application/yaml",
  yml: "application/yaml",
  toml: "application/toml",
  ts: "text/typescript",
  js: "text/javascript",
  py: "text/x-python",
  sh: "text/x-shellscript",
  css: "text/css",
  html: "text/html",
};

function mediaTypeFor(path: string): string | undefined {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return TEXT_EXTENSIONS[ext] ?? IMAGE_EXTENSIONS[ext];
}

function classify(
  path: string,
  mediaType: string | undefined,
  bytes: Uint8Array,
): "text" | "image" | "binary" {
  const effective = mediaType ?? mediaTypeFor(path);
  if (effective?.startsWith("image/")) {
    return "image";
  }
  if (effective && TEXT_MIMES.test(effective)) {
    return "text";
  }
  // Sniff: no NUL byte in the first KiB → treat as editable text.
  const probe = bytes.subarray(0, 1024);
  for (const byte of probe) {
    if (byte === 0) {
      return "binary";
    }
  }
  return "text";
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ["KiB", "MiB", "GiB"];
  let value = bytes;
  let unit = "B";
  for (const next of units) {
    if (value < 1024) break;
    value /= 1024;
    unit = next;
  }
  return `${value.toFixed(value >= 10 ? 0 : 1)} ${unit}`;
}

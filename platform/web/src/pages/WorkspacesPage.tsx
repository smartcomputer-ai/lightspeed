import { useActionPermissions } from "@/lib/permissions";
import { ReadError } from "@/components/read-error";
import { useEffect, useMemo, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { NavLink, useNavigate, useParams } from "react-router-dom";
import { slugify, workspaceCreateSchema, validWorkspaceTransferPath } from "@lightspeed-ai/platform-shared";
import {
  ChevronRight,
  File,
  FolderGit2,
  Plus,
  ExternalLink,
} from "lucide-react";
import {
  api,
  type BlobContent,
  type VfsFileEntry,
  type VfsTreeEntry,
  type WorkspaceRow,
  type WorkspaceTree,
} from "@/api";
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
import { SettingsDisclosure } from "@/components/ui/settings-disclosure";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { DetailPrompt, LoadingNote, UniverseNotFound } from "@/components/page";
import { useCreateParam } from "@/lib/create-param";
import { useActiveUniverse } from "@/lib/universes";
import { cn } from "@/lib/utils";
import { appHref, blobHref } from "@/lib/blob-view";
import { ListPane } from "@/components/list-pane";
import { WorkspaceFileTree } from "@/components/workspace-file-tree";
import { PdfPreview } from "@/components/pdf-preview";
import { MarkdownViewToggle } from "@/components/markdown-view-toggle";
import { MarkdownContent } from "@/components/session/markdown-content";
import { WorkspaceTransfers, WorkspaceActionsMenu, WorkspaceDropArea } from "@/components/workspace-transfers";

/// U4b: workspace explorer + functional editor. Pane = workspace picker +
/// file tree of the head snapshot; detail = file editor (text), preview
/// (images), or metadata (binary). Writes run the full VFS dance
/// server-side (blob → manifest → snapshot → head advance) guarded by the
/// workspace revision the tree was loaded at.
export function WorkspacesPage({ admin: _admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const navigate = useNavigate();
  const permissions = useActionPermissions(universe?.id);
  const params = useParams<{ workspaceId: string; "*": string }>();
  const workspaceId = params.workspaceId;
  const filePath = params["*"] || undefined;
  const [newFile, setNewFile] = useState<{ parent: string; revision: number } | null>(null);
  const [preview, setPreview] = useState(false);
  const [editor, setEditor] = useState({ workspaceId, path: filePath, identity: 0 });
  const [pendingRename, setPendingRename] = useState<{
    from: string;
    to: string;
  } | null>(null);
  // A rename preserves the editor; ordinary navigation starts a new one.
  if (editor.workspaceId !== workspaceId || editor.path !== filePath) {
    if (editor.workspaceId !== workspaceId) setNewFile(null);
    const renamed = editor.workspaceId === workspaceId &&
      pendingRename?.from === editor.path && pendingRename?.to === filePath;
    setEditor({
      workspaceId, path: filePath, identity: editor.identity + (renamed ? 0 : 1),
    });
    setPendingRename(null);
  }
  const [, setCreateOpen] = useCreateParam("workspace");

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !permissions.can("read")) {
    return (
      <div className="p-6">
        <UniverseNotFound slug={slug} />
      </div>
    );
  }

  return (
    <WorkspaceTransfers key={`${universe.id}:${workspaceId}`} universeId={universe.id} workspaceId={workspaceId}
      onNewFile={(parent, revision) => setNewFile({ parent, revision })}
      onWorkspaceRemoved={() => navigate(`/u/${slug}/workspaces`, { replace: true })}
      onRenamed={(from, to) => {
        if (filePath && (filePath === from || filePath.startsWith(`${from}/`))) {
          const path = to + filePath.slice(from.length);
          setPendingRename({ from: filePath, to: path });
          navigate(`/u/${slug}/workspaces/${workspaceId}/files/${path.split("/").map(encodeURIComponent).join("/")}`, { replace: true });
        }
      }}
      onRemoved={(path) => {
        if (filePath === path || filePath?.startsWith(`${path}/`)) navigate(`/u/${slug}/workspaces/${workspaceId}`);
      }}>
    <div className="flex min-h-0 flex-1">
      <ListPane detailOpen={Boolean(filePath)}>
        <WorkspacePane
          universeId={universe.id}
          slug={slug!}
          workspaceId={workspaceId}
          filePath={filePath}
        />
      </ListPane>
      <section
        className={cn("min-w-0 flex-1 flex-col", filePath ? "flex" : "hidden md:flex")}
      >
        {workspaceId && filePath ? (
          <FileDetail
            universeId={universe.id}
            slug={slug!}
            workspaceId={workspaceId}
            filePath={filePath}
            fileIdentity={editor.identity}
            preview={preview}
            onPreviewChange={setPreview}
          />
        ) : (
          workspaceId ? (
            <DetailPrompt icon={<File className="size-10 text-muted-foreground/60" />}>
              {permissions.can("use_resource") ? "Pick a file, or drop files and folders into this workspace." : "Pick a file."}
            </DetailPrompt>
          ) : (
            <DetailPrompt
              icon={<FolderGit2 className="size-10 text-muted-foreground/60" />}
              create={permissions.can("create_workspace") ? { label: "New workspace", onClick: () => setCreateOpen(true) } : undefined}
            >
              Pick a workspace{permissions.can("create_workspace") ? ", or create one" : ""}.
            </DetailPrompt>
          )
        )}
      </section>
    </div>
    {newFile && workspaceId && permissions.can("use_resource") && (
      <NewFileDialog
        key={`${workspaceId}:${newFile.parent}`}
        universeId={universe.id}
        slug={slug!}
        workspaceId={workspaceId}
        revision={newFile.revision}
        parent={newFile.parent}
        open
        onOpenChange={(open) => { if (!open) setNewFile(null); }}
      />
    )}
    </WorkspaceTransfers>
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
  const permissions = useActionPermissions(universeId);
  const canCreate = permissions.can("create_workspace");
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
  const [createOpen, setCreateOpen] = useCreateParam("workspace");

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
        {canCreate && (
          <Button
            variant="ghost"
            size="icon-sm"
            className="ml-auto"
            onClick={() => setCreateOpen(true)}
            aria-label="New workspace"
          >
            <Plus />
          </Button>
        )}
      </div>
      <div className="flex min-w-0 items-center gap-2 border-b p-3">
        <div className="min-w-0 flex-1">
        {workspaces.data && workspaces.data.length > 0 ? (
          <Select
            value={workspaceId ?? ""}
            onValueChange={(value) => navigate(`/u/${slug}/workspaces/${value}`)}
          >
            <SelectTrigger className="w-full" aria-label="Workspace">
              <FolderGit2 className="size-4 shrink-0 text-muted-foreground" />
              <SelectValue>
                {(value: string) => {
                  const label = workspaces.data?.find((w) => w.workspaceId === value)?.displayName ?? value;
                  // Text directly inside the flex value cannot ellipsize.
                  return <span className="min-w-0 truncate" title={label}>{label}</span>;
                }}
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              {workspaces.data.map((workspace) => (
                <SelectItem key={workspace.workspaceId} value={workspace.workspaceId}>
                  <span className="truncate">{workspace.displayName ?? workspace.workspaceId}</span>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        ) : (
          <p className="text-sm text-muted-foreground">
            {workspaces.isLoading ? "Loading…" : "No workspaces yet."}
          </p>
        )}
        </div>
        {tree.data && <WorkspaceActionsMenu kind="workspace" fileCount={tree.data.workspace.files} />}
      </div>
      <WorkspaceDropArea className="min-h-0 min-w-0 flex-1 overflow-y-auto p-2">
        {tree.error && (
          <ReadError error={tree.error} loading={!tree.data} className="p-2" />
        )}
        {tree.data && Object.keys(tree.data.manifest.root.entries).length === 0 && (
          <p className="p-2 text-sm text-muted-foreground">
            Empty workspace.
          </p>
        )}
        {tree.data && (
          <WorkspaceFileTree
            entries={tree.data.manifest.root.entries}
            slug={slug}
            workspaceId={workspaceId!}
            activePath={filePath}
          />
        )}
      </WorkspaceDropArea>
      {canCreate && (
        <NewWorkspaceDialog
          universeId={universeId}
          slug={slug}
          open={createOpen}
          onOpenChange={setCreateOpen}
        />
      )}
    </>
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
  fileIdentity,
  preview,
  onPreviewChange,
}: {
  universeId: string;
  slug: string;
  workspaceId: string;
  filePath: string;
  fileIdentity: number;
  preview: boolean;
  onPreviewChange: (preview: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const permissions = useActionPermissions(universeId);
  const canEditFiles = permissions.can("use_resource");
  const tree = useQuery({
    queryKey: ["workspace-tree", universeId, workspaceId],
    queryFn: () =>
      api<WorkspaceTree>(
        "GET",
        `/api/v1/universes/${universeId}/workspaces/${workspaceId}/tree`,
      ),
  });
  const file = tree.data
    ? findFile(tree.data.manifest.root.entries, filePath)
    : null;
  const blob = useQuery({
    queryKey: [
      "workspace-file",
      universeId,
      workspaceId,
      filePath,
      file?.blob_ref,
    ],
    queryFn: () =>
      api<BlobContent>(
        "GET",
        `/api/v1/universes/${universeId}/workspaces/${encodeURIComponent(workspaceId)}/files/${filePath.split("/").map(encodeURIComponent).join("/")}`,
      ),
    enabled: !!file,
    placeholderData: (previous, query) =>
      query?.queryKey[1] === universeId &&
      query.queryKey[2] === workspaceId &&
      query.queryKey[3] === filePath
        ? previous
        : undefined,
  });

  const decoded = useMemo(() => {
    if (!blob.data) {
      return null;
    }
    const bytes = Uint8Array.from(atob(blob.data.bytesBase64), (c) =>
      c.charCodeAt(0),
    );
    return { bytes, kind: classify(filePath, file?.media_type, bytes) };
  }, [blob.data, file?.media_type, filePath]);

  const text =
    decoded?.kind === "text" ? new TextDecoder().decode(decoded.bytes) : null;
  const [editor, setEditor] = useState<{
    identity: number;
    draft: string | null;
    saved: { text: string; previousBlob: string } | null;
    error: string | null;
  }>({ identity: fileIdentity, draft: null, saved: null, error: null });
  if (editor.identity !== fileIdentity) {
    setEditor({
      identity: fileIdentity,
      draft: null,
      saved: null,
      error: null,
    });
  }
  // A successful save becomes the baseline immediately. Keep its draft until
  // the refreshed blob is ready, and preserve anything typed during the save.
  useEffect(() => {
    if (blob.isPlaceholderData || text === null || !file) return;
    setEditor((current) => {
      if (
        current.identity !== fileIdentity ||
        !current.saved ||
        (file.blob_ref === current.saved.previousBlob &&
          text !== current.saved.text)
      )
        return current;
      return {
        ...current,
        draft: current.draft === current.saved.text ? null : current.draft,
        saved: null,
      };
    });
  }, [
    fileIdentity,
    file?.blob_ref,
    text,
    blob.isPlaceholderData,
    editor.saved,
  ]);

  const invalidate = () =>
    Promise.all([
      queryClient.invalidateQueries({
        queryKey: ["workspace-tree", universeId, workspaceId],
      }),
      queryClient.invalidateQueries({ queryKey: ["workspaces", universeId] }),
    ]);

  const save = useMutation({
    mutationFn: (submission: {
      contentText: string;
      path: string;
      revision: number;
      identity: number;
      previousBlob: string;
    }) =>
      api(
        "PUT",
        `/api/v1/universes/${universeId}/workspaces/${workspaceId}/files/${submission.path.split("/").map(encodeURIComponent).join("/")}`,
        {
          contentText: submission.contentText,
          expectedRevision: submission.revision,
          ...(mediaTypeFor(submission.path)
            ? { mediaType: mediaTypeFor(submission.path) }
            : {}),
        },
      ),
    onSuccess: async (_result, submission) => {
      setEditor((current) =>
        current.identity === submission.identity
          ? {
              ...current,
              error: null,
              saved: {
                text: submission.contentText,
                previousBlob: submission.previousBlob,
              },
            }
          : current,
      );
      await invalidate();
    },
    onError: (err, submission) =>
      setEditor((current) =>
        current.identity === submission.identity
          ? { ...current, error: err.message }
          : current,
      ),
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

  const value = editor.draft ?? text ?? "";
  const dirty =
    editor.draft !== null && editor.draft !== (editor.saved?.text ?? text);
  const blobLink = file ? blobHref(slug, file.blob_ref, {
    name: filePath.split("/").at(-1),
    type: file.media_type ?? mediaTypeFor(filePath),
    workspace: workspaceId,
    path: filePath,
  }) : null;
  const saveCurrent = () =>
    save.mutate({
      contentText: value,
      path: filePath,
      revision: tree.data!.workspace.revision,
      identity: fileIdentity,
      previousBlob: file!.blob_ref,
    });

  return (
    <div
      className="flex min-h-0 flex-1 flex-col"
      onKeyDown={(event) => {
        if (
          !canEditFiles ||
          decoded?.kind !== "text" ||
          event.nativeEvent.isComposing ||
          !(event.metaKey || event.ctrlKey) ||
          event.altKey ||
          event.shiftKey ||
          event.key.toLowerCase() !== "s"
        )
          return;
        event.preventDefault();
        if (dirty && !save.isPending && !event.repeat) saveCurrent();
      }}
    >
      <header className="flex min-h-12 shrink-0 flex-wrap items-center gap-3 border-b px-4 py-2">
        <NavLink
          to={`/u/${slug}/workspaces/${workspaceId}`}
          className="md:hidden"
        >
          <ChevronRight className="size-4 rotate-180" />
        </NavLink>
        <h1 className="min-w-0 flex-1 truncate font-mono text-sm">{filePath}</h1>
        <span className="shrink-0 text-xs text-muted-foreground">
          {file ? formatBytes(file.size_bytes) : ""}
        </span>
        <div className="ml-auto flex shrink-0 basis-full items-center justify-end gap-1.5 sm:basis-auto">
          {blobLink && (
            <Button
              variant="ghost"
              size="icon-sm"
              nativeButton={false}
              render={<a href={appHref(blobLink)} target="_blank" rel="noopener noreferrer" />}
              aria-label="Open in blob viewer (new tab)"
              title={dirty ? "Open saved version in blob viewer (new tab)" : "Open in blob viewer (new tab)"}
            >
              <ExternalLink />
            </Button>
          )}
          {decoded?.kind === "text" && (
            <MarkdownViewToggle preview={preview} onPreviewChange={onPreviewChange} />
          )}
          {canEditFiles && decoded?.kind === "text" && (
            <Button
              size="sm"
              aria-keyshortcuts="Meta+S Control+S"
              title="Save (⌘S / Ctrl+S)"
              disabled={!dirty || save.isPending}
              onClick={saveCurrent}
            >
              {save.isPending ? "Saving…" : dirty ? "Save" : "Saved"}
            </Button>
          )}
        </div>
      </header>
      {editor.error && (
        <p className="border-b px-4 py-2 text-sm text-destructive">
          {editor.error}
        </p>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {blob.isLoading && (
          <div className="p-4">
            <LoadingNote />
          </div>
        )}
        {blob.error && (
          <p className="p-4 text-sm text-destructive">{blob.error.message}</p>
        )}
        {decoded?.kind === "text" && (preview ? (
          <MarkdownContent className="mx-auto max-w-5xl p-4">{value}</MarkdownContent>
        ) : (
          <textarea
            className="h-full w-full resize-none bg-transparent p-4 font-mono text-sm outline-none"
            value={value}
            onChange={(e) =>
              setEditor((current) => ({ ...current, draft: e.target.value }))
            }
            spellCheck={false}
            readOnly={!canEditFiles}
            aria-label={
              canEditFiles ? "File contents" : "File contents (read only)"
            }
          />
        ))}
        {decoded?.kind === "image" && (
          <div className="flex items-start p-4">
            <img
              alt={filePath}
              className="max-w-full rounded-lg border"
              src={`data:${file?.media_type ?? mediaTypeFor(filePath) ?? "image/png"};base64,${blob.data?.bytesBase64}`}
            />
          </div>
        )}
        {decoded?.kind === "pdf" && (
          <PdfPreview bytes={decoded.bytes} name={filePath} />
        )}
        {decoded?.kind === "binary" && (
          <p className="p-4 text-sm text-muted-foreground">
            Binary file ({formatBytes(file?.size_bytes ?? 0)}) — no preview.
          </p>
        )}
      </div>
    </div>
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
            {/* The id follows the name until it is edited; it opens on its own
                when it is missing, invalid or already taken. */}
            <SettingsDisclosure
              summary={workspaceId
                ? <>Id: <code className="font-mono">{workspaceId}</code></>
                : "Id derived from the display name"}
              action="Change"
              label="Change workspace id"
              forceOpen={Boolean(error) && (!workspaceId || /workspaceId|already exists/.test(error ?? ""))}
            >
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
                  What profile workspace attachments reference — cannot be changed later.
                </FieldDescription>
              </Field>
            </SettingsDisclosure>
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
  parent,
  open,
  onOpenChange,
}: {
  universeId: string;
  slug: string;
  workspaceId: string;
  revision: number;
  parent: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);

  const create = useMutation({
    mutationFn: (target: string) =>
      api(
        "POST",
        `/api/v1/universes/${universeId}/workspaces/${encodeURIComponent(workspaceId)}/upload`,
        {
          expectedRevision: revision,
          replace: false,
          entries: [{
            kind: "file", path: target, contentBase64: "",
            ...(mediaTypeFor(target) ? { mediaType: mediaTypeFor(target) } : {}),
          }],
        },
      ),
    onSuccess: async (_data, target) => {
      await queryClient.invalidateQueries({
        queryKey: ["workspace-tree", universeId, workspaceId],
      });
      await queryClient.invalidateQueries({ queryKey: ["workspaces", universeId] });
      onOpenChange(false);
      setPath("");
      setError(null);
      navigate(`/u/${slug}/workspaces/${workspaceId}/files/${target.split("/").map(encodeURIComponent).join("/")}`);
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const trimmed = path.trim();
    if (create.isPending) return;
    if (!validWorkspaceTransferPath(trimmed)) {
      setError("Enter a valid relative file path.");
      return;
    }
    create.mutate(parent ? `${parent}/${trimmed}` : trimmed);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New file</DialogTitle>
          <DialogDescription>
            Create a file in {parent || "the workspace root"}.
            Directories in the path are created as needed.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="new-file-path">Path</FieldLabel>
            <Input
              id="new-file-path"
              value={path}
              disabled={create.isPending}
              onChange={(e) => { setPath(e.target.value); setError(null); }}
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
  if (ext === "pdf") return "application/pdf";
  return TEXT_EXTENSIONS[ext] ?? IMAGE_EXTENSIONS[ext];
}

function classify(
  path: string,
  mediaType: string | undefined,
  bytes: Uint8Array,
): "text" | "image" | "pdf" | "binary" {
  const mime = mediaType?.split(";")[0]?.trim().toLowerCase();
  if (mime === "application/pdf" || /\.pdf$/i.test(path) ||
    [0x25, 0x50, 0x44, 0x46, 0x2d].every((byte, index) => bytes[index] === byte)) {
    return "pdf";
  }
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

import { useWorkspaceList, useWorkspaceTree } from "@/module-api/workspaces";
import { useActionPermissions } from "@/lib/permissions";
import { useDocumentTitle } from "@/lib/document-title";
import { ReadError } from "@/components/read-error";
import { useEffect, useMemo, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "react-router-dom";
import { slugify, workspaceCreateSchema, validWorkspaceTransferPath } from "@lightspeed-ai/platform-shared";
import { AddIcon, DownloadIcon, FileIcon, WorkspaceIcon } from "@/icons";
import {
  api,
  type BlobContent,
  type VfsFileEntry,
  type VfsTreeEntry,
  type WorkspaceRow,
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
import { useDialogWarp } from "@/lib/dialog-warp";
import { useActiveUniverse } from "@/lib/universes";
import { cn } from "@/lib/utils";
import { appHref, blobHref } from "@/lib/blob-view";
import { DetailHeader } from "@/components/detail-header";
import { useAvailableWebModules } from "@/lib/web-modules";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { MetaToolbar } from "@/components/meta-toolbar";
import { fileMetaItems } from "@/components/workspace-file-meta";
import { ListPane, ListPaneHeader, ListPaneToolbar } from "@/components/list-pane";
import { WorkspaceFileTree } from "@/components/workspace-file-tree";
import { FileHost } from "@/components/file-providers/host";
import { useWorkspaceFileEditing, type FileRelocation } from "@/components/file-providers/editing";
import { canEdit, fileFacts, providerFor } from "@/components/file-providers/registry";
import { mediaTypeFor } from "@/components/file-providers/media-types";
import { textProvider } from "@/components/file-providers/providers/text";
import { SETTINGS, useSetting } from "@/lib/settings";
import { WorkspaceTransfers, WorkspaceActionsMenu, WorkspaceDropArea, useWorkspaceDownload } from "@/components/workspace-transfers";

/// U4b: workspace explorer + functional editor. Pane = workspace picker +
/// file tree of the head snapshot; detail = the open file, shown by its
/// file provider (components/file-providers). Writes run the full VFS dance
/// server-side (blob → manifest → snapshot → head advance) guarded by the
/// workspace revision the tree was loaded at.
export function WorkspacesPage() {
  const { universe, handle, isLoading } = useActiveUniverse();
  const navigate = useNavigate();
  const permissions = useActionPermissions(universe?.id);
  const params = useParams<{ workspaceId: string; "*": string }>();
  const workspaceId = params.workspaceId;
  const filePath = params["*"] || undefined;
  const [newFile, setNewFile] = useState<{ parent: string; revision: number } | null>(null);
  const [editor, setEditor] = useState({ workspaceId, path: filePath, identity: 0 });
  const [relocation, setRelocation] = useState<{ identity: number; before: number; after: number } | null>(null);
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
        <UniverseNotFound handle={handle} />
      </div>
    );
  }

  return (
    <WorkspaceTransfers key={`${universe.id}:${workspaceId}`} universeId={universe.id} workspaceId={workspaceId}
      onNewFile={(parent, revision) => setNewFile({ parent, revision })}
      onWorkspaceRemoved={() => navigate(`/u/${handle}/workspaces`, { replace: true })}
      onRenamed={(from, to, revision) => {
        if (filePath && (filePath === from || filePath.startsWith(`${from}/`))) {
          const path = to + filePath.slice(from.length);
          setRelocation({ identity: editor.identity, ...revision });
          setPendingRename({ from: filePath, to: path });
          navigate(`/u/${handle}/workspaces/${workspaceId}/files/${path.split("/").map(encodeURIComponent).join("/")}`, { replace: true });
        }
      }}
      onRemoved={(path) => {
        if (filePath === path || filePath?.startsWith(`${path}/`)) navigate(`/u/${handle}/workspaces/${workspaceId}`);
      }}>
    <div className="flex min-h-0 flex-1">
      <ListPane detailOpen={Boolean(filePath)}>
        <WorkspacePane
          universeId={universe.id}
          handle={handle!}
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
            handle={handle!}
            workspaceId={workspaceId}
            filePath={filePath}
            fileIdentity={editor.identity}
            relocation={relocation}
          />
        ) : (
          workspaceId ? (
            <DetailPrompt icon={<FileIcon className="size-10 text-muted-foreground/60" />}>
              {permissions.can("useResource") ? "Pick a file, or drop files and folders into this workspace." : "Pick a file."}
            </DetailPrompt>
          ) : (
            <DetailPrompt
              icon={<WorkspaceIcon className="size-10 text-muted-foreground/60" />}
              create={permissions.can("createWorkspace") ? { label: "New workspace", onClick: () => setCreateOpen(true) } : undefined}
            >
              Pick a workspace{permissions.can("createWorkspace") ? ", or create one" : ""}.
            </DetailPrompt>
          )
        )}
      </section>
    </div>
    {newFile && workspaceId && permissions.can("useResource") && (
      <NewFileDialog
        key={`${workspaceId}:${newFile.parent}`}
        universeId={universe.id}
        handle={handle!}
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
  handle,
  workspaceId,
  filePath,
}: {
  universeId: string;
  handle: string;
  workspaceId: string | undefined;
  filePath: string | undefined;
}) {
  const navigate = useNavigate();
  const permissions = useActionPermissions(universeId);
  const canCreate = permissions.can("createWorkspace");
  const workspaces = useWorkspaceList(universeId);
  const tree = useWorkspaceTree(universeId, workspaceId);
  const workspaceName = workspaceId && workspaces.data
    ? (workspaces.data.find((w) => w.workspaceId === workspaceId)?.displayName ?? workspaceId)
    : undefined;
  // The open file's path, deepest folder first, then its workspace.
  useDocumentTitle([...(filePath?.split("/").reverse() ?? []), workspaceName]);
  const [createOpen, setCreateOpen] = useCreateParam("workspace");

  // Auto-select the first workspace when landing on bare /workspaces.
  useEffect(() => {
    if (!workspaceId && workspaces.data?.[0]) {
      navigate(`/u/${handle}/workspaces/${workspaces.data[0].workspaceId}`, {
        replace: true,
      });
    }
  }, [workspaceId, workspaces.data, navigate, handle]);

  return (
    <>
      <ListPaneHeader
        title="Workspaces"
        count={workspaces.data?.length}
        actions={canCreate && (
          <Button variant="ghost" size="icon-sm" onClick={() => setCreateOpen(true)} aria-label="New workspace">
            <AddIcon />
          </Button>
        )}
      />
      {/* Which workspace the tree shows. */}
      <ListPaneToolbar className="flex-nowrap p-3 text-sm">
        <div className="min-w-0 flex-1">
        {workspaces.data && workspaces.data.length > 0 ? (
          <Select
            value={workspaceId ?? ""}
            onValueChange={(value) => navigate(`/u/${handle}/workspaces/${value}`)}
          >
            <SelectTrigger className="w-full" aria-label="Workspace">
              <SelectValue>
                {(value: string) => {
                  const label = workspaces.data?.find((w) => w.workspaceId === value)?.displayName ?? value;
                  // The icon is part of the value, as it is of each option,
                  // so the open list lines its icons and names up with it.
                  // Text directly inside the flex value cannot ellipsize.
                  return (
                    <>
                      <WorkspaceIcon className="size-4 shrink-0 text-muted-foreground" />
                      <span className="min-w-0 truncate" title={label}>{label}</span>
                    </>
                  );
                }}
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              {workspaces.data.map((workspace) => (
                <SelectItem key={workspace.workspaceId} value={workspace.workspaceId}>
                  {/* As in the trigger's value, so each option lines up under it. */}
                  <WorkspaceIcon className="size-4 shrink-0 text-muted-foreground" />
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
        {tree.data && workspaceId && (
          <WorkspaceActionsMenu kind="workspace" fileCount={tree.data.workspace.files}>
            <WorkspaceModuleActions handle={handle} workspaceId={workspaceId} />
          </WorkspaceActionsMenu>
        )}
      </ListPaneToolbar>
      {/* The scrolling body is the drop target, so a drop anywhere below the
          tree lands in the workspace root. */}
      <WorkspaceDropArea className="min-h-0 min-w-0 flex-1 overflow-y-auto">
        {tree.error && (
          <ReadError error={tree.error} loading={!tree.data} className="p-4" />
        )}
        {tree.data && Object.keys(tree.data.manifest.root.entries).length === 0 && (
          <p className="p-4 text-sm text-muted-foreground">
            Empty workspace.
          </p>
        )}
        {tree.data && (
          <WorkspaceFileTree
            entries={tree.data.manifest.root.entries}
            handle={handle}
            workspaceId={workspaceId!}
            activePath={filePath}
          />
        )}
      </WorkspaceDropArea>
      {canCreate && (
        <NewWorkspaceDialog
          universeId={universeId}
          handle={handle}
          open={createOpen}
          onOpenChange={setCreateOpen}
        />
      )}
    </>
  );
}

function WorkspaceModuleActions({ handle, workspaceId }: { handle: string; workspaceId: string }) {
  const { universe } = useActiveUniverse();
  const { modules } = useAvailableWebModules(universe);
  return modules.map(({ id, icon: Icon, workspaceAction }) => workspaceAction && (
    <DropdownMenuItem key={id} render={<Link to={workspaceAction.href(handle, workspaceId)} />}>
      <Icon /> {workspaceAction.label}
    </DropdownMenuItem>
  ));
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

/// One workspace file, shown by its provider (see components/file-providers):
/// the page loads the tree and the content and keeps the editing session;
/// the host lays out the header, the mode toggle and the view.
function FileDetail({
  universeId,
  handle,
  workspaceId,
  filePath,
  fileIdentity,
  relocation,
}: {
  universeId: string;
  handle: string;
  workspaceId: string;
  filePath: string;
  fileIdentity: number;
  relocation: FileRelocation | null;
}) {
  const permissions = useActionPermissions(universeId);
  const canEditFiles = permissions.can("useResource");
  const downloads = useWorkspaceDownload();
  const [mode, setMode] = useSetting(SETTINGS.workspaceFileMode(universeId, workspaceId, filePath));
  const tree = useWorkspaceTree(universeId, workspaceId);
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
        `/api/ui/universes/${universeId}/blobs/${encodeURIComponent(file!.blob_ref)}`,
      ),
    enabled: !!file,
    placeholderData: (previous, query) =>
      query?.queryKey[1] === universeId &&
      query.queryKey[2] === workspaceId &&
      query.queryKey[3] === filePath
        ? previous
        : undefined,
  });

  const bytes = useMemo(
    () => (blob.data ? Uint8Array.from(atob(blob.data.bytesBase64), (c) => c.charCodeAt(0)) : null),
    [blob.data],
  );
  const facts = fileFacts(filePath, file, bytes ?? EMPTY_BYTES);
  // Decided once the content is here: its first bytes are part of the match.
  const matched = bytes ? providerFor(facts) : null;
  const text = useMemo(
    () => (bytes && matched?.content === "text" ? new TextDecoder().decode(bytes) : null),
    [bytes, matched?.content],
  );
  const editing = useWorkspaceFileEditing({
    universeId,
    workspaceId,
    filePath,
    fileIdentity,
    relocation,
    tree: tree.data,
    treeError: tree.error,
    file,
    text,
    placeholder: blob.isPlaceholderData,
  });
  // A draft stays in a text editor even if the file can no longer be read
  // as text (it was replaced, or removed).
  const provider = editing.hasDraft && !(matched && canEdit(matched)) ? textProvider : matched;

  if (tree.isLoading) {
    return <LoadingNote />;
  }
  if (tree.data && !file && !editing.hasDraft) {
    return (
      <div className="flex flex-1 items-center justify-center p-6 text-sm text-muted-foreground">
        File not found in this workspace.
      </div>
    );
  }

  const blobLink = file ? blobHref(handle, file.blob_ref, {
    name: filePath.split("/").at(-1),
    type: file.media_type ?? mediaTypeFor(filePath),
    workspace: workspaceId,
    path: filePath,
  }) : null;
  const download = downloads ? () => downloads.download(filePath, false) : undefined;

  return (
    <FileHost
      header={{
        back: { to: `/u/${handle}/workspaces/${workspaceId}`, label: "Back to files" },
        title: filePath,
        titleHint: filePath,
        titleClassName: "font-mono font-normal",
        tags: file && (
          <MetaToolbar
            items={fileMetaItems({
              name: filePath.split("/").at(-1) ?? filePath,
              file,
              text,
              revision: tree.data?.workspace.revision,
              blobViewerHref: blobLink ? appHref(blobLink) : null,
              unsaved: editing.dirty,
            })}
            place="header"
            full
            label="File details"
            className="grow"
          />
        ),
        tools: [
          ...(downloads
            ? [{
                key: "download",
                label: "Download",
                icon: <DownloadIcon />,
                disabled: downloads.downloading,
                onSelect: download,
              }]
            : []),
          // The file's actions (rename, download, delete, …), as its row's
          // right-click menu in the list has them.
          { kind: "custom" as const, key: "file-actions", node: <WorkspaceActionsMenu path={filePath} kind="file" /> },
        ],
      }}
      facts={facts}
      provider={provider}
      bytes={bytes}
      loading={blob.isLoading}
      loadError={blob.error}
      notice={tree.error && !editing.dirty && <p className="p-4 text-sm text-destructive">{tree.error.message}</p>}
      unavailable={!file || tree.error !== null}
      editing={editing}
      writable={canEditFiles}
      mode={mode}
      onModeChange={setMode}
      {...(download ? { download } : {})}
    />
  );
}

/// No content yet: a provider decides on the file's metadata alone.
const EMPTY_BYTES = new Uint8Array();

function NewWorkspaceDialog({
  universeId,
  handle,
  open,
  onOpenChange,
}: {
  universeId: string;
  handle: string;
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

  const dialogWarp = useDialogWarp(open);
  const create = useMutation({
    mutationFn: (input: unknown) =>
      api<WorkspaceRow>("POST", `/api/ui/universes/${universeId}/workspaces`, input),
    onSuccess: async (created) => {
      await queryClient.invalidateQueries({ queryKey: ["workspaces", universeId] });
      // Once the warp has played out: close, and open the new workspace.
      dialogWarp.afterWarp(() => {
        onOpenChange(false);
        setDisplayName("");
        setWorkspaceId("");
        setIdTouched(false);
        setError(null);
        navigate(`/u/${handle}/workspaces/${created.workspaceId}`);
      });
    },
    onError: (err) => {
      // Called off: the dialog comes back with the error.
      dialogWarp.cancel();
      setError(err.message);
    },
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
    // Creating sends the dialog off to warp meanwhile.
    dialogWarp.start();
    create.mutate(parsed.data);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent starscape warp={dialogWarp.warp}>
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
  handle,
  workspaceId,
  revision,
  parent,
  open,
  onOpenChange,
}: {
  universeId: string;
  handle: string;
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
        `/api/ui/universes/${universeId}/workspaces/${encodeURIComponent(workspaceId)}/upload`,
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
      navigate(`/u/${handle}/workspaces/${workspaceId}/files/${target.split("/").map(encodeURIComponent).join("/")}`);
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

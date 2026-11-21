import { useEffect, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link, useSearchParams } from "react-router-dom";
import { formatUploadBytes } from "@lightspeed-ai/platform-shared";
import { useActionPermissions, useActiveUniverse, useDocumentTitle } from "@lightspeed-ai/platform-web/modules";
import {
  Button, DocumentIcon, EmptyState, Input, LoadingNote, PageHeader, ReadError, Select, SelectContent,
  SelectItem, SelectTrigger, SelectValue, UniverseNotFound, fileIcon,
} from "@lightspeed-ai/platform-web/modules/ui";
import {
  addWorkspaceFiles, blobHref, refreshWorkspace, useWorkspaceList, useWorkspaceTree,
  type VfsFileEntry, type VfsTreeEntry, type WorkspaceTree,
} from "@lightspeed-ai/platform-web/modules/workspaces";

// The shelf indexes ordinary workspace files. Moving a file into or out of
// this folder changes its publication status for every reader of the workspace.
const FOLDER = "artifacts";

function* filesIn(
  entries: Record<string, VfsTreeEntry>,
  parent = "",
): Generator<{ path: string; file: VfsFileEntry }> {
  for (const [name, entry] of Object.entries(entries)) {
    const path = parent ? `${parent}/${name}` : name;
    if (entry.kind === "directory") yield* filesIn(entry.entries, path);
    else yield { path, file: entry };
  }
}

export function ArtifactsPage() {
  const { universe, handle, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (isLoading) return <LoadingNote />;
  if (!universe || !permissions.can("read")) return <UniverseNotFound handle={handle} />;
  return (
    <Artifacts
      key={universe.id}
      universeId={universe.id}
      handle={universe.handle}
      writable={permissions.can("useResource")}
    />
  );
}

function Artifacts({
  universeId,
  handle,
  writable,
}: {
  universeId: string;
  handle: string;
  writable: boolean;
}) {
  const [params, setParams] = useSearchParams();
  const workspaceId = params.get("workspace");
  const workspaces = useWorkspaceList(universeId);
  const selected = workspaces.data?.find((workspace) => workspace.workspaceId === workspaceId);
  const first = workspaces.data?.[0]?.workspaceId;
  useEffect(() => {
    if (workspaceId === null && first) setParams({ workspace: first }, { replace: true });
  }, [workspaceId, first, setParams]);
  useDocumentTitle([selected?.displayName ?? selected?.workspaceId]);
  return (
    <section className="mx-auto w-full max-w-4xl p-4 sm:p-6">
      <PageHeader
        title="Artifacts"
        description={
          <>
            Files published to the <code>artifacts/</code> folder of a workspace.
          </>
        }
      />
      {workspaces.isPending && <LoadingNote />}
      {workspaces.error && <ReadError error={workspaces.error} />}
      {workspaces.isSuccess &&
        (workspaces.data.length === 0 ? (
          <EmptyState icon={DocumentIcon} title="No workspaces available" />
        ) : (
          <>
            <label className="mb-5 grid w-full max-w-sm gap-1.5 text-sm font-medium">
              Workspace
              <Select
                value={selected?.workspaceId ?? ""}
                onValueChange={(value) => {
                  if (value) setParams({ workspace: value });
                }}
              >
                <SelectTrigger aria-label="Workspace" className="w-full">
                  <SelectValue placeholder="Select a workspace" />
                </SelectTrigger>
                <SelectContent>
                  {workspaces.data.map((w) => (
                    <SelectItem key={w.workspaceId} value={w.workspaceId}>
                      {w.displayName ?? w.workspaceId}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </label>
            {selected && (
              <Shelf
                key={selected.workspaceId}
                workspaceId={selected.workspaceId}
                universeId={universeId}
                handle={handle}
                writable={writable}
              />
            )}
            {workspaceId && !selected && (
              <p role="status">This workspace is unavailable. Select another workspace.</p>
            )}
          </>
        ))}
    </section>
  );
}

function Shelf({
  universeId,
  workspaceId,
  handle,
  writable,
}: {
  universeId: string;
  workspaceId: string;
  handle: string;
  writable: boolean;
}) {
  const client = useQueryClient();
  const input = useRef<HTMLInputElement>(null);
  const [files, setFiles] = useState<File[]>([]);
  const [filter, setFilter] = useState("");
  const tree = useWorkspaceTree(universeId, workspaceId);
  const publish = useMutation({
    mutationFn: (snapshot: WorkspaceTree) => addWorkspaceFiles(universeId, snapshot, FOLDER, files),
    onSuccess: () => {
      if (input.current) input.current.value = "";
      setFiles([]);
      setFilter("");
    },
    onSettled: () => refreshWorkspace(client, universeId, workspaceId),
  });
  if (tree.error) return <ReadError error={tree.error} />;
  if (!tree.data) return <LoadingNote />;
  const snapshot = tree.data;
  const folder = snapshot.manifest.root.entries[FOLDER];
  if (folder?.kind === "file")
    return (
      <p role="alert">
        The workspace contains a file named <code>artifacts</code>. Rename it in Workspaces to publish
        artifacts here.
      </p>
    );
  const items = folder
    ? Array.from(filesIn(folder.entries)).sort((a, b) => a.path.localeCompare(b.path))
    : [];
  const shown = items.filter(({ path }) => path.toLowerCase().includes(filter.trim().toLowerCase()));
  return (
    <div className="grid gap-5">
      {writable && (
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            if (files.length && !publish.isPending) publish.mutate(snapshot);
          }}
        >
          <label className="grid min-w-0 flex-1 gap-1.5 text-sm font-medium">
            Files to publish
            <Input
              ref={input}
              type="file"
              multiple
              disabled={publish.isPending}
              onChange={(event) => {
                setFiles(Array.from(event.target.files ?? []));
                publish.reset();
              }}
            />
          </label>
          <Button type="submit" disabled={files.length === 0 || publish.isPending}>
            {publish.isPending ? "Publishing…" : "Publish"}
          </Button>
        </form>
      )}
      {publish.isError && (
        <p role="alert" className="text-sm text-destructive">
          {publish.error.message}
        </p>
      )}
      {publish.isSuccess && (
        <p role="status" className="text-sm text-muted-foreground">
          Files published.
        </p>
      )}
      {items.length === 0 ? (
        <EmptyState icon={DocumentIcon} title="No published artifacts" />
      ) : (
        <>
          <Input
            aria-label="Filter artifacts by path"
            placeholder="Filter by file path"
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
          />
          {shown.length === 0 ? (
            <p className="text-sm text-muted-foreground">No matching artifacts.</p>
          ) : (
            <ul className="divide-y rounded-lg border" aria-label="Published artifacts">
              {shown.map(({ path, file }) => {
                const name = path.split("/").at(-1)!;
                const Icon = fileIcon(name);
                const href = blobHref(handle, file.blob_ref, {
                  name,
                  type: file.media_type,
                  workspace: workspaceId,
                  path: `${FOLDER}/${path}`,
                });
                return (
                  <li key={path} className="flex items-center gap-3 px-3 py-3 text-sm">
                    <Icon className="size-5 shrink-0 text-muted-foreground" />
                    {href ? (
                      <Link to={href} className="min-w-0 flex-1 break-all font-medium hover:underline">
                        {path}
                      </Link>
                    ) : (
                      <span className="min-w-0 flex-1 break-all">{path}</span>
                    )}
                    <span className="shrink-0 text-xs tabular-nums text-muted-foreground">
                      {formatUploadBytes(file.size_bytes)}
                    </span>
                  </li>
                );
              })}
            </ul>
          )}
        </>
      )}
    </div>
  );
}

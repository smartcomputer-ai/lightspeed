import { useState, type FormEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { formatUploadBytes } from "@lightspeed-ai/platform-shared";
import { api, ApiError } from "@lightspeed-ai/platform-web/modules/client";
import {
  Button, Input, ReadError, Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from "@lightspeed-ai/platform-web/modules/ui";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@lightspeed-ai/platform-web/modules/ui/dialog";
import { useWorkspaceList, useWorkspaceTree, type VfsTreeEntry } from "@lightspeed-ai/platform-web/modules/workspaces";
import { ARTIFACT_MAX_BYTES } from "../contract";
import { artifactsApi, artifactsKey } from "./queries";

function* workspaceFiles(entries: Record<string, VfsTreeEntry>, prefix = ""): Generator<{ path: string; size: number }> {
  for (const [name, entry] of Object.entries(entries)) {
    const path = prefix + name;
    if (entry.kind === "directory") yield* workspaceFiles(entry.entries, path + "/");
    else yield { path, size: entry.size_bytes };
  }
}

export function PublishArtifactDialog({ universeId, initialWorkspace, onClose }: {
  universeId: string; initialWorkspace: string | null; onClose: () => void;
}) {
  const client = useQueryClient();
  const [selection, setSelection] = useState(initialWorkspace ?? "");
  const [path, setPath] = useState("");
  const [title, setTitle] = useState("");
  const workspaces = useWorkspaceList(universeId);
  const workspaceId = selection || workspaces.data?.[0]?.workspaceId;
  const selected = workspaces.data?.find(w => w.workspaceId === workspaceId);
  const tree = useWorkspaceTree(universeId, selected?.workspaceId);
  const files = tree.data ? Array.from(workspaceFiles(tree.data.manifest.root.entries)).sort((a,b) => a.path.localeCompare(b.path)) : [];
  const selectedFile = files.find(file => file.path === path);
  const name = path.split("/").at(-1) || "";
  const publish = useMutation({
    mutationFn: (input: { workspaceId: string; path: string; title: string; expectedRevision: number }) =>
      api("POST", artifactsApi(universeId), input),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: [...artifactsKey(universeId), "list"] });
      onClose();
    },
    onError: async (error) => {
      if (error instanceof ApiError && error.status === 409) await tree.refetch();
    },
  });
  function submit(event: FormEvent) {
    event.preventDefault();
    if (!selectedFile || selectedFile.size > ARTIFACT_MAX_BYTES || !tree.data || publish.isPending) return;
    publish.mutate({ workspaceId: tree.data.workspace.workspaceId, expectedRevision: tree.data.workspace.revision, path, title: title.trim() || name });
  }
  return <Dialog open onOpenChange={open => { if (!open && !publish.isPending) onClose(); }}>
    <DialogContent>
      <DialogHeader>
        <DialogTitle>Publish artifact</DialogTitle>
        <DialogDescription>Share a snapshot with readers of this universe. Choose a file up to {formatUploadBytes(ARTIFACT_MAX_BYTES)}.</DialogDescription>
      </DialogHeader>
      <form onSubmit={submit} className="grid gap-4">
        <label className="grid gap-1.5 text-sm font-medium">Workspace
          <Select value={selected?.workspaceId ?? ""} disabled={publish.isPending || !workspaces.data?.length}
            onValueChange={value => { setSelection(value ?? ""); setPath(""); setTitle(""); publish.reset(); }}>
            <SelectTrigger aria-label="Workspace" className="w-full"><SelectValue placeholder="Choose a workspace">
              {selected?.displayName || selected?.workspaceId}
            </SelectValue></SelectTrigger>
            <SelectContent>{workspaces.data?.map(w =>
              <SelectItem key={w.workspaceId} value={w.workspaceId}>{w.displayName || w.workspaceId}</SelectItem>)}</SelectContent>
          </Select>
        </label>
        {workspaces.error && <ReadError error={workspaces.error} />}
        {workspaces.isSuccess && !selected && <p role="status" className="text-sm text-muted-foreground">
          {workspaces.data.length ? "This workspace is unavailable. Select another workspace." : "No workspaces available."}
        </p>}
        <label className="grid gap-1.5 text-sm font-medium">Workspace file
          <Select value={selectedFile?.path ?? ""} disabled={publish.isPending || !files.length}
            onValueChange={value => { setPath(value ?? ""); publish.reset(); }}>
            <SelectTrigger aria-label="Workspace file" className="w-full"><SelectValue placeholder="Choose a file" /></SelectTrigger>
            <SelectContent>{files.map(file => <SelectItem key={file.path} value={file.path} disabled={file.size > ARTIFACT_MAX_BYTES}>
              {file.path}{file.size > ARTIFACT_MAX_BYTES && ` (${formatUploadBytes(file.size)})`}
            </SelectItem>)}</SelectContent>
          </Select>
        </label>
        {tree.error && <ReadError error={tree.error} />}
        {tree.isSuccess && !files.length && <p role="status" className="text-sm text-muted-foreground">This workspace has no files.</p>}
        <label className="grid gap-1.5 text-sm font-medium">Title (optional)
          <Input value={title} maxLength={200} placeholder={name} disabled={publish.isPending} onChange={event => setTitle(event.target.value)} />
        </label>
        {publish.error && <p role="alert" className="text-sm text-destructive">{publish.error.message}</p>}
        <DialogFooter>
          <Button type="button" variant="outline" disabled={publish.isPending} onClick={onClose}>Cancel</Button>
          <Button type="submit" disabled={!selectedFile || selectedFile.size > ARTIFACT_MAX_BYTES || (title.trim() || name).length > 200 || tree.isFetching || publish.isPending}>
            {publish.isPending ? "Publishing…" : "Publish"}
          </Button>
        </DialogFooter>
      </form>
    </DialogContent>
  </Dialog>;
}

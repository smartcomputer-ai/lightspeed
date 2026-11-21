import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useSearchParams } from "react-router-dom";
import { formatUploadBytes } from "@lightspeed-ai/platform-shared";
import { useActiveUniverse, useDocumentTitle } from "@lightspeed-ai/platform-web/modules";
import { api } from "@lightspeed-ai/platform-web/modules/client";
import {
  Button, DocumentIcon, EmptyState, LoadingNote, PageHeader, ReadError, UniverseNotFound, fileIcon,
} from "@lightspeed-ai/platform-web/modules/ui";
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from "@lightspeed-ai/platform-web/modules/ui/dialog";
import type { Artifact } from "../contract";
import { artifactsApi, artifactsKey, publications } from "./queries";
import { PublishArtifactDialog } from "./PublishArtifactDialog";

export function ArtifactsPage() {
  const { universe, handle, isLoading } = useActiveUniverse();
  if (isLoading) return <LoadingNote />;
  if (!universe) return <UniverseNotFound handle={handle} />;
  return <Library key={universe.id} universeId={universe.id} />;
}

function Library({ universeId }: { universeId: string }) {
  const [params, setParams] = useSearchParams();
  const workspace = params.get("workspace");
  const library = useQuery(publications(universeId, workspace));
  const [publishing, setPublishing] = useState(false);
  const [retracting, setRetracting] = useState<Artifact | null>(null);
  useDocumentTitle([workspace]);
  return (
    <section className="mx-auto w-full max-w-4xl p-4 sm:p-6">
      <PageHeader title="Artifacts" description="Published snapshots of workspace files."
        actions={library.data?.permissions.publish && !library.error &&
          <Button onClick={() => setPublishing(true)}>Publish artifact</Button>} />
      {workspace && <div className="mb-4 flex items-center gap-3 text-sm">
        <span>Workspace <strong>{workspace}</strong></span>
        <Button variant="link" size="sm" onClick={() => setParams({})}>Show all artifacts</Button>
      </div>}
      {library.isPending && <LoadingNote />}
      {library.error && <ReadError error={library.error} />}
      {library.isSuccess && (library.data.artifacts.length === 0
        ? <EmptyState icon={DocumentIcon} title="No published artifacts" />
        : <ul className="divide-y rounded-lg border" aria-label="Published artifacts">
          {library.data.artifacts.map(artifact => {
            const Icon = fileIcon(artifact.path);
            return <li key={artifact.id} className="flex items-center gap-3 p-3 text-sm">
              <Icon className="size-5 shrink-0 text-muted-foreground" />
              <div className="min-w-0 flex-1">
                <Link className="break-words font-medium hover:underline" to={encodeURIComponent(artifact.id)}>{artifact.title}</Link>
                <p className="truncate text-xs text-muted-foreground" title={artifact.path}>{artifact.workspaceId} / {artifact.path}</p>
              </div>
              <span className="shrink-0 text-xs tabular-nums text-muted-foreground">{formatUploadBytes(artifact.sizeBytes)}</span>
              {library.data.permissions.retract && <Button size="sm" variant="ghost" aria-label={`Retract ${artifact.title}`}
                onClick={() => setRetracting(artifact)}>Retract</Button>}
            </li>;
          })}
        </ul>)}
      {publishing && library.data?.permissions.publish && !library.error &&
        <PublishArtifactDialog universeId={universeId} initialWorkspace={workspace} onClose={() => setPublishing(false)} />}
      {retracting && library.data?.permissions.retract && !library.error &&
        <RetractDialog universeId={universeId} artifact={retracting} onClose={() => setRetracting(null)} />}
    </section>
  );
}

function RetractDialog({ universeId, artifact, onClose }: { universeId: string; artifact: Artifact; onClose: () => void }) {
  const client = useQueryClient();
  const retract = useMutation({
    mutationFn: () => api("DELETE", `${artifactsApi(universeId)}/${encodeURIComponent(artifact.id)}`),
    onSuccess: async () => {
      client.removeQueries({ queryKey: [...artifactsKey(universeId), "content", artifact.id] });
      client.removeQueries({ queryKey: [...artifactsKey(universeId), "record", artifact.id] });
      await client.invalidateQueries({ queryKey: [...artifactsKey(universeId), "list"] });
      onClose();
    },
  });
  return <Dialog open onOpenChange={open => { if (!open && !retract.isPending) onClose(); }}>
    <DialogContent>
      <DialogHeader>
        <DialogTitle>Retract artifact?</DialogTitle>
        <DialogDescription>Remove “{artifact.title}” and its snapshot from the published library.</DialogDescription>
      </DialogHeader>
      {retract.error && <p role="alert" className="text-sm text-destructive">{retract.error.message}</p>}
      <DialogFooter>
        <Button variant="outline" disabled={retract.isPending} onClick={onClose}>Cancel</Button>
        <Button variant="destructive" disabled={retract.isPending} onClick={() => retract.mutate()}>
          {retract.isPending ? "Retracting…" : "Retract artifact"}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}

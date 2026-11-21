import { useMemo, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { CopiedIcon, CopyLinkIcon } from "@/icons";
import { validWorkspaceTransferPath } from "@lightspeed-ai/platform-shared";
import { api, type SessionView, type WorkspaceTree, type VfsTreeEntry } from "@/api";
import { LoadingNote, UniverseNotFound } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { Button } from "@/components/ui/button";
import { MarkdownViewToggle } from "@/components/markdown-view-toggle";
import { BlobPreview, BlobDownloadLink } from "@/components/blob-preview";
import { formatBytes } from "@/lib/format-bytes";
import { blobView, type BlobView } from "@/lib/blob-view";
import { useActionPermissions } from "@/lib/permissions";
import { useActiveUniverse } from "@/lib/universes";
import { useSessionPageHref } from "@/lib/web-modules";
import { useDocumentTitle } from "@/lib/document-title";

/// One stored blob, addressed by its digest within the universe: stored
/// text, instructions, catalogs, and media the model was shown. Links carry
/// a name, a type, and their source session or workspace as display hints.
export function BlobPage() {
  const { universe, handle, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  const { digest } = useParams<{ digest: string }>();
  if (isLoading) return <LoadingNote />;
  if (!universe || !permissions.can("read")) {
    return <div className="p-6"><UniverseNotFound handle={handle} /></div>;
  }
  if (!digest || !/^[a-f0-9]{64}$/.test(digest)) {
    return <p className="p-6 text-sm text-muted-foreground">This is not a blob address.</p>;
  }
  return <BlobDetail key={`${universe.id}/${digest}`} universeId={universe.id} handle={handle!} digest={digest} />;
}

function BlobDetail({ universeId, handle, digest }: { universeId: string; handle: string; digest: string }) {
  const [preview, setPreview] = useState(false);
  const [search] = useSearchParams();
  const name = search.get("name")?.trim() || undefined;
  const hint = search.get("type")?.trim() || undefined;
  const session = search.get("session")?.trim() || undefined;
  const workspace = search.get("workspace")?.trim() || undefined;
  const path = search.get("path") || undefined;
  const blob = useQuery({
    queryKey: ["blob", universeId, digest],
    queryFn: () => api<{ bytesBase64: string; bytes: number }>(
      "GET", `/api/ui/universes/${universeId}/blobs/${encodeURIComponent(`sha256:${digest}`)}`,
    ),
    staleTime: Infinity,
  });
  const bytes = useMemo(
    () => (blob.data ? Uint8Array.from(atob(blob.data.bytesBase64), (char) => char.charCodeAt(0)) : null),
    [blob.data],
  );
  const view = useMemo(() => (bytes ? blobView(bytes, hint) : null), [bytes, hint]);
  const title = name ?? `sha256:${digest.slice(0, 12)}…`;
  useDocumentTitle([title]);

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <header className="sticky top-0 z-10 flex min-h-12 shrink-0 flex-wrap items-center gap-3 border-b bg-background px-4 py-2">
        <div className="min-w-0 flex-1 basis-full sm:basis-auto">
          <div className="flex min-w-0 items-baseline gap-2">
            <h1 className="truncate text-sm font-semibold" title={title}>{title}</h1>
            <span className="shrink-0 text-xs text-muted-foreground">
              {[view && typeLabel(view), blob.data && formatBytes(blob.data.bytes)].filter(Boolean).join(" · ")}
            </span>
          </div>
          {session && <LinkedFrom universeId={universeId} handle={handle} sessionId={session} />}
          {workspace && <LinkedFromWorkspace universeId={universeId} handle={handle} workspaceId={workspace} path={path} />}
        </div>
        <CopyLink />
        {view?.kind === "text" && <MarkdownViewToggle preview={preview} onPreviewChange={setPreview} />}
        {bytes && view && <BlobDownloadLink bytes={bytes} name={name ?? `${digest.slice(0, 12)}${extension(view)}`} />}
      </header>
      <div className="min-h-0 flex-1 overflow-auto">
        {blob.isLoading && <LoadingNote />}
        {blob.error && <ReadError error={blob.error} className="p-6" />}
        {bytes && view && <BlobPreview bytes={bytes} view={view} name={title} preview={preview} />}
      </div>
    </div>
  );
}

/// The session a link was followed from. The page opens in a new tab, so
/// this is where it came from rather than a way back; a session the viewer
/// cannot read is named by its id without a link.
function LinkedFrom({ universeId, handle, sessionId }: { universeId: string; handle: string; sessionId: string }) {
  const sessionHref = useSessionPageHref(handle, sessionId);
  const source = useQuery({
    queryKey: ["session", universeId, sessionId],
    queryFn: () => api<SessionView>("GET", `/api/ui/universes/${universeId}/sessions/${encodeURIComponent(sessionId)}`),
    retry: false,
  });
  const label = source.data?.displayName?.trim() || `${sessionId.slice(0, 18)}${sessionId.length > 18 ? "…" : ""}`;
  return (
    <p className="truncate text-xs text-muted-foreground">
      Linked from session:{" "}
      {source.error || !sessionHref
        ? <span className="font-mono" title={sessionId}>{label}</span>
        : <Link to={sessionHref} title={label}
            className="font-medium text-foreground underline-offset-2 hover:underline">{label}</Link>}
    </p>
  );
}

function LinkedFromWorkspace({ universeId, handle, workspaceId, path }: {
  universeId: string; handle: string; workspaceId: string; path?: string;
}) {
  const source = useQuery({
    queryKey: ["workspace-tree", universeId, workspaceId],
    queryFn: () => api<WorkspaceTree>("GET", `/api/ui/universes/${universeId}/workspaces/${encodeURIComponent(workspaceId)}/tree`),
    retry: false,
  });
  const label = source.data?.workspace.displayName?.trim() || workspaceId;
  const workspaceHref = `/u/${handle}/workspaces/${encodeURIComponent(workspaceId)}`;
  let entry: VfsTreeEntry | undefined;
  if (source.data && path && validWorkspaceTransferPath(path)) {
    entry = { kind: "directory", entries: source.data.manifest.root.entries };
    for (const segment of path.split("/")) {
      entry = entry?.kind === "directory" && Object.hasOwn(entry.entries, segment)
        ? entry.entries[segment] : undefined;
    }
  }
  const available = source.data && !source.error;
  const linkClass = "font-medium text-foreground underline-offset-2 hover:underline";
  return (
    <p className="truncate text-xs text-muted-foreground">
      Linked from workspace:{" "}
      {available
        ? <Link to={workspaceHref} title={label} className={linkClass}>{label}</Link>
        : <span title={workspaceId}>{label}</span>}
      {path && <>{" / "}{available && entry?.kind === "file"
        ? <Link to={`${workspaceHref}/files/${path.split("/").map(encodeURIComponent).join("/")}`} title={path} className={linkClass}>{path}</Link>
        : <span title={path}>{path}</span>}</>}
    </p>
  );
}

function CopyLink() {
  const [copied, setCopied] = useState(false);
  return (
    <Button type="button" variant="ghost" size="sm" onClick={() => {
      void navigator.clipboard?.writeText(window.location.href).then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      });
    }}>
      {copied ? <CopiedIcon data-icon="inline-start" /> : <CopyLinkIcon data-icon="inline-start" />}
      {copied ? "Copied" : "Copy link"}
    </Button>
  );
}

const TEXT_LABELS: Record<string, string> = {
  "text/plain": "Text", "text/markdown": "Markdown", "text/csv": "CSV", "application/json": "JSON",
};
const TEXT_EXTENSIONS: Record<string, string> = {
  "text/markdown": ".md", "text/csv": ".csv", "application/json": ".json",
};

function typeLabel(view: BlobView): string {
  switch (view.kind) {
    case "image": return view.mime.replace("image/", "").toUpperCase();
    case "pdf": return "PDF";
    case "text": return TEXT_LABELS[view.mime] ?? view.mime;
    case "binary": return "Binary";
  }
}

function extension(view: BlobView): string {
  switch (view.kind) {
    case "image": return `.${view.mime.replace("image/", "").replace("jpeg", "jpg")}`;
    case "pdf": return ".pdf";
    case "text": return TEXT_EXTENSIONS[view.mime] ?? ".txt";
    case "binary": return "";
  }
}

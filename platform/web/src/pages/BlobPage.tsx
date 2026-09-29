import { useEffect, useMemo, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Check, Download, FileQuestion, Link2 } from "lucide-react";
import { api, type SessionView } from "@/api";
import { LoadingNote, UniverseNotFound } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { Button } from "@/components/ui/button";
import { formatBytes } from "@/lib/composer-attachments";
import { blobView, type BlobView } from "@/lib/blob-view";
import { useActionPermissions } from "@/lib/permissions";
import { useActiveUniverse } from "@/lib/universes";

/// One stored blob, addressed by its digest within the universe: stored
/// text, instructions, catalogs, and media the model was shown. Links carry
/// a name, a type, and the session they came from as display hints.
export function BlobPage({ admin: _admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  const { digest } = useParams<{ digest: string }>();
  if (isLoading) return <LoadingNote />;
  if (!universe || !permissions.can("read")) {
    return <div className="p-6"><UniverseNotFound slug={slug} /></div>;
  }
  if (!digest || !/^[a-f0-9]{64}$/.test(digest)) {
    return <p className="p-6 text-sm text-muted-foreground">This is not a blob address.</p>;
  }
  return <BlobDetail key={`${universe.id}/${digest}`} universeId={universe.id} slug={slug!} digest={digest} />;
}

function BlobDetail({ universeId, slug, digest }: { universeId: string; slug: string; digest: string }) {
  const [search] = useSearchParams();
  const name = search.get("name")?.trim() || undefined;
  const hint = search.get("type")?.trim() || undefined;
  const session = search.get("session")?.trim() || undefined;
  const blob = useQuery({
    queryKey: ["blob", universeId, digest],
    queryFn: () => api<{ bytesBase64: string; bytes: number }>(
      "GET", `/api/v1/universes/${universeId}/blobs/${encodeURIComponent(`sha256:${digest}`)}`,
    ),
    staleTime: Infinity,
  });
  const bytes = useMemo(
    () => (blob.data ? Uint8Array.from(atob(blob.data.bytesBase64), (char) => char.charCodeAt(0)) : null),
    [blob.data],
  );
  const view = useMemo(() => (bytes ? blobView(bytes, hint) : null), [bytes, hint]);
  const title = name ?? `sha256:${digest.slice(0, 12)}…`;
  useEffect(() => {
    const previous = document.title;
    document.title = title;
    return () => { document.title = previous; };
  }, [title]);

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <header className="sticky top-0 z-10 flex min-h-12 shrink-0 items-center gap-3 border-b bg-background px-4 py-2">
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-baseline gap-2">
            <h1 className="truncate text-sm font-semibold" title={title}>{title}</h1>
            <span className="shrink-0 text-xs text-muted-foreground">
              {[view && typeLabel(view), blob.data && formatBytes(blob.data.bytes)].filter(Boolean).join(" · ")}
            </span>
          </div>
          {session && <LinkedFrom universeId={universeId} slug={slug} sessionId={session} />}
        </div>
        <CopyLink />
        {bytes && view && <DownloadLink bytes={bytes} name={name ?? `${digest.slice(0, 12)}${extension(view)}`} />}
      </header>
      <div className="min-h-0 flex-1 overflow-auto">
        {blob.isLoading && <LoadingNote />}
        {blob.error && <ReadError error={blob.error} className="p-6" />}
        {bytes && view && <BlobBody bytes={bytes} view={view} name={title} />}
      </div>
    </div>
  );
}

/// The session a link was followed from. The page opens in a new tab, so
/// this is where it came from rather than a way back; a session the viewer
/// cannot read is named by its id without a link.
function LinkedFrom({ universeId, slug, sessionId }: { universeId: string; slug: string; sessionId: string }) {
  const source = useQuery({
    queryKey: ["session", universeId, sessionId],
    queryFn: () => api<SessionView>("GET", `/api/v1/universes/${universeId}/sessions/${encodeURIComponent(sessionId)}`),
    retry: false,
  });
  const label = source.data?.displayName?.trim() || `${sessionId.slice(0, 18)}${sessionId.length > 18 ? "…" : ""}`;
  return (
    <p className="truncate text-xs text-muted-foreground">
      Linked from session:{" "}
      {source.error
        ? <span className="font-mono" title={sessionId}>{label}</span>
        : <Link to={`/u/${slug}/sessions/${encodeURIComponent(sessionId)}`} title={label}
            className="font-medium text-foreground underline-offset-2 hover:underline">{label}</Link>}
    </p>
  );
}

function BlobBody({ bytes, view, name }: { bytes: Uint8Array; view: BlobView; name: string }) {
  const url = useObjectUrl(bytes, view.kind === "image" || view.kind === "pdf" ? view.mime : null);
  switch (view.kind) {
    case "image":
      return (
        <div className="flex justify-center p-6">
          {url && <img src={url} alt={name} className="max-w-full rounded-lg border bg-[repeating-conic-gradient(var(--muted)_0_25%,transparent_0_50%)] bg-size-[16px_16px] object-contain" />}
        </div>
      );
    case "pdf":
      return url ? <iframe src={url} title={name} className="block h-full min-h-[70vh] w-full border-0" /> : null;
    case "text":
      return <div className="mx-auto max-w-5xl px-6 py-6"><TextBlock text={view.text} /></div>;
    case "binary":
      return (
        <div className="flex flex-col items-center gap-2 p-12 text-sm text-muted-foreground">
          <FileQuestion className="size-6" />
          <p>This blob has no preview. Download it to open it.</p>
        </div>
      );
  }
}

function TextBlock({ text }: { text: string }) {
  return <pre className="font-mono text-[13px] leading-relaxed whitespace-pre-wrap [overflow-wrap:anywhere]">{text}</pre>;
}

/// An object URL for bytes the browser renders itself (images and PDFs),
/// revoked when the view changes or unmounts.
function useObjectUrl(bytes: Uint8Array, mime: string | null): string | null {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!mime || typeof URL.createObjectURL !== "function") return;
    const next = URL.createObjectURL(new Blob([bytes as BlobPart], { type: mime }));
    setUrl(next);
    return () => { URL.revokeObjectURL(next); setUrl(null); };
  }, [bytes, mime]);
  return url;
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
      {copied ? <Check data-icon="inline-start" /> : <Link2 data-icon="inline-start" />}
      {copied ? "Copied" : "Copy link"}
    </Button>
  );
}

/// Downloads always carry a generic type, so the browser saves the bytes
/// instead of rendering them as a page.
function DownloadLink({ bytes, name }: { bytes: Uint8Array; name: string }) {
  const url = useObjectUrl(bytes, "application/octet-stream");
  if (!url) return null;
  return (
    <Button variant="outline" size="sm" render={<a href={url} download={name} />}>
      <Download data-icon="inline-start" />
      Download
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

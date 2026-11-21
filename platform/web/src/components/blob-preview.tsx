import { useEffect, useState } from "react";
import { DownloadIcon, UnknownFileIcon } from "@/icons";
import { MarkdownContent } from "@/components/markdown-content";
import { Button } from "@/components/ui/button";
import type { BlobView } from "@/lib/blob-view";

export function BlobPreview({ bytes, view, name, preview = false }: { bytes: Uint8Array; view: BlobView; name: string; preview?: boolean }) {
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
      return <div className="mx-auto max-w-5xl px-6 py-6">
        {preview ? <MarkdownContent>{view.text}</MarkdownContent> : <TextBlock text={view.text} />}
      </div>;
    case "binary":
      return (
        <div className="flex flex-col items-center gap-2 p-12 text-sm text-muted-foreground">
          <UnknownFileIcon className="size-6" />
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

/// Downloads always carry a generic type, so the browser saves the bytes
/// instead of rendering them as a page.
export function BlobDownloadLink({ bytes, name }: { bytes: Uint8Array; name: string }) {
  const url = useObjectUrl(bytes, "application/octet-stream");
  if (!url) return null;
  return (
    <Button variant="outline" size="sm" nativeButton={false} render={<a href={url} download={name} />}>
      <DownloadIcon data-icon="inline-start" />
      Download
    </Button>
  );
}

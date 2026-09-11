import { useContext, useEffect, useState } from "react";
import { FileText, ImageOff } from "lucide-react";
import { TranscriptLinksContext } from "@/components/session/transcript-links";
import type { TranscriptMedia } from "@/lib/sessions/transcript";
import { cn } from "@/lib/utils";

/// Media the model was shown, rendered from CAS: image thumbnails that open
/// full size, and document chips that open the file. Bytes load lazily
/// through the page's blob reader and are cached per blob for the session.

const urlCache = new Map<string, Promise<string>>();

export function useMediaUrl(media: TranscriptMedia | null): string | null {
  const links = useContext(TranscriptLinksContext);
  const loadMedia = links.loadMedia;
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!media || !loadMedia) return;
    let cancelled = false;
    let promise = urlCache.get(media.blobRef);
    if (!promise) {
      promise = loadMedia(media.blobRef, media.mime);
      urlCache.set(media.blobRef, promise);
      promise.catch(() => urlCache.delete(media.blobRef));
    }
    promise.then((value) => { if (!cancelled) setUrl(value); }, () => { if (!cancelled) setUrl(null); });
    return () => { cancelled = true; };
  }, [media, loadMedia]);
  return url;
}

export function MediaStrip({ items, className }: { items: TranscriptMedia[]; className?: string }) {
  if (items.length === 0) return null;
  return (
    <div className={cn("flex min-w-0 max-w-full flex-wrap gap-2", className)} aria-label="Media">
      {items.map((media) => media.kind === "image"
        ? <MediaThumb key={`${media.handle}:${media.blobRef}`} media={media} />
        : <DocumentChip key={`${media.handle}:${media.blobRef}`} media={media} />)}
    </div>
  );
}

function mediaTitle(media: TranscriptMedia): string {
  return media.name ? `${media.name} · ${media.handle}` : media.handle;
}

export function MediaThumb({ media, className }: { media: TranscriptMedia; className?: string }) {
  const url = useMediaUrl(media);
  const title = mediaTitle(media);
  if (!url) {
    return (
      <div
        className={cn("flex h-24 w-32 items-center justify-center rounded-lg border bg-muted/40 text-muted-foreground", className)}
        title={title}
        aria-label={title}
      >
        <ImageOff className="size-4" />
      </div>
    );
  }
  return (
    <a href={url} target="_blank" rel="noreferrer" title={title} className={cn("block max-w-full", className)}>
      <img
        src={url}
        alt={media.name ?? media.handle}
        className="max-h-48 max-w-full rounded-lg border object-contain"
        loading="lazy"
      />
    </a>
  );
}

export function DocumentChip({ media }: { media: TranscriptMedia }) {
  const url = useMediaUrl(media);
  const label = media.name ?? media.handle;
  const body = (
    <>
      <FileText className="size-3.5 shrink-0" />
      <span className="truncate">{label}</span>
    </>
  );
  const className = "inline-flex max-w-full items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs";
  return url ? (
    <a href={url} target="_blank" rel="noreferrer" title={mediaTitle(media)} className={cn(className, "text-primary hover:bg-foreground/5")}>
      {body}
    </a>
  ) : (
    <span title={mediaTitle(media)} className={cn(className, "text-muted-foreground")}>{body}</span>
  );
}

/// An image the assistant referenced by handle in its text; the handle is
/// resolved against the media the transcript has folded so far.
export function MediaByHandle({ handle, alt, inline = false }: { handle: string; alt?: string; inline?: boolean }) {
  const links = useContext(TranscriptLinksContext);
  const media = links.mediaByHandle?.get(handle) ?? null;
  const url = useMediaUrl(media);
  if (!media) {
    return (
      <span className="rounded border border-dashed px-1 text-xs text-muted-foreground" title="This media is not part of the session">
        {alt || handle} (not found)
      </span>
    );
  }
  if (media.kind !== "image") {
    return <DocumentChip media={media} />;
  }
  if (!url) {
    return <span className="text-xs text-muted-foreground">{alt || media.name || handle}</span>;
  }
  return (
    <a href={url} target="_blank" rel="noreferrer" title={mediaTitle(media)} className={inline ? "inline-block align-middle" : "block"}>
      <img src={url} alt={alt || media.name || handle} className="max-h-96 max-w-full rounded-lg border object-contain" loading="lazy" />
    </a>
  );
}

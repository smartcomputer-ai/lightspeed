import { useContext, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { MarkdownContent as Markdown, type MarkdownReferenceProps } from "@lightspeed-ai/platform-web/modules/ui";
import { useMediaUrl } from "./media";
import { TranscriptLinksContext } from "./transcript-links";
import { isFileHandle, type FileReference } from "./model/file-references";

/** Resolve recorded session attachments when rendering transcript Markdown. */
export function MarkdownContent({ className, children }: { className?: string; children: string }) {
  return <Markdown className={className} renderReference={TranscriptReference}>{children}</Markdown>;
}

function TranscriptReference({ href, children, showImage = false, ...rest }: MarkdownReferenceProps) {
  const links = useContext(TranscriptLinksContext);
  if (typeof href === "string" && href.startsWith("file:")) {
    const file = isFileHandle(href) ? links.filesByHandle?.get(href) ?? undefined : undefined;
    if (isFileHandle(href) && links.fileReferenceSource) {
      return <HistoricalFileLink href={href} showImage={showImage} {...rest} data-attachment-reference={href}>{children}</HistoricalFileLink>;
    }
    return <FileLink file={file} showImage={showImage} {...rest} data-attachment-reference={href}>{children}</FileLink>;
  }
  const media = href && links.mediaByHandle?.get(href);
  if (media) {
    const target = links.blobHref?.(media.blobRef, { name: media.name, type: media.mime });
    return <AttachmentContent source={media} href={target ?? undefined} showImage={showImage && media.kind === "image"} {...rest} data-attachment-reference={href}>{children}</AttachmentContent>;
  }
  return (
    <span className="rounded border border-dashed px-1 text-xs text-muted-foreground" title="This media is not part of the session">
      {children} (not found)
    </span>
  );
}

function HistoricalFileLink({ href, children, ...rest }: MarkdownReferenceProps & { href: string }) {
  const links = useContext(TranscriptLinksContext);
  const source = links.fileReferenceSource!;
  const reference = useQuery({
    queryKey: ["file-reference", source.universeId, source.sessionId, href, links.filesByHandle?.get(href)],
    queryFn: ({ signal }) => source.load(href, signal),
    staleTime: Infinity,
    retry: false,
  });
  if (reference.isPending) return <span className="text-muted-foreground" title="Looking up file reference" aria-busy="true">{children}</span>;
  return <FileLink file={reference.data ?? undefined} {...rest}>{children}</FileLink>;
}

function FileLink({ file, children, ...rest }: MarkdownReferenceProps & { file?: FileReference }) {
  const links = useContext(TranscriptLinksContext);
  const target = file && links.blobHref?.(file.blobRef, {
    name: file.name, type: file.type,
    workspace: file.workspace, path: file.path,
  });
  return file
    ? <AttachmentContent source={{ blobRef: file.blobRef, mime: file.type ?? "application/octet-stream" }} href={target ?? undefined} {...rest}>{children}</AttachmentContent>
    : <span className="text-muted-foreground" title="This file reference is unavailable">{children} (unavailable)</span>;
}

/// Markdown chooses presentation; loading a preview here never changes model context.
function AttachmentContent({ source, href, showImage = false, children, ...rest }: MarkdownReferenceProps & {
  source: { blobRef: string; mime: string; localUrl?: string };
}) {
  const canPreview = showImage && (source.mime.startsWith("image/") || source.mime === "application/octet-stream");
  const url = useMediaUrl(canPreview || !href ? source : null);
  const [failedUrl, setFailedUrl] = useState<string | null>(null);
  const target = href || url;
  const preview = canPreview && url && failedUrl !== url;
  const content = preview
    ? <img src={url} alt={typeof children === "string" ? children : "Image"} onError={() => setFailedUrl(url)} className="max-h-96 max-w-full rounded-lg border object-contain" loading="lazy" />
    : children;
  return target
    ? <a {...rest} href={target} target="_blank" rel="noopener noreferrer">{content}</a>
    : <span className="text-muted-foreground">{children}</span>;
}

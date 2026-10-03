import Markdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ComponentProps } from "react";
import { useMediaUrl } from "@/components/session/media";
import { TranscriptLinksContext } from "@/components/session/transcript-links";
import { useContext, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { isFileHandle, type FileReference } from "@/lib/file-references";
import { cn } from "@/lib/utils";

const MEDIA_SCHEME = "media:";

/// Media and file handles name recorded tool assets; keep those URLs so
/// the renderer below can resolve them, and sanitize everything else.
function urlTransform(url: string): string {
  return url.startsWith(MEDIA_SCHEME) || url.startsWith("file:") ? url : defaultUrlTransform(url);
}

type ReferenceProps = ComponentProps<"a"> & { showImage?: boolean };

function MarkdownImage({ src, alt }: ComponentProps<"img">) {
  if (typeof src === "string" && (src.startsWith("file:") || src.startsWith(MEDIA_SCHEME))) {
    return <MarkdownLink href={src} showImage>{alt ?? "Image"}</MarkdownLink>;
  }
  return <img src={src} alt={alt} />;
}

function MarkdownLink({ href, children, showImage = false, ...rest }: ReferenceProps) {
  const links = useContext(TranscriptLinksContext);
  if (typeof href === "string" && href.startsWith("file:")) {
    const file = isFileHandle(href) ? links.filesByHandle?.get(href) ?? undefined : undefined;
    if (isFileHandle(href) && links.fileReferenceSource) {
      return <HistoricalFileLink href={href} showImage={showImage} {...rest} data-attachment-reference={href}>{children}</HistoricalFileLink>;
    }
    return <FileLink file={file} showImage={showImage} {...rest} data-attachment-reference={href}>{children}</FileLink>;
  }
  if (typeof href === "string" && href.startsWith(MEDIA_SCHEME)) {
    const media = links.mediaByHandle?.get(href);
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
  return <a href={href} target="_blank" rel="noreferrer" {...rest}>{children}</a>;
}

function HistoricalFileLink({ href, children, ...rest }: ReferenceProps & { href: string }) {
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

function FileLink({ file, children, ...rest }: ReferenceProps & { file?: FileReference }) {
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
function AttachmentContent({ source, href, showImage = false, children, ...rest }: ReferenceProps & {
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

export function MarkdownContent({
  className,
  children,
}: {
  className?: string;
  children: string;
}) {
  return (
    <div
      className={cn(
        "min-w-0 max-w-full text-sm leading-relaxed [overflow-wrap:anywhere]",
        "[&_p]:my-3 [&_p:first-child]:mt-0 [&_p:last-child]:mb-0",
        "[&_h1]:mb-3 [&_h1]:mt-5 [&_h1]:text-xl [&_h1]:font-semibold [&_h1:first-child]:mt-0",
        "[&_h2]:mb-2 [&_h2]:mt-5 [&_h2]:text-lg [&_h2]:font-semibold [&_h2:first-child]:mt-0",
        "[&_h3]:mb-2 [&_h3]:mt-4 [&_h3]:font-semibold [&_h3:first-child]:mt-0",
        "[&_ul]:my-3 [&_ul]:list-disc [&_ul]:space-y-1 [&_ul]:pl-6",
        "[&_ol]:my-3 [&_ol]:list-decimal [&_ol]:space-y-1 [&_ol]:pl-6",
        "[&_li>p]:my-0",
        "[&_a]:font-medium [&_a]:text-primary [&_a]:underline [&_a]:underline-offset-4",
        "[&_blockquote]:my-3 [&_blockquote]:border-l-2 [&_blockquote]:pl-4 [&_blockquote]:text-muted-foreground",
        "[&_hr]:my-5 [&_hr]:border-border",
        "[&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:py-0.5 [&_code]:font-mono [&_code]:text-[0.875em]",
        "[&_pre]:my-3 [&_pre]:min-w-0 [&_pre]:max-w-full [&_pre]:overflow-x-auto [&_pre]:rounded-lg [&_pre]:border [&_pre]:bg-muted/50 [&_pre]:p-3 [&_pre]:text-xs [&_pre]:leading-relaxed",
        "[&_pre_code]:bg-transparent [&_pre_code]:p-0 [&_pre_code]:text-xs",
        "[&_table]:my-3 [&_table]:block [&_table]:max-w-full [&_table]:overflow-x-auto [&_table]:border-collapse",
        "[&_th]:border [&_th]:bg-muted/50 [&_th]:px-3 [&_th]:py-2 [&_th]:text-left [&_th]:font-medium",
        "[&_td]:border [&_td]:px-3 [&_td]:py-2",
        "[&_img]:my-3 [&_img]:max-w-full [&_img]:rounded-lg",
        className,
      )}
    >
      <Markdown remarkPlugins={[remarkGfm]} urlTransform={urlTransform} components={{ img: MarkdownImage, a: MarkdownLink }}>
        {children}
      </Markdown>
    </div>
  );
}

import Markdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import { createContext, useContext, type ComponentProps, type ComponentType } from "react";
import { cn } from "@/lib/utils";

const MEDIA_SCHEME = "media:";

/// Preserve asset handles for the optional reference renderer.
function urlTransform(url: string): string {
  return url.startsWith(MEDIA_SCHEME) || url.startsWith("file:") ? url : defaultUrlTransform(url);
}

export type MarkdownReferenceProps = ComponentProps<"a"> & { showImage?: boolean };

const ReferenceContext = createContext<ComponentType<MarkdownReferenceProps> | undefined>(undefined);

/// Whether the content being rendered sits inside a Markdown link.
const InsideLinkContext = createContext(false);

/// The host an address leads to when that is not this app, resolved the way
/// the browser would (`//host/path` names another host too); null for an
/// address on this origin or one that names no host.
function foreignHost(url: string): string | null {
  try {
    const resolved = new URL(url, window.location.href);
    return resolved.origin === window.location.origin ? null : resolved.host || null;
  } catch {
    return null;
  }
}

/// Loading an image chosen by a document would reveal the reader to its host.
/// Show an ordinary link for remote images and let the caller resolve recorded assets.
function MarkdownImage({ src, alt }: ComponentProps<"img">) {
  const insideLink = useContext(InsideLinkContext);
  if (typeof src === "string" && (src.startsWith("file:") || src.startsWith(MEDIA_SCHEME))) {
    return <MarkdownLink href={src} showImage>{alt ?? "Image"}</MarkdownLink>;
  }
  const label = alt || "Image";
  // An address the sanitizer emptied leaves nothing to open.
  if (typeof src !== "string" || src === "") return <span className="text-muted-foreground">{label}</span>;
  const host = foreignHost(src);
  const text = host ? `${label} (${host})` : label;
  // A link cannot hold another link: the surrounding one keeps its target.
  return insideLink ? <span>{text}</span> : <MarkdownLink href={src}>{text}</MarkdownLink>;
}

function MarkdownAnchor(props: MarkdownReferenceProps) {
  return <InsideLinkContext.Provider value={true}><MarkdownLink {...props} /></InsideLinkContext.Provider>;
}

/** File and media handles are inert unless the caller can resolve them. */
function MarkdownLink({ href, children, showImage, ...rest }: MarkdownReferenceProps) {
  const Reference = useContext(ReferenceContext);
  if (href?.startsWith("file:") || href?.startsWith(MEDIA_SCHEME)) {
    if (Reference) return <Reference href={href} showImage={showImage} {...rest}>{children}</Reference>;
    return href.startsWith("file:")
      ? <span className="text-muted-foreground" title="This file reference is unavailable">{children} (unavailable)</span>
      : <span className="rounded border border-dashed px-1 text-xs text-muted-foreground">{children} (not found)</span>;
  }
  return <a href={href} target="_blank" rel="noreferrer" {...rest}>{children}</a>;
}

export function MarkdownContent({
  renderReference,
  className,
  children,
}: {
  renderReference?: ComponentType<MarkdownReferenceProps>;
  className?: string;
  children: string;
}) {
  return (
    <div
      className={cn(
        "min-w-0 max-w-full cursor-select text-sm leading-relaxed [overflow-wrap:anywhere]",
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
      <ReferenceContext.Provider value={renderReference}>
        <Markdown remarkPlugins={[remarkGfm]} urlTransform={urlTransform} components={{ img: MarkdownImage, a: MarkdownAnchor }}>
          {children}
        </Markdown>
      </ReferenceContext.Provider>
    </div>
  );
}

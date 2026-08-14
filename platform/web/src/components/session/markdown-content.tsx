import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { cn } from "@/lib/utils";

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
      <Markdown remarkPlugins={[remarkGfm]}>{children}</Markdown>
    </div>
  );
}

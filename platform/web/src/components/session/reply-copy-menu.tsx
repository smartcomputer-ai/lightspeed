import { useEffect, useState, type RefObject } from "react";
import { Check, ChevronDown, Copy, TriangleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { replyFormatted, replyMarkdown } from "./reply-copy";

export function ReplyCopyMenu({ contentRef, text, citations }: {
  contentRef: RefObject<HTMLDivElement | null>;
  text: string;
  citations?: Array<{ url: string; title?: string | null }>;
}) {
  const [feedback, setFeedback] = useState("");
  const [copying, setCopying] = useState(false);
  useEffect(() => {
    if (!feedback) return;
    const timer = window.setTimeout(() => setFeedback(""), 4000);
    return () => window.clearTimeout(timer);
  }, [feedback]);

  async function copy(formatted: boolean) {
    const content = contentRef.current;
    if (!content || copying) return;
    setCopying(true);
    setFeedback("");
    try {
      if (formatted) {
        const { html, text: plain } = replyFormatted(content);
        await navigator.clipboard.write([new ClipboardItem({
          "text/html": new Blob([html], { type: "text/html" }),
          "text/plain": new Blob([plain], { type: "text/plain" }),
        })]);
      } else {
        await navigator.clipboard.writeText(replyMarkdown(text, content, citations));
      }
      setFeedback("Copied");
    } catch (error) {
      setFeedback(error instanceof Error && error.message.startsWith("An attachment")
        ? error.message : "Could not copy. Try again or check clipboard permissions.");
    } finally {
      setCopying(false);
    }
  }

  return (
    <div className="absolute right-0 top-0 z-10">
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button variant="ghost" size="xs" aria-label="Copy reply" title={feedback || "Copy reply"}
          className="h-6 gap-1 px-1.5 text-muted-foreground opacity-0 group-hover/message:opacity-100 group-focus-within/message:opacity-100 aria-expanded:opacity-100 hover:text-foreground focus-visible:text-foreground aria-expanded:text-foreground"
          disabled={copying} />}>
          {feedback === "Copied" ? <Check className="size-3.5 text-emerald-600 dark:text-emerald-400" />
            : feedback ? <TriangleAlert className="size-3.5 text-destructive" /> : <Copy className="size-3.5" />}
          <ChevronDown className="size-3" />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-44">
          <DropdownMenuItem onClick={() => void copy(false)}>Copy as Markdown</DropdownMenuItem>
          <DropdownMenuItem onClick={() => void copy(true)}>Copy formatted</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <span role="status" className={feedback && feedback !== "Copied"
        ? "absolute right-0 top-7 w-56 rounded-md border bg-popover px-2 py-1 text-xs text-popover-foreground shadow-md"
        : "sr-only"}>{feedback}</span>
    </div>
  );
}

import { useEffect, useState } from "react";
import { Check, Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/// Copies text, falling back to a hidden textarea where the async clipboard
/// API is unavailable (plain-HTTP origins other than localhost).
export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.append(area);
    area.select();
    const copied = document.execCommand("copy");
    area.remove();
    if (!copied) throw new Error("Copy failed");
  }
}

/// A titled monospace block with a copy button in its header.
export function CodeCopy({ code, title, label = "Copy", className }: { code: string; title: string; label?: string; className?: string }) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  useEffect(() => {
    if (state === "idle") return;
    const timer = setTimeout(() => setState("idle"), 1600);
    return () => clearTimeout(timer);
  }, [state]);
  return (
    <div className={cn("grid min-w-0 overflow-hidden rounded-lg border bg-muted/50", className)}>
      <div className="flex items-center justify-between gap-2 border-b px-3 py-1.5">
        <span className="text-xs font-medium text-muted-foreground">{title}</span>
        <Button
          size="xs"
          variant="outline"
          onClick={() => copyText(code).then(() => setState("copied"), () => setState("failed"))}
        >
          {state === "copied" ? <Check data-icon="inline-start" /> : <Copy data-icon="inline-start" />}
          {state === "copied" ? "Copied" : state === "failed" ? "Copy failed" : label}
        </Button>
      </div>
      <pre className="max-h-80 overflow-auto p-3 font-mono text-xs leading-relaxed whitespace-pre-wrap wrap-anywhere">{code}</pre>
    </div>
  );
}

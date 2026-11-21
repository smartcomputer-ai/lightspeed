import { useState, type ReactNode } from "react";
import { LoadingIcon } from "@/icons";
import { Button } from "@/components/ui/button";

export type FullTextLoader = (blobRef: string) => Promise<string>;

export function ExpandableContent({
  text,
  truncated = false,
  blobRef,
  loadFullText,
  children,
}: {
  text: string;
  truncated?: boolean;
  blobRef?: string;
  loadFullText?: FullTextLoader;
  children: (text: string) => ReactNode;
}) {
  const [fullText, setFullText] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const expandable = truncated && Boolean(blobRef && loadFullText);

  const expand = () => {
    if (!blobRef || !loadFullText || loading) return;
    setLoading(true);
    setError(null);
    void loadFullText(blobRef)
      .then(setFullText)
      .catch((reason: unknown) => {
        setError(reason instanceof Error ? reason.message : String(reason));
      })
      .finally(() => setLoading(false));
  };

  return (
    <div className="min-w-0">
      {children(fullText ?? text)}
      {truncated && fullText === null && (
        <div className="mt-1.5 flex items-center gap-2 text-xs text-muted-foreground">
          <span>Truncated preview.</span>
          {expandable && (
            <Button variant="link" size="xs" className="h-auto px-0" disabled={loading} onClick={expand}>
              {loading && <LoadingIcon className="animate-spin" />}
              {loading ? "Loading…" : "Expand full entry"}
            </Button>
          )}
        </div>
      )}
      {error && <p className="mt-1 text-xs text-destructive">Could not expand: {error}</p>}
    </div>
  );
}

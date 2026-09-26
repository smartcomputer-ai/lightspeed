import { useEffect, useState } from "react";
import { Check, Copy } from "lucide-react";
import {
  DropdownMenuGroup,
  DropdownMenuLabel,
  DropdownMenuSeparator,
} from "@/components/ui/dropdown-menu";

export function sessionMenuMetadataEntries(
  metadata: Record<string, string> | undefined,
): Array<[string, string]> {
  return Object.entries(metadata ?? {}).sort(([left], [right]) =>
    left.localeCompare(right),
  );
}

/** Compact, read-only session identity shared by the bot and Sessions menus. */
export function SessionMenuIdentity({ sessionId }: { sessionId: string }) {
  return <MenuIdentity noun="session" id={sessionId} />;
}

/** A menu's read-only id with a copy button, labelled by what it names. */
export function MenuIdentity({ noun, id }: { noun: string; id: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1_500);
    return () => window.clearTimeout(timer);
  }, [copied]);
  return (
    <DropdownMenuGroup>
      <DropdownMenuLabel>{capitalized(noun)} ID</DropdownMenuLabel>
      <div className="flex min-w-0 items-center gap-2 px-2 pb-1.5">
        <code
          className="block min-w-0 flex-1 truncate text-xs text-foreground"
          title={id}
        >
          {id}
        </code>
        <button
          type="button"
          className="grid size-6 shrink-0 place-items-center rounded-sm text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
          aria-label={copied ? `${capitalized(noun)} id copied` : `Copy ${noun} id`}
          title={copied ? "Copied" : `Copy ${noun} id`}
          onClick={() => {
            void navigator.clipboard
              .writeText(id)
              .then(() => setCopied(true))
              .catch(() => undefined);
          }}
        >
          {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
        </button>
      </div>
    </DropdownMenuGroup>
  );
}

function capitalized(word: string): string {
  return word.charAt(0).toUpperCase() + word.slice(1);
}

/** Optional metadata appendix; includes its own separator for last position. */
export function SessionMenuMetadata({ metadata }: { metadata?: Record<string, string> }) {
  const entries = sessionMenuMetadataEntries(metadata);
  if (entries.length === 0) return null;
  return (
    <>
      <DropdownMenuSeparator />
      <DropdownMenuGroup>
        <DropdownMenuLabel>Metadata</DropdownMenuLabel>
        <div className="grid min-w-0 gap-1 px-2 pb-1.5">
          {entries.map(([key, value]) => (
            <div
              key={key}
              className="grid min-w-0 grid-cols-[minmax(0,2fr)_minmax(0,3fr)] items-baseline gap-2 text-xs"
            >
              <code className="min-w-0 truncate text-muted-foreground" title={key}>
                {key}
              </code>
              <span className="min-w-0 truncate text-foreground" title={value}>
                {value}
              </span>
            </div>
          ))}
        </div>
      </DropdownMenuGroup>
    </>
  );
}

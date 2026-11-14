import { createContext, useCallback, useContext, useEffect, useId, useMemo, useState, type ReactNode } from "react";

/// The browser tab's title is the navigation path read backwards: what is
/// open, then the section and universe that hold it, then the app, e.g.
/// "Explain the auth middleware / Sessions / Software Factory – Lightspeed".

/// Between two segments of the path.
export const TITLE_SEPARATOR = " / ";

/// Between the path and the app name.
export const TITLE_APP_SEPARATOR = " – ";

/// The end of every title, after the path.
export const TITLE_APP_NAME = "Lightspeed";

/// A longer segment (a session named after its first prompt) is cut to this
/// many characters, ellipsis included, so the rest of the path stays visible.
export const TITLE_SEGMENT_MAX_LENGTH = 48;

export type TitleSegment = string | null | undefined | false;

function shorten(segment: string): string {
  if (segment.length <= TITLE_SEGMENT_MAX_LENGTH) return segment;
  return `${segment.slice(0, TITLE_SEGMENT_MAX_LENGTH - 1).trimEnd()}…`;
}

/// The title for a trail, most specific segment first. Empty segments are
/// dropped and the app name is appended; an empty trail is the app alone.
export function documentTitle(trail: readonly TitleSegment[]): string {
  const path = trail
    .map((segment) => (typeof segment === "string" ? segment.replace(/\s+/g, " ").trim() : ""))
    .filter((segment) => segment.length > 0)
    .map(shorten)
    .join(TITLE_SEPARATOR);
  return path ? `${path}${TITLE_APP_SEPARATOR}${TITLE_APP_NAME}` : TITLE_APP_NAME;
}

interface TitleRegistry {
  set(id: string, trail: readonly string[] | null): void;
}

const DocumentTitleContext = createContext<TitleRegistry | null>(null);

/// Owns the title for everything rendered inside it. `base` is the part the
/// shell knows from the route (section, universe); the open page adds what
/// only it knows through `useDocumentTitle`.
export function DocumentTitleProvider({ base, children }: { base: readonly TitleSegment[]; children: ReactNode }) {
  const [entries, setEntries] = useState<ReadonlyArray<{ id: string; trail: readonly string[] }>>([]);
  const set = useCallback((id: string, trail: readonly string[] | null) => {
    setEntries((current) => {
      const others = current.filter((entry) => entry.id !== id);
      return trail ? [...others, { id, trail }] : others;
    });
  }, []);
  const registry = useMemo(() => ({ set }), [set]);
  // The newest registration wins, so a page leaving and the next arriving
  // in one commit cannot leave the old page's title behind.
  const detail = entries.at(-1)?.trail ?? [];
  const title = documentTitle([...detail, ...base]);
  useEffect(() => {
    document.title = title;
  }, [title]);
  return <DocumentTitleContext.Provider value={registry}>{children}</DocumentTitleContext.Provider>;
}

/// Names what this page shows, most specific segment first. Inside a
/// `DocumentTitleProvider` the segments go before the provider's base;
/// outside one (sign-in, tool pages) they are the whole title. An empty
/// trail inside a provider claims nothing, so a pane embedded in another
/// page can opt out and leave that page's title standing.
export function useDocumentTitle(trail: readonly TitleSegment[]): void {
  const registry = useContext(DocumentTitleContext);
  const id = useId();
  const segments = trail.filter((segment): segment is string => typeof segment === "string" && segment.trim().length > 0);
  const key = JSON.stringify(segments);
  useEffect(() => {
    const current = JSON.parse(key) as string[];
    if (!registry) {
      document.title = documentTitle(current);
      return;
    }
    if (current.length === 0) return;
    registry.set(id, current);
    return () => registry.set(id, null);
  }, [registry, id, key]);
}

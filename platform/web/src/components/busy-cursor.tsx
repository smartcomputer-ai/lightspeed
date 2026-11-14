import { useEffect } from "react";
import { useIsMutating } from "@tanstack/react-query";

/// The page's busy cursor. While any change is being saved the page shows
/// the progress cursor (--cursor-progress) and stays usable; while one that
/// blocks the UI runs (a mutation with `meta: { blocking: true }`, e.g.
/// closing or deleting a session) the whole page shows the wait cursor
/// (--cursor-wait). Sets `data-cursor` on <html>; index.css draws it.
export function BusyCursor() {
  const blocking = useIsMutating({ predicate: (mutation) => mutation.options.meta?.blocking === true });
  const working = useIsMutating();
  const cursor = blocking > 0 ? "wait" : working > 0 ? "progress" : null;
  useEffect(() => {
    const root = document.documentElement;
    if (cursor) root.dataset.cursor = cursor;
    else delete root.dataset.cursor;
    return () => { delete root.dataset.cursor; };
  }, [cursor]);
  return null;
}

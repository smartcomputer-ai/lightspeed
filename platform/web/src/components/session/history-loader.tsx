import { useEffect, useRef } from "react";
import type { SessionTail } from "@/lib/sessions/tail";
import { ReadError } from "@/components/read-error";

/** Lives outside MessageScrollerContent: a persistent first child inside that
 * element would hide prepends from the primitive's first-message detection.
 */
export function SessionHistoryLoader({ tail }: { tail: SessionTail }) {
  const sentinel = useRef<HTMLDivElement>(null);
  const { hasOlder, loadingOlder, loadOlder, historyRevision, phase } = tail;
  useEffect(() => {
    const marker = sentinel.current;
    const viewport = marker?.closest<HTMLElement>('[data-slot="message-scroller-viewport"]');
    if (!marker || !viewport || !hasOlder || loadingOlder || phase !== "live") return;
    let frame = 0;
    const check = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const atEnd = viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop <= 8;
        const fillsViewport = viewport.scrollHeight > viewport.clientHeight + 8;
        // Opening at the bottom must not eagerly drain history. Short windows
        // may fetch enough older content to fill the viewport.
        if (atEnd && fillsViewport) return;
        if (marker.getBoundingClientRect().bottom >= viewport.getBoundingClientRect().top - 800) loadOlder();
      });
    };
    const observer = typeof IntersectionObserver === "undefined" ? null : new IntersectionObserver(check, {
      root: viewport, rootMargin: "800px 0px 0px 0px",
    });
    observer?.observe(marker);
    viewport.addEventListener("scroll", check, { passive: true });
    check();
    return () => {
      observer?.disconnect();
      viewport.removeEventListener("scroll", check);
      cancelAnimationFrame(frame);
    };
  }, [hasOlder, loadingOlder, loadOlder, historyRevision, phase]);

  return (
    <div ref={sentinel} className="relative h-px shrink-0" data-history-sentinel>
      {(loadingOlder || tail.historyError) && (
        <div className="absolute inset-x-0 top-1 text-center">
          {tail.historyError
            ? <ReadError error={tail.historyError} transient={tail.historyErrorIsTransient} retrying loading graceMs={10_000} prefix="Earlier history" className="text-xs" />
            : <span role="status" className="text-xs text-muted-foreground">Loading earlier history…</span>}
        </div>
      )}
    </div>
  );
}

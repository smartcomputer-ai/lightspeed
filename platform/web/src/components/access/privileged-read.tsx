import { Eye } from "lucide-react";

/** Shown only when the response carrying this view relied on privileged access. */
export function PrivilegedReadMarker({ privileged }: { privileged?: boolean }) {
  if (!privileged) return null;
  return (
    <span
      role="note"
      className="inline-flex shrink-0 items-center gap-1 text-xs text-amber-700 dark:text-amber-400"
      title="This view includes content read through private-content access. These reads are recorded in the access audit."
    >
      <Eye className="size-3" aria-hidden="true" />
      Privileged read
    </span>
  );
}

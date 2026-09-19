import { useEffect, useState } from "react";
import { isTransientReadError } from "@/lib/read-errors";
import { cn } from "@/lib/utils";

/** Keep brief network interruptions quiet without delaying actionable errors. */
export function ReadError({ error, transient = isTransientReadError(error), retrying = false, loading = false, graceMs = 3_000, prefix, className }: {
  error: unknown;
  transient?: boolean;
  retrying?: boolean;
  loading?: boolean;
  graceMs?: number;
  prefix?: string;
  className?: string;
}) {
  const waiting = Boolean(error) && transient;
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    if (!waiting) {
      setVisible(false);
      return;
    }
    const timer = setTimeout(() => setVisible(true), graceMs);
    return () => clearTimeout(timer);
  }, [waiting, graceMs]);
  if (!error || (transient && !visible)) {
    return loading ? <p className={cn("text-sm text-muted-foreground", className)}>Loading…</p> : null;
  }
  const message = transient
    ? retrying ? "Connection lost — retrying…" : "Connection unavailable."
    : error instanceof Error ? error.message : String(error);
  return <p role={transient ? "status" : "alert"} className={cn("text-sm", transient ? "text-muted-foreground" : "text-destructive", className)}>
    {prefix && `${prefix}: `}{message}
  </p>;
}

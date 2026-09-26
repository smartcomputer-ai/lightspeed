import { useEffect, useId, useState, type ReactNode } from "react";

import { cn } from "@/lib/utils";

/**
 * Optional settings with good defaults. Closed, one quiet line states in
 * words what is in effect, followed by a text link that reveals the fields
 * inline: "Default limits · Customize limits". Callers name every
 * non-default value in the summary, so closing never hides a customization.
 *
 * It starts closed, including when editing saved values, and opens on its
 * own only while a field inside is invalid (`forceOpen`). Fields that were
 * invalid stay in view after they are fixed.
 *
 * Wording: "Change" for a single choice, "Customize <noun>" for a group.
 */
export function SettingsDisclosure({
  summary,
  action,
  label,
  forceOpen = false,
  open: controlledOpen,
  onOpenChange,
  className,
  contentClassName,
  children,
}: {
  /** What is in effect, in words. */
  summary?: ReactNode;
  /** The link text while closed. Open, the link reads "Hide". */
  action: string;
  /** A stable accessible name when the visible link alone is ambiguous. */
  label?: string;
  /** Open while a field inside has a validation error; the link cannot hide it. */
  forceOpen?: boolean;
  /** Controlled open state, for callers that do work only while open. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  className?: string;
  contentClassName?: string;
  children: ReactNode;
}) {
  const contentId = useId();
  const [uncontrolledOpen, setUncontrolledOpen] = useState(false);
  const open = controlledOpen ?? uncontrolledOpen;
  const setOpen = (next: boolean) => {
    if (controlledOpen === undefined) setUncontrolledOpen(next);
    onOpenChange?.(next);
  };
  useEffect(() => {
    if (forceOpen && !open) setOpen(true);
  }, [forceOpen, open]);
  const shown = open || forceOpen;
  const hasSummary =
    summary !== undefined && summary !== null && summary !== false && summary !== "";
  return (
    <div
      data-slot="settings-disclosure"
      className={cn("grid min-w-0 gap-3", className)}
    >
      <p className="flex min-w-0 flex-wrap items-baseline gap-x-1.5 gap-y-0.5 text-xs text-muted-foreground">
        {hasSummary && (
          <>
            <span className="min-w-0 wrap-anywhere">{summary}</span>
            <span aria-hidden="true">·</span>
          </>
        )}
        <button
          type="button"
          aria-expanded={shown}
          aria-controls={contentId}
          aria-label={label}
          disabled={forceOpen}
          onClick={() => setOpen(!open)}
          className="w-fit shrink-0 cursor-pointer rounded-sm text-left text-xs text-muted-foreground underline decoration-muted-foreground/40 underline-offset-4 outline-none hover:text-foreground hover:decoration-current focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-default disabled:no-underline disabled:opacity-60"
        >
          {shown ? "Hide" : action}
        </button>
      </p>
      {shown && (
        <div
          id={contentId}
          className={cn("grid min-w-0 gap-4", contentClassName)}
        >
          {children}
        </div>
      )}
    </div>
  );
}

/** A small heading over one group of fields inside an open disclosure. */
export function SettingsGroup({
  title,
  description,
  className,
  children,
}: {
  title: string;
  description?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  const titleId = useId();
  return (
    <div
      role="group"
      aria-labelledby={titleId}
      className={cn("grid min-w-0 gap-3", className)}
    >
      <div className="grid gap-0.5">
        <p
          id={titleId}
          className="text-xs font-medium tracking-wide text-muted-foreground uppercase"
        >
          {title}
        </p>
        {description && (
          <p className="text-xs text-muted-foreground">{description}</p>
        )}
      </div>
      {children}
    </div>
  );
}

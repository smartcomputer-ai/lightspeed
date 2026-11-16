import { useEffect, useRef, type ComponentProps } from "react"

import { FailedIcon, SelectedIcon } from "@/icons"
import { AnimatedLoadingIcon } from "@/components/ui/animated-loading-icon"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

type SaveState = "dirty" | "error" | "saving" | "saved"

/// A Save button that shows whether there is anything to save: "Save"
/// while there are unsaved changes (`dirty`), "Saving…" while a save runs,
/// and "Saved" when there is nothing to save. It saves only while dirty:
/// saving, it stays primary and ignores clicks; saved, it turns secondary
/// and ignores clicks. Neither is disabled, so neither fades, but both say
/// they are unavailable. `children` names the action when "Save" alone is
/// not enough, such as "Save changes".
///
/// With `error` (the last save failed) it reads "Save failed" in the
/// destructive colours, with the failed icon pulsing at its right end; it
/// can be pressed to try again.
///
/// Its width never changes: every state's label is laid out in the same
/// place, only the current one visible, so the button is as wide as the
/// widest of them. "Save failed" is the exception: it is laid out only
/// while shown, so the button grows for it if it has to.
///
/// At its right end is a check. While a save runs the check fades out and
/// the animated loading icon begins in its place; when the save finishes
/// the loading icon stops and the check grows in from a fifth of its size
/// and fades in (not with reduced motion, and not when it shows "Saved"
/// from the start).
function SaveButton({
  dirty,
  saving = false,
  error = false,
  disabled,
  // Primary while there is something to save, in a group too.
  variant = "default",
  className,
  onClick,
  children = "Save",
  ...props
}: Omit<ComponentProps<typeof Button>, "children"> & {
  dirty: boolean
  saving?: boolean
  /// The last save failed.
  error?: boolean
  children?: string
}) {
  const state: SaveState = saving ? "saving" : error ? "error" : dirty ? "dirty" : "saved"
  const labels: Record<SaveState, string> = { dirty: children, error: "Save failed", saving: "Saving…", saved: "Saved" }
  // Pressing saves (or tries again) only with changes to save.
  const ready = state === "dirty" || state === "error"
  // The state of the last render, to tell a save that just finished.
  const previous = useRef(state)
  const justSaved = state === "saved" && previous.current === "saving"
  useEffect(() => {
    previous.current = state
  })
  return (
    <Button
      data-save-state={state}
      aria-busy={saving || undefined}
      aria-disabled={(!ready && !disabled) || undefined}
      disabled={disabled}
      variant={state === "saved" ? "secondary" : state === "error" ? "destructive" : variant}
      className={cn(
        // Saving or saved, it does not look pressable: no hover, no press.
        !ready && "cursor-default active:not-aria-[haspopup]:translate-y-0",
        state === "saving" && "hover:bg-primary",
        state === "saved" && "hover:bg-secondary",
        className
      )}
      onClick={(event) => {
        // Nothing to save: the click (or Enter, or a form's submit) does nothing.
        if (!ready) {
          event.preventDefault()
          return
        }
        onClick?.(event)
      }}
      {...props}
    >
      {/* Left-aligned: a shorter label starts where the longest does. */}
      <span className="grid text-left">
        {(Object.keys(labels) as SaveState[]).filter((each) => each !== "error" || state === "error").map((each) => (
          <span
            key={each}
            data-save-label={each}
            aria-hidden={each !== state || undefined}
            className={cn("col-start-1 row-start-1", each !== state && "invisible")}
          >
            {labels[each]}
          </span>
        ))}
      </span>
      {/* The check, the loading icon and the failed icon share the right end:
          the check fades out while a save runs and the loading icon spins in
          its place; a failed save shows the failed icon, pulsing. */}
      <span data-icon="inline-end" className="grid">
        <SelectedIcon
          aria-hidden="true"
          data-just-saved={justSaved || undefined}
          className={cn(
            "col-start-1 row-start-1 transition-opacity duration-300 motion-reduce:transition-none",
            (state === "saving" || state === "error") && "opacity-0",
            justSaved && "motion-safe:animate-in motion-safe:fade-in-0 motion-safe:zoom-in-20 motion-safe:duration-300 motion-safe:ease-out"
          )}
        />
        <AnimatedLoadingIcon active={saving} className="col-start-1 row-start-1" />
        <FailedIcon
          aria-hidden="true"
          className={cn("col-start-1 row-start-1", state === "error" ? "motion-safe:animate-pulse" : "invisible")}
        />
      </span>
    </Button>
  )
}

export { SaveButton }

import { useState } from "react"

import { LoadingIcon } from "@/icons"
import { cn } from "@/lib/utils"

/// The loading icon, spinning smoothly, that comes and goes: when `active`
/// begins it scales up from a fifth of its size and fades in while it
/// spins; when it stops it fades out and scales back down, still spinning,
/// and then rests hidden. Also when it is first shown active. Under
/// reduced motion it neither spins nor scales: it just shows and hides.
///
/// The spin and the scale are on two elements, so the one never disturbs
/// the other. Hidden, it keeps its space (so a button keeps its width).
function AnimatedLoadingIcon({ active, className }: { active: boolean; className?: string }) {
  // Spinning while shown and while it fades out; resting once gone.
  const [fadingOut, setFadingOut] = useState(false)
  const [wasActive, setWasActive] = useState(active)
  if (wasActive !== active) {
    setWasActive(active)
    setFadingOut(!active)
  }
  return (
    <span
      data-slot="animated-loading-icon"
      data-active={active || undefined}
      aria-hidden="true"
      onTransitionEnd={(event) => {
        if (event.target === event.currentTarget && !active) setFadingOut(false)
      }}
      className={cn(
        "inline-flex transition-[opacity,scale] duration-300 ease-out motion-reduce:transition-none",
        active
          ? "scale-100 opacity-100 starting:scale-20 starting:opacity-0"
          : "scale-20 opacity-0",
        className
      )}
    >
      <LoadingIcon className={cn((active || fadingOut) && "motion-safe:animate-spin")} />
    </span>
  )
}

export { AnimatedLoadingIcon }

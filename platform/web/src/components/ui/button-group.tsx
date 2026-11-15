import type { ComponentProps, MouseEvent, PointerEvent } from "react"

import { ButtonGroupContext, type ButtonVariant } from "@/components/ui/button"
import { cn } from "@/lib/utils"

/// Marks the button the pointer just pressed, until the pointer leaves it.
const JUST_PRESSED = "data-just-pressed"

const buttonOf = (target: EventTarget | null, group: HTMLElement) => {
  const button = target instanceof Element ? target.closest("[data-slot=button]") : null
  return button && button.parentElement === group ? button : null
}

/// A row of buttons that belong together, such as a set of actions or the
/// choices of a toggle. The buttons stick to each other: the corners where
/// one meets the next are square, and a hairline gap shows between filled
/// buttons (outlined ones share a border). Its buttons are `secondary`
/// unless the group's `variant`, or a button's own, says otherwise.
///
/// As a toggle, the chosen button is the one marked `aria-pressed`: it is
/// filled with the primary colour, and the others stay as they are. A
/// choice just clicked shows no hover until the pointer leaves it and
/// comes back, so the click shows the choice itself, not a dimmed one.
///
/// With `checkMarks`, each toggle button ends with a check: full for the
/// choice and faint for the others, so every choice reads as one to pick.
/// Off by default, when the buttons keep no space for it.
function ButtonGroup({
  className,
  variant = "secondary",
  checkMarks = false,
  onClick,
  onPointerOut,
  ...props
}: ComponentProps<"div"> & { variant?: ButtonVariant; checkMarks?: boolean }) {
  const pressed = (event: MouseEvent<HTMLDivElement>) => {
    onClick?.(event)
    // detail is 0 for a click from the keyboard, where there is no hover.
    if (event.detail > 0) buttonOf(event.target, event.currentTarget)?.setAttribute(JUST_PRESSED, "")
  }
  const left = (event: PointerEvent<HTMLDivElement>) => {
    onPointerOut?.(event)
    const button = buttonOf(event.target, event.currentTarget)
    if (button && !(event.relatedTarget instanceof Node && button.contains(event.relatedTarget))) {
      button.removeAttribute(JUST_PRESSED)
    }
  }
  return (
    <ButtonGroupContext.Provider value={{ variant, checkMarks }}>
      <div
        role="group"
        data-slot="button-group"
        onClick={pressed}
        onPointerOut={left}
        className={cn(
          "flex w-fit items-stretch",
          // Joined: only the outer corners are round, and neighbours share
          // the border between them.
          "*:data-[slot=button]:not-first:rounded-l-none *:data-[slot=button]:not-first:border-l-0 *:data-[slot=button]:not-last:rounded-r-none",
          // A focus ring is drawn over the neighbours, not under them.
          "*:data-[slot=button]:focus-visible:relative *:data-[slot=button]:focus-visible:z-10",
          // The chosen button of a toggle, dimmed on hover unless just clicked.
          "*:data-[slot=button]:aria-pressed:bg-primary *:data-[slot=button]:aria-pressed:text-primary-foreground *:data-[slot=button]:aria-pressed:not-data-just-pressed:hover:bg-primary/80",
          className
        )}
        {...props}
      />
    </ButtonGroupContext.Provider>
  )
}

export { ButtonGroup }

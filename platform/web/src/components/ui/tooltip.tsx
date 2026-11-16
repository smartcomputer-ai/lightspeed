"use client"

import { useEffect, useRef, useState, type ReactElement, type ReactNode } from "react"
import { Tooltip as TooltipPrimitive } from "@base-ui/react/tooltip"

import { cn } from "@/lib/utils"
import { NOTCH_CLASS } from "@/components/ui/notch"

function TooltipProvider({
  delay = 0,
  ...props
}: TooltipPrimitive.Provider.Props) {
  return (
    <TooltipPrimitive.Provider
      data-slot="tooltip-provider"
      delay={delay}
      {...props}
    />
  )
}

function Tooltip({ ...props }: TooltipPrimitive.Root.Props) {
  return <TooltipPrimitive.Root data-slot="tooltip" {...props} />
}

function TooltipTrigger({ ...props }: TooltipPrimitive.Trigger.Props) {
  return <TooltipPrimitive.Trigger data-slot="tooltip-trigger" {...props} />
}

function TooltipContent({
  className,
  side = "top",
  sideOffset = 8,
  align = "center",
  alignOffset = 0,
  children,
  ...props
}: TooltipPrimitive.Popup.Props &
  Pick<
    TooltipPrimitive.Positioner.Props,
    "align" | "alignOffset" | "side" | "sideOffset"
  >) {
  return (
    <TooltipPrimitive.Portal>
      <TooltipPrimitive.Positioner
        align={align}
        alignOffset={alignOffset}
        side={side}
        sideOffset={sideOffset}
        className="isolate z-50"
      >
        <TooltipPrimitive.Popup
          data-slot="tooltip-content"
          className={cn(
            "z-50 inline-flex w-fit max-w-xs origin-(--transform-origin) duration-(--popover-transition-duration) items-center gap-1.5 rounded-(--surface-popover-radius) bg-(--surface-popover) surface-filter-popover px-3 py-1.5 text-xs text-popover-foreground shadow-(--surface-popover-shadow) inset-ring-(length:--surface-popover-border-width) inset-ring-(color:--surface-popover-border) has-data-[slot=kbd]:pr-1.5 data-[side=bottom]:slide-in-from-top-2 data-[side=inline-end]:slide-in-from-left-2 data-[side=inline-start]:slide-in-from-right-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2 **:data-[slot=kbd]:relative **:data-[slot=kbd]:isolate **:data-[slot=kbd]:z-50 **:data-[slot=kbd]:rounded-sm data-[state=delayed-open]:animate-in data-[state=delayed-open]:fade-in-0 data-[state=delayed-open]:zoom-in-95 data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95",
            className
          )}
          {...props}
        >
          {children}
          {/* The popover surface and notch: tooltips look like small popovers. */}
          <TooltipPrimitive.Arrow className={cn(NOTCH_CLASS, "z-50")} />
        </TooltipPrimitive.Popup>
      </TooltipPrimitive.Positioner>
    </TooltipPrimitive.Portal>
  )
}

/// The app's tooltip on one element, instead of the browser's `title`:
/// `children` (a button, or a menu's trigger) is rendered as the tooltip's
/// trigger. Without `content`, the element alone. While the element has its
/// popup open (a ⋯ opening its menu: aria-expanded), the tooltip is off, so
/// it does not cover the menu.
function WithTooltip({
  content,
  side,
  children,
}: {
  content?: ReactNode
  side?: TooltipPrimitive.Positioner.Props["side"]
  children: ReactElement
}) {
  const trigger = useRef<HTMLButtonElement>(null)
  const [expanded, setExpanded] = useState(false)
  useEffect(() => {
    const element = trigger.current
    if (!element) return
    const read = () => setExpanded(element.getAttribute("aria-expanded") === "true")
    read()
    const observer = new MutationObserver(read)
    observer.observe(element, { attributes: true, attributeFilter: ["aria-expanded"] })
    return () => observer.disconnect()
  }, [content])
  if (!content) return children
  return (
    <Tooltip disabled={expanded}>
      <TooltipTrigger ref={trigger} render={children} />
      <TooltipContent side={side}>{content}</TooltipContent>
    </Tooltip>
  )
}

export { Tooltip, TooltipTrigger, TooltipContent, TooltipProvider, WithTooltip }

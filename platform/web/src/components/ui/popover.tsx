import * as React from "react"
import { Popover as PopoverPrimitive } from "@base-ui/react/popover"

import { cn } from "@/lib/utils"
import { NOTCH_CLASS, NOTCH_OFFSET } from "@/components/ui/notch"

function Popover({ ...props }: PopoverPrimitive.Root.Props) {
  return <PopoverPrimitive.Root data-slot="popover" {...props} />
}

function PopoverTrigger({ ...props }: PopoverPrimitive.Trigger.Props) {
  return <PopoverPrimitive.Trigger data-slot="popover-trigger" {...props} />
}

/// A popover. It points at the element that opened it with a notch on its
/// edge (over that element's centre, so near the popover's start or end
/// corner when aligned to it); `notch={false}` leaves it off.
function PopoverContent({
  align = "start",
  alignOffset = 0,
  side = "bottom",
  notch = true,
  sideOffset = notch ? NOTCH_OFFSET : 6,
  className,
  ...props
}: PopoverPrimitive.Popup.Props &
  Pick<
    PopoverPrimitive.Positioner.Props,
    "align" | "alignOffset" | "side" | "sideOffset"
  > & { notch?: boolean }) {
  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Positioner
        className="isolate z-50 outline-none"
        align={align}
        alignOffset={alignOffset}
        side={side}
        sideOffset={sideOffset}
      >
        <PopoverPrimitive.Popup
          data-slot="popover-content"
          className={cn(
            "max-h-(--available-height) w-[min(24rem,calc(100vw-2rem))] origin-(--transform-origin) overflow-y-auto rounded-(--surface-popover-radius) bg-(--surface-popover) surface-filter-popover text-popover-foreground shadow-(--surface-popover-shadow) inset-ring-(length:--surface-popover-border-width) inset-ring-(color:--surface-popover-border) outline-none duration-(--popover-transition-duration) data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95",
            className
          )}
          {...props}
        />
        {/* Beside the popup, not in it: the popup scrolls and would clip it. */}
        {notch && (
          <PopoverPrimitive.Arrow
            data-slot="popover-notch"
            className={cn(NOTCH_CLASS, "duration-(--popover-transition-duration) data-open:animate-in data-open:fade-in-0 data-closed:animate-out data-closed:fade-out-0")}
          />
        )}
      </PopoverPrimitive.Positioner>
    </PopoverPrimitive.Portal>
  )
}

export { Popover, PopoverContent, PopoverTrigger }

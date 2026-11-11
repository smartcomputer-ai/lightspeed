import * as React from "react"
import { ContextMenu as ContextMenuPrimitive } from "@base-ui/react/context-menu"

import { MENU_POPUP_CLASS } from "@/components/ui/menu-styles"
import { cn } from "@/lib/utils"

/// A menu opened by right-click on its trigger area (or a long press on a
/// touch screen), at the pointer. It looks like a dropdown menu and takes
/// the same items (DropdownMenuItem, DropdownMenuSeparator, …).

function ContextMenu({ ...props }: ContextMenuPrimitive.Root.Props) {
  return <ContextMenuPrimitive.Root data-slot="context-menu" {...props} />
}

/// The area a right-click opens the menu in.
function ContextMenuTrigger({ ...props }: ContextMenuPrimitive.Trigger.Props) {
  return <ContextMenuPrimitive.Trigger data-slot="context-menu-trigger" {...props} />
}

function ContextMenuContent({
  className,
  finalFocus,
  ...props
}: ContextMenuPrimitive.Popup.Props) {
  return (
    <ContextMenuPrimitive.Portal>
      <ContextMenuPrimitive.Positioner className="isolate z-50 outline-none">
        <ContextMenuPrimitive.Popup
          data-slot="context-menu-content"
          finalFocus={finalFocus}
          className={cn(MENU_POPUP_CLASS, className)}
          {...props}
        />
      </ContextMenuPrimitive.Positioner>
    </ContextMenuPrimitive.Portal>
  )
}

export { ContextMenu, ContextMenuContent, ContextMenuTrigger }

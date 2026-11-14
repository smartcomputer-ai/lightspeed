"use client"

import * as React from "react"
import { Dialog as DialogPrimitive } from "@base-ui/react/dialog"

import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"
import { DialogCloseIcon } from "@/icons";
import { Starscape } from "@/components/starscape/starscape"

/// How long a dialog's warp runs: it shrinks away, the starscape rushes up
/// to warp, and from 0.5s the backdrop and starscape fade out over 1s. By
/// then nothing shows, and its caller closes it.
export const DIALOG_WARP_MS = 1500

function Dialog({ ...props }: DialogPrimitive.Root.Props) {
  return <DialogPrimitive.Root data-slot="dialog" {...props} />
}

function DialogTrigger({ ...props }: DialogPrimitive.Trigger.Props) {
  return <DialogPrimitive.Trigger data-slot="dialog-trigger" {...props} />
}

function DialogPortal({ ...props }: DialogPrimitive.Portal.Props) {
  return <DialogPrimitive.Portal data-slot="dialog-portal" {...props} />
}

function DialogClose({ ...props }: DialogPrimitive.Close.Props) {
  return <DialogPrimitive.Close data-slot="dialog-close" {...props} />
}

function DialogOverlay({
  className,
  ...props
}: DialogPrimitive.Backdrop.Props) {
  return (
    <DialogPrimitive.Backdrop
      data-slot="dialog-overlay"
      className={cn(
        "fixed inset-0 isolate z-50 bg-(--surface-backdrop) duration-(--dialog-transition-duration) surface-filter-backdrop data-open:animate-in data-open:fade-in-0 data-closed:animate-out data-closed:fade-out-0",
        className
      )}
      {...props}
    />
  )
}

/// A dialog over a backdrop. `starscape` puts the Lightspeed starscape on
/// the backdrop, centred on the dialog and fading out toward the edges (for
/// moments that start something, e.g. a new session); the backdrop's tint
/// shows through it, and in the light theme it is drawn dark. `warp` sends
/// it off: the dialog shrinks away, the starscape rushes up to warp, and
/// everything fades out while it does (see DIALOG_WARP_MS, after which the
/// caller closes it). Turned off before then, the dialog comes back.
function DialogContent({
  className,
  children,
  showCloseButton = true,
  starscape = false,
  warp = false,
  ...props
}: DialogPrimitive.Popup.Props & {
  showCloseButton?: boolean
  starscape?: boolean
  warp?: boolean
}) {
  return (
    <DialogPortal>
      <DialogOverlay
        className={cn(
          // The fade starts by itself, mid-warp, rather than waiting for the
          // close (and whatever else happens then).
          warp
            ? "opacity-0 [transition:opacity_1000ms_ease-in-out_500ms]"
            : "[transition:opacity_300ms_ease-out]"
        )}
      >
        {starscape && (
          <Starscape
            warp={warp}
            transparentBackground
            fade={{ start: 160, end: 760 }}
            className="pointer-events-none absolute inset-0"
          />
        )}
      </DialogOverlay>
      <DialogPrimitive.Popup
        data-slot="dialog-content"
        className={cn(
          "fixed top-1/2 left-1/2 z-50 grid w-full max-w-[calc(100%-2rem)] -translate-x-1/2 -translate-y-1/2 gap-6 rounded-(--surface-dialog-radius) bg-(--surface-dialog) surface-filter-dialog p-6 text-sm text-popover-foreground shadow-(--surface-dialog-shadow) inset-ring-(length:--surface-dialog-border-width) inset-ring-(color:--surface-dialog-border) duration-(--dialog-transition-duration) outline-none sm:max-w-md data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95",
          className,
          // Warping, it shrinks away into the stars (and grows back if the
          // warp is called off).
          // (Plain transitions: a duration utility would also set the open and
          // close animations'.)
          warp
            // (With reduced motion it only fades; there is no starscape then.)
            ? "pointer-events-none scale-0 opacity-0 [transition:scale_400ms_ease-in,opacity_400ms_ease-in] motion-reduce:scale-100"
            : "[transition:scale_400ms_ease-out,opacity_400ms_ease-out]"
        )}
        {...props}
      >
        {children}
        {showCloseButton && (
          <DialogPrimitive.Close
            data-slot="dialog-close"
            render={
              <Button
                variant="ghost"
                className="absolute top-4 right-4"
                size="icon-sm"
              />
            }
          >
            <DialogCloseIcon
            />
            <span className="sr-only">Close</span>
          </DialogPrimitive.Close>
        )}
      </DialogPrimitive.Popup>
    </DialogPortal>
  )
}

function DialogHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="dialog-header"
      className={cn("flex flex-col gap-2", className)}
      {...props}
    />
  )
}

function DialogFooter({
  className,
  showCloseButton = false,
  children,
  ...props
}: React.ComponentProps<"div"> & {
  showCloseButton?: boolean
}) {
  return (
    <div
      data-slot="dialog-footer"
      className={cn(
        "flex flex-col-reverse gap-2 sm:flex-row sm:justify-end",
        className
      )}
      {...props}
    >
      {children}
      {showCloseButton && (
        <DialogPrimitive.Close render={<Button variant="outline" />}>
          Close
        </DialogPrimitive.Close>
      )}
    </div>
  )
}

function DialogTitle({ className, ...props }: DialogPrimitive.Title.Props) {
  return (
    <DialogPrimitive.Title
      data-slot="dialog-title"
      className={cn("font-heading leading-none font-semibold", className)}
      {...props}
    />
  )
}

function DialogDescription({
  className,
  ...props
}: DialogPrimitive.Description.Props) {
  return (
    <DialogPrimitive.Description
      data-slot="dialog-description"
      className={cn(
        "text-sm text-muted-foreground *:[a]:underline *:[a]:underline-offset-3 *:[a]:hover:text-foreground",
        className
      )}
      {...props}
    />
  )
}

export {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogOverlay,
  DialogPortal,
  DialogTitle,
  DialogTrigger,
}

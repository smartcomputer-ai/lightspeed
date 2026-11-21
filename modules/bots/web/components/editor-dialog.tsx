import type { ReactNode } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@lightspeed-ai/platform-web/modules/ui/dialog";
import { cn } from "@lightspeed-ai/platform-web/modules/ui";

export function BotEditorDialog({
  open,
  onOpenChange,
  icon,
  title,
  description,
  contentClassName,
  starscape = false,
  warp = false,
  children,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  icon?: ReactNode;
  title: string;
  description: string;
  contentClassName?: string;
  /** The starscape on the backdrop, for a dialog that creates (see DialogContent). */
  starscape?: boolean;
  /** Sends it off into the stars (see DialogContent). */
  warp?: boolean;
  children: ReactNode;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        starscape={starscape}
        warp={warp}
        className={cn(
          "flex h-dvh w-screen max-w-none flex-col gap-0 overflow-hidden rounded-none p-0 sm:h-[min(90dvh,900px)] sm:w-[calc(100%-2rem)] sm:max-w-6xl sm:rounded-xl",
          contentClassName,
        )}
      >
        <DialogHeader className="shrink-0 border-b px-5 py-4 pr-14">
          <div className="flex min-w-0 items-center gap-3">
            {icon && <div className="shrink-0">{icon}</div>}
            <div className="min-w-0">
              <DialogTitle>{title}</DialogTitle>
              <DialogDescription>{description}</DialogDescription>
            </div>
          </div>
        </DialogHeader>
        {children}
      </DialogContent>
    </Dialog>
  );
}

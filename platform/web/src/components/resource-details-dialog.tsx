import { useRef, type ReactNode, type RefObject } from "react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

export function ResourceDetailsDialog({ open, onOpenChange, title, description, icon, finalFocus, children }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: ReactNode;
  icon?: ReactNode;
  finalFocus?: RefObject<HTMLElement | null>;
  children: ReactNode;
}) {
  const contentRef = useRef<HTMLDivElement | null>(null);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent ref={contentRef} initialFocus={contentRef}
        className="flex max-h-[min(90dvh,860px)] flex-col gap-0 overflow-hidden p-0 sm:max-w-2xl" finalFocus={finalFocus}>
        <DialogHeader className="shrink-0 border-b p-6 pr-14">
          <DialogTitle className="flex min-w-0 items-center gap-2">
            {icon && <span aria-hidden="true" className="shrink-0">{icon}</span>}
            <span className="min-w-0 break-words">{title}</span>
          </DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        <div className="grid min-h-0 gap-6 overflow-y-auto p-6">{children}</div>
      </DialogContent>
    </Dialog>
  );
}

export function ResourceDetailsGrid({ children }: { children: ReactNode }) {
  return <dl className="grid gap-x-8 gap-y-4 sm:grid-cols-2">{children}</dl>;
}

export function ResourceDetail({ label, children, mono = false }: { label: string; children: ReactNode; mono?: boolean }) {
  return (
    <div className="min-w-0">
      <dt className="text-xs font-medium text-muted-foreground">{label}</dt>
      <dd className={cn("mt-1 break-words text-sm [overflow-wrap:anywhere]", mono && "font-mono text-xs")}>{children}</dd>
    </div>
  );
}

export function ResourceActions({ children }: { children: ReactNode }) {
  return <div className="flex flex-wrap items-center gap-2 border-t pt-4">{children}</div>;
}

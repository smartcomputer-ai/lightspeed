import { useRef, type ReactNode } from "react";
import { ChevronRight, LoaderCircle } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Table, TableActionsCell, TableBody, TableCard, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import type { ResourceStatusInfo } from "@/lib/resource-status";
import { cn } from "@/lib/utils";

/** Inventory rows share their hierarchy and interaction; columns keep their domain meaning. */
export function ResourceTable({ label, columns, children }: {
  label: string;
  columns: string[];
  children: ReactNode;
}) {
  return (
    <TableCard>
      <Table aria-label={label} className="[&_td]:px-3 [&_td]:py-3 [&_th]:px-3">
        <TableHeader>
          <TableRow>
            {columns.map((column, index) => <TableHead key={column} scope="col"
              className={index > 0 && index < columns.length - 1 ? "hidden sm:table-cell" : undefined}>{column}</TableHead>)}
            <TableHead scope="col" className="w-0"><span className="sr-only">Details</span></TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>{children}</TableBody>
      </Table>
    </TableCard>
  );
}

export function ResourceRow({ name, onOpen, children }: {
  name: string;
  onOpen: (trigger: HTMLButtonElement | null) => void;
  children: ReactNode;
}) {
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  return (
    <TableRow className="cursor-pointer focus-within:bg-muted/50" onClick={() => onOpen(triggerRef.current)}>
      {children}
      <TableActionsCell>
        <Button ref={triggerRef} variant="ghost" size="icon-sm" className="text-muted-foreground"
          aria-label={`Details for ${name}`} aria-haspopup="dialog">
          <ChevronRight aria-hidden="true" className="size-4" />
        </Button>
      </TableActionsCell>
    </TableRow>
  );
}

export function ResourceIdentityCell({ name, subtitle, mono = false, icon, compactDescription }: {
  name: string;
  subtitle?: string;
  mono?: boolean;
  icon?: ReactNode;
  compactDescription?: string;
}) {
  const secondary = subtitle?.trim().toLocaleLowerCase() !== name.trim().toLocaleLowerCase() ? subtitle : undefined;
  return (
    <TableCell className="max-w-40 sm:max-w-72">
      <div className="flex min-w-0 items-center gap-3">
        {icon && <span aria-hidden="true" className="shrink-0">{icon}</span>}
        <div className="grid min-w-0 gap-0.5">
          <span className="truncate font-medium" title={name}>{name}</span>
          {secondary && <span className={cn("truncate text-xs text-muted-foreground", mono && "font-mono", compactDescription && "hidden sm:block")} title={secondary}>{secondary}</span>}
          {compactDescription && <span className="truncate text-xs text-muted-foreground sm:hidden" title={compactDescription}>{compactDescription}</span>}
        </div>
      </div>
    </TableCell>
  );
}

export function ResourceValue({ children, description, mono = false }: {
  children: string;
  description?: string;
  mono?: boolean;
}) {
  return (
    <div className="grid min-w-0 gap-0.5 text-muted-foreground">
      <span className={cn("truncate", mono && "font-mono text-xs")} title={children}>{children}</span>
      {description && <span className="truncate text-xs" title={description}>{description}</span>}
    </div>
  );
}

export function ResourceStatusBadge({ status }: { status: ResourceStatusInfo }) {
  return (
    <Badge variant={status.tone === "positive" ? "secondary" : "outline"} className={cn(
      status.tone === "warning" && "border-amber-600/35 text-amber-700 dark:text-amber-400",
      status.tone === "danger" && "border-destructive/50 text-destructive",
      status.tone === "neutral" && "text-muted-foreground",
    )}>
      {status.tone === "pending" && <LoaderCircle aria-hidden="true" className="animate-spin motion-reduce:animate-none" />}
      {status.label}
    </Badge>
  );
}

export function ResourceStatus({ status }: { status: ResourceStatusInfo }) {
  return (
    <div className="grid gap-1">
      <ResourceStatusBadge status={status} />
      {status.description && <p className="max-w-56 whitespace-normal text-xs text-muted-foreground">{status.description}</p>}
    </div>
  );
}

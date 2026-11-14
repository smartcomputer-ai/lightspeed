import * as React from "react"

import { cn } from "@/lib/utils"

/// The card surface around a table: the card's background, radius, border
/// and shadow tokens, as Card. The border is a real border rather than
/// Card's inset ring, so row hover backgrounds stop at it instead of
/// painting over it. `Table` itself owns the horizontal scroll container,
/// so this must not add another `overflow-x-auto` (nested scrollers clip
/// the border radius and double the scrollbars).
function TableCard({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="table-card"
      className={cn(
        "min-w-0 max-w-full overflow-hidden rounded-(--surface-card-radius) border-(length:--surface-card-border-width) border-(--surface-card-border) bg-(--surface-card) text-card-foreground shadow-(--surface-card-shadow)",
        className
      )}
      {...props}
    />
  )
}

/// Primary label with an optional secondary line (typically the mono id):
/// keeps "name + id" in one column instead of two, which is what makes most
/// settings tables fit the page column.
function TableTitleCell({
  title,
  subtitle,
  className,
  ...props
}: React.ComponentProps<"td"> & { title: React.ReactNode; subtitle?: React.ReactNode }) {
  return (
    <TableCell className={cn("max-w-72", className)} {...props}>
      <div className="grid min-w-0 gap-0.5">
        <span className="block min-w-0 truncate font-medium">{title}</span>
        {subtitle != null && subtitle !== "" && (
          <IdText className="text-muted-foreground">{subtitle}</IdText>
        )}
      </div>
    </TableCell>
  )
}

/// Monospace identifier that truncates instead of widening its column; the
/// full value stays available as a tooltip.
function IdText({
  className,
  children,
  title,
  ...props
}: React.ComponentProps<"span">) {
  return (
    <span
      data-slot="id-text"
      title={title ?? (typeof children === "string" ? children : undefined)}
      className={cn("block min-w-0 max-w-full cursor-select truncate font-mono text-xs", className)}
      {...props}
    >
      {children}
    </span>
  )
}

/// Trailing actions column: never wraps, never widens beyond its content.
function TableActionsCell({ className, ...props }: React.ComponentProps<"td">) {
  return (
    <TableCell
      className={cn("w-0 whitespace-nowrap text-right [&>*]:align-middle", className)}
      {...props}
    />
  )
}

function Table({ className, ...props }: React.ComponentProps<"table">) {
  return (
    <div
      data-slot="table-container"
      className="relative w-full min-w-0 max-w-full overflow-x-auto"
    >
      <table
        data-slot="table"
        className={cn("w-full caption-bottom text-sm", className)}
        {...props}
      />
    </div>
  )
}

function TableHeader({ className, ...props }: React.ComponentProps<"thead">) {
  return (
    <thead
      data-slot="table-header"
      className={cn("[&_tr]:border-b", className)}
      {...props}
    />
  )
}

function TableBody({ className, ...props }: React.ComponentProps<"tbody">) {
  return (
    <tbody
      data-slot="table-body"
      className={cn("[&_tr:last-child]:border-0", className)}
      {...props}
    />
  )
}

function TableFooter({ className, ...props }: React.ComponentProps<"tfoot">) {
  return (
    <tfoot
      data-slot="table-footer"
      className={cn(
        "border-t bg-muted/50 font-medium [&>tr]:last:border-b-0",
        className
      )}
      {...props}
    />
  )
}

function TableRow({ className, ...props }: React.ComponentProps<"tr">) {
  return (
    <tr
      data-slot="table-row"
      className={cn(
        "border-b transition-colors hover:bg-muted/50 has-aria-expanded:bg-muted/50 data-[state=selected]:bg-muted",
        className
      )}
      {...props}
    />
  )
}

function TableHead({ className, ...props }: React.ComponentProps<"th">) {
  return (
    <th
      data-slot="table-head"
      className={cn(
        "h-10 px-2 text-left align-middle font-medium whitespace-nowrap text-foreground [&:has([role=checkbox])]:pr-0",
        className
      )}
      {...props}
    />
  )
}

function TableCell({ className, ...props }: React.ComponentProps<"td">) {
  return (
    <td
      data-slot="table-cell"
      className={cn(
        "p-2 align-middle whitespace-nowrap [&:has([role=checkbox])]:pr-0",
        className
      )}
      {...props}
    />
  )
}

function TableCaption({
  className,
  ...props
}: React.ComponentProps<"caption">) {
  return (
    <caption
      data-slot="table-caption"
      className={cn("mt-4 text-sm text-muted-foreground", className)}
      {...props}
    />
  )
}

export {
  IdText,
  Table,
  TableActionsCell,
  TableBody,
  TableCaption,
  TableCard,
  TableCell,
  TableFooter,
  TableHead,
  TableHeader,
  TableRow,
  TableTitleCell,
}

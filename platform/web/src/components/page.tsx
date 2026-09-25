import type { ComponentType, ReactNode } from "react";
import { Plus } from "lucide-react";
import { Button } from "@/components/ui/button";

export function PageHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description?: string;
  actions?: ReactNode;
}) {
  return (
    <div className="mb-6 flex flex-wrap items-start justify-between gap-3">
      <div className="grid min-w-0 flex-1 gap-1">
        <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
        {description && <p className="text-sm text-muted-foreground">{description}</p>}
      </div>
      {actions && <div className="shrink-0">{actions}</div>}
    </div>
  );
}

export function SectionHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description?: string;
  actions?: ReactNode;
}) {
  return (
    <div className="mb-3 flex flex-wrap items-end justify-between gap-3">
      <div className="grid gap-0.5">
        <h2 className="text-base font-medium">{title}</h2>
        {description && <p className="text-sm text-muted-foreground">{description}</p>}
      </div>
      {actions}
    </div>
  );
}

/// An empty list: what belongs here and where it comes from, under the
/// page's own icon. The page header holds the action that adds one; the box
/// does not repeat it.
export function EmptyState({
  icon: Icon,
  title,
  children,
}: {
  icon: ComponentType<{ className?: string }>;
  title: string;
  children?: ReactNode;
}) {
  return (
    <div className="flex min-h-40 flex-col items-center justify-center gap-2 rounded-xl border border-dashed p-8 text-center">
      <Icon className="size-7 text-muted-foreground" />
      <p className="text-sm font-medium">{title}</p>
      {children && <p className="max-w-lg text-sm text-muted-foreground">{children}</p>}
    </div>
  );
}

/// The detail side of a list-and-detail page with nothing open: the page's
/// icon, what to pick, and, for a member who may, the way to create one. The
/// list header's small add button stays; this is where a newcomer finds it.
export function DetailPrompt({
  icon,
  children,
  create,
}: {
  icon: ReactNode;
  children: ReactNode;
  create?: { label: string; onClick: () => void };
}) {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center text-sm text-muted-foreground">
      {icon}
      <span>{children}</span>
      {create && (
        <Button size="sm" variant="outline" onClick={create.onClick}>
          <Plus data-icon="inline-start" /> {create.label}
        </Button>
      )}
    </div>
  );
}

/// An empty list column: one muted line.
export function ListNote({ children }: { children: ReactNode }) {
  return <p className="p-4 text-sm text-muted-foreground">{children}</p>;
}

export function CenteredNote({ children }: { children: ReactNode }) {
  return (
    <div className="flex min-h-48 items-center justify-center rounded-xl border border-dashed p-8 text-center text-sm text-muted-foreground">
      <div className="grid gap-2">{children}</div>
    </div>
  );
}

/// Universe route guard result: loading, or the slug doesn't resolve.
export function UniverseNotFound({ slug }: { slug: string | undefined }) {
  return (
    <CenteredNote>
      <span>
        Universe <span className="font-medium text-foreground">{slug}</span> was not
        found, or you are not a member.
      </span>
    </CenteredNote>
  );
}

export function LoadingNote() {
  return <p className="text-sm text-muted-foreground">Loading…</p>;
}

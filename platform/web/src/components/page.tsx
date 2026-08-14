import type { ReactNode } from "react";

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
      <div className="grid gap-1">
        <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
        {description && <p className="text-sm text-muted-foreground">{description}</p>}
      </div>
      {actions}
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

import type { ReactNode } from "react";
import { InfoBox } from "@/components/ui/info-box";

/// A placeholder for work not done yet: a box saying so, with an optional
/// description of what is missing. Development, tests and the demo build
/// show it. A production build leaves it out: it renders nothing there.
/// A `critical` one must not ship at all: the production build fails while
/// one is rendered into it (the lightspeed-todo-guard plugin in
/// vite.config.ts), and it shows in the error colour meanwhile.
export function Todo({
  title,
  description,
  critical = false,
  className,
}: {
  title?: ReactNode;
  description?: ReactNode;
  /** Work the app cannot ship without: it fails the production build. */
  critical?: boolean;
  className?: string;
}) {
  // A constant in each build: a production build drops the box.
  if (import.meta.env.MODE === "production") return null;
  return (
    <InfoBox
      tone={critical ? "error" : "warning"}
      title={title ?? (critical ? "Critical To Do" : "To Do")}
      data-todo={critical ? "critical" : ""}
      className={className}
    >
      {description}
    </InfoBox>
  );
}

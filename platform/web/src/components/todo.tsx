import type { ReactNode } from "react";
import { InfoBox } from "@/components/ui/info-box";

/// A placeholder for work not done yet: a warning box saying so, with an
/// optional description of what is missing. It must not ship: the
/// production build fails while any module imports this one (the
/// lightspeed-todo-guard plugin in vite.config.ts). Development, tests and
/// the demo build show it.
export function Todo({
  title = "To Do",
  description,
  className,
}: {
  title?: ReactNode;
  description?: ReactNode;
  className?: string;
}) {
  return (
    <InfoBox tone="warning" title={title} data-todo="" className={className}>
      {description}
    </InfoBox>
  );
}

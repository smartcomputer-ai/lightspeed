import { cloneElement, isValidElement, type ReactElement, type ReactNode } from "react";

/// Base UI's floating positioner blocks jsdom's thread when a popup opens,
/// so component tests swap the popover for this inline stand-in: the content
/// always renders in place, next to its trigger.
export function Popover({ children }: { children: ReactNode }) {
  return <>{children}</>;
}

export function PopoverTrigger({ render, children }: { render?: ReactElement; children?: ReactNode }) {
  return isValidElement(render) ? cloneElement(render, {}, children) : <button type="button">{children}</button>;
}

export function PopoverContent({ children }: { children?: ReactNode }) {
  return <div data-slot="popover-content">{children}</div>;
}

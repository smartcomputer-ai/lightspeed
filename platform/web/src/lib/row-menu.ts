import { useEffect, useState, useSyncExternalStore, type MouseEvent as ReactMouseEvent } from "react";

/// A list row's right-click menu that is its detail page's ⋯ menu. The row
/// asks (requestRowMenu: which entity, where the pointer is), opens the
/// entity's page if it is not open, and the page header's menu for that
/// entity takes the request (useRowMenu) and opens at the pointer. The
/// menu, its items and the dialogs they open stay the page's, so a row
/// offers exactly what the header does.

export type RowMenuKind = "session" | "bot";

type RowMenuRequest = { kind: RowMenuKind; id: string; x: number; y: number };

/// A point the menu is anchored to (a virtual element, as the menu's
/// positioner takes).
export type RowMenuAnchor = { getBoundingClientRect: () => DOMRect };

let pending: RowMenuRequest | null = null;
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((listener) => listener());
const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

/// Asks for the menu of `kind` `id` at the right-click's pointer (or, from
/// the keyboard, which has none, under the row's start).
export function requestRowMenu(kind: RowMenuKind, id: string, event: ReactMouseEvent<HTMLElement>) {
  let { clientX: x, clientY: y } = event;
  if (x === 0 && y === 0) {
    const row = event.currentTarget.getBoundingClientRect();
    x = row.left + 24;
    y = row.bottom;
  }
  pending = { kind, id, x, y };
  notify();
}

/// The open state and anchor of the ⋯ menu of `kind` `id` (none: a menu
/// that rows do not open): opened at the pointer when its row asks, and at
/// its own button otherwise.
export function useRowMenu(kind: RowMenuKind | null, id: string) {
  const request = useSyncExternalStore(subscribe, () => pending, () => null);
  const [open, setOpen] = useState(false);
  const [anchor, setAnchor] = useState<RowMenuAnchor>();
  useEffect(() => {
    if (!kind || !request || request.kind !== kind || request.id !== id) return;
    pending = null;
    notify();
    const { x, y } = request;
    setAnchor({ getBoundingClientRect: () => new DOMRect(x, y, 0, 0) });
    setOpen(true);
  }, [kind, id, request]);
  return {
    open,
    anchor,
    onOpenChange: (next: boolean) => {
      setOpen(next);
      if (!next) setAnchor(undefined);
    },
  };
}

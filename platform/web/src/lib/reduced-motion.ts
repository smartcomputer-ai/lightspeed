import { useSyncExternalStore } from "react";
import { SETTINGS, settings, type ReducedMotion } from "@/lib/settings";

/// Whether to reduce motion: the user's choice in Preferences, which by
/// default follows the system setting. Kept current as either changes.
/// Without matchMedia (tests), the system asks for full motion.

const QUERY = "(prefers-reduced-motion: reduce)";

function systemQuery(): MediaQueryList | undefined {
  try {
    return window.matchMedia?.(QUERY);
  } catch {
    return undefined;
  }
}

/// Whether a choice reduces motion, given the system setting now.
export function reducesMotion(choice: ReducedMotion): boolean {
  return choice === "system" ? (systemQuery()?.matches ?? false) : choice === "on";
}

/// Calls back when the choice or the system setting changes.
export function subscribeReducedMotion(onChange: () => void): () => void {
  const query = systemQuery();
  query?.addEventListener("change", onChange);
  const unsubscribe = settings.subscribe(SETTINGS.reducedMotion, onChange);
  return () => {
    query?.removeEventListener("change", onChange);
    unsubscribe();
  };
}

/// The same, read once (outside React, e.g. when a timer is set).
export function prefersReducedMotion(): boolean {
  return reducesMotion(settings.get(SETTINGS.reducedMotion));
}

export function useReducedMotion(): boolean {
  return useSyncExternalStore(subscribeReducedMotion, prefersReducedMotion, () => false);
}

/// Keeps `data-motion` on <html> ("reduce" or "full") in step, which the
/// `motion-safe` and `motion-reduce` variants and index.css key on.
/// index.html sets it before first paint.
export function applyReducedMotion(): () => void {
  const apply = () => {
    document.documentElement.dataset.motion = prefersReducedMotion() ? "reduce" : "full";
  };
  apply();
  return subscribeReducedMotion(apply);
}

import { useCallback, useSyncExternalStore } from "react";
import { createBrowserStorageProvider } from "./browser-storage";
import type { Setting } from "./setting";
import { createSettingsStore, type SettingsStore } from "./store";

export { SETTINGS, DEFAULT_THEME, type ReducedMotion, type Theme } from "./registry";
export type { Setting } from "./setting";
export type { SettingsStore } from "./store";

/// The one settings store of this page. Every setting is kept in browser
/// storage for now, in the demo too. Saving synced settings with the account
/// changes only this line, not the code that uses settings.
const browserStorage = createBrowserStorageProvider();
export const settings: SettingsStore = createSettingsStore(browserStorage);

/** A module can expire stale local values even when it loads after the shell. */
export function registerDeviceExpiry(prefixes: readonly string[]): void {
  browserStorage.registerExpiry(prefixes);
}

/// A setting's value and a setter, like `useState`. Every component using the
/// setting updates together, including when another tab changes it. Setting
/// applies at once; the save runs in the background and a failed one keeps
/// the value for this page.
export function useSetting<T>(setting: Setting<T>): [T, (next: T | ((current: T) => T)) => void] {
  const subscribe = useCallback((onChange: () => void) => settings.subscribe(setting, onChange), [setting]);
  const read = useCallback(() => settings.get(setting), [setting]);
  const value = useSyncExternalStore(subscribe, read, read);
  const update = useCallback((next: T | ((current: T) => T)) => {
    const resolved = typeof next === "function" ? (next as (current: T) => T)(settings.get(setting)) : next;
    settings.set(setting, resolved).catch(() => {
      // The value applies for this page; storage that refuses it only means
      // it does not outlast the page.
    });
  }, [setting]);
  return [value, update];
}

import type { SettingsProvider, StorageArea, StoredChange } from "./provider";

export interface BrowserStorageOptions {
  expiringPrefixes?: readonly string[];
  now?: () => number;
}

/** Device drafts and recovery data are retained for a month after last use. */
const DEVICE_MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000;

/// When each expiring entry was last used, by key.
export const LAST_USED_KEY = "lightspeed:settings:last-used";

interface Pending {
  change: StoredChange;
  waiters: Array<{ resolve: () => void; reject: (error: unknown) => void }>;
}

interface BrowserStorageProvider extends SettingsProvider {
  registerExpiry(prefixes: readonly string[]): void;
}

/// Keeps synced and device settings in `localStorage` and tab settings in
/// `sessionStorage`, under the keys the registry names. It loads at once,
/// follows changes other tabs make, and never throws on blocked or full
/// storage: reads give nothing and saves reject.
export function createBrowserStorageProvider(options: BrowserStorageOptions = {}): BrowserStorageProvider {
  const now = options.now ?? Date.now;
  const expiring = new Set(options.expiringPrefixes);
  const pending = new Map<string, Pending>();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let timerDue = Infinity;
  /// Expiring keys used on this page since the last-used index was written.
  const used = new Map<string, number | null>();
  let usedTimer: ReturnType<typeof setTimeout> | undefined;

  const storage = (area: StorageArea): Storage | null => {
    try {
      if (typeof window === "undefined") return null;
      return area === "tab" ? window.sessionStorage : window.localStorage;
    } catch {
      return null;
    }
  };
  const id = (area: StorageArea, key: string) => `${area}\u0000${key}`;
  const expires = (area: StorageArea, key: string) => {
    if (area !== "local") return false;
    for (const prefix of expiring) if (key.startsWith(prefix)) return true;
    return false;
  };

  const readLastUsed = (local: Storage): Record<string, number> => {
    try {
      const parsed: unknown = JSON.parse(local.getItem(LAST_USED_KEY) ?? "null");
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
      return Object.fromEntries(Object.entries(parsed).filter(([, at]) => typeof at === "number"));
    } catch {
      return {};
    }
  };

  const writeLastUsed = () => {
    usedTimer = undefined;
    const local = storage("local");
    if (!local || used.size === 0) return;
    try {
      const index = readLastUsed(local);
      for (const [key, at] of used) {
        if (at === null) delete index[key];
        else index[key] = at;
      }
      used.clear();
      if (Object.keys(index).length) local.setItem(LAST_USED_KEY, JSON.stringify(index));
      else local.removeItem(LAST_USED_KEY);
    } catch {
      // Expiry is housekeeping; it waits for storage that works.
    }
  };

  const markUsed = (key: string, at: number | null) => {
    used.set(key, at);
    usedTimer ??= setTimeout(writeLastUsed, 1_000);
  };
  const touchedOnRead = new Set<string>();

  /// Drops expiring entries not used within their age, and starts the clock
  /// for entries stored before they had one.
  const sweep = () => {
    const local = storage("local");
    if (!local) return;
    try {
      const index = readLastUsed(local);
      if (expiring.size === 0 && !Object.keys(index).length) return;
      const at = now();
      const present = new Set<string>();
      for (let i = 0; i < local.length; i++) {
        const key = local.key(i);
        if (key !== null) present.add(key);
      }
      let changed = false;
      for (const key of present) {
        // The index keeps a registered key expiring even on a page that omits its module.
        const last = Object.hasOwn(index, key) ? index[key] : undefined;
        if (last === undefined) {
          if (!expires("local", key)) continue;
          index[key] = at;
          changed = true;
        } else if (at - last > DEVICE_MAX_AGE_MS) {
          local.removeItem(key);
          delete index[key];
          changed = true;
        }
      }
      for (const key of Object.keys(index)) {
        if (!present.has(key)) {
          delete index[key];
          changed = true;
        }
      }
      if (!changed) return;
      if (Object.keys(index).length) local.setItem(LAST_USED_KEY, JSON.stringify(index));
      else local.removeItem(LAST_USED_KEY);
    } catch {
      // Expiry is housekeeping; it waits for storage that works.
    }
  };

  const write = (change: StoredChange) => {
    const target = storage(change.area);
    if (!target) throw new Error("Browser storage is not available.");
    if (change.raw === null) target.removeItem(change.key);
    else target.setItem(change.key, change.raw);
    if (expires(change.area, change.key)) markUsed(change.key, change.raw === null ? null : now());
  };

  const flushPending = () => {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
    timerDue = Infinity;
    const batch = [...pending.values()];
    pending.clear();
    for (const { change, waiters } of batch) {
      try {
        write(change);
        for (const waiter of waiters) waiter.resolve();
      } catch (error) {
        for (const waiter of waiters) waiter.reject(error);
      }
    }
    if (usedTimer !== undefined) {
      clearTimeout(usedTimer);
      writeLastUsed();
    }
  };

  const schedule = (delayMs: number) => {
    const due = now() + delayMs;
    if (timer !== undefined && timerDue <= due) return;
    if (timer !== undefined) clearTimeout(timer);
    timerDue = due;
    timer = setTimeout(flushPending, delayMs);
  };

  if (typeof window !== "undefined") {
    // A page being hidden may never come back: save what is held back.
    window.addEventListener("pagehide", flushPending);
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "hidden") flushPending();
    });
  }
  sweep();

  return {
    ready: Promise.resolve(),

    registerExpiry(prefixes) {
      for (const prefix of prefixes) expiring.add(prefix);
      sweep();
    },

    peek(area, key) {
      const source = storage(area);
      if (!source) return null;
      try {
        const raw = source.getItem(key);
        if (raw !== null && !touchedOnRead.has(key) && expires(area, key)) {
          touchedOnRead.add(key);
          markUsed(key, now());
        }
        return raw;
      } catch {
        return null;
      }
    },

    save(changes) {
      return Promise.all(changes.map((change) => new Promise<void>((resolve, reject) => {
        const slot = id(change.area, change.key);
        const held = pending.get(slot);
        if (!change.delayMs) {
          // A direct write supersedes one held back for the same key.
          pending.delete(slot);
          try {
            write(change);
            resolve();
            for (const waiter of held?.waiters ?? []) waiter.resolve();
          } catch (error) {
            reject(error);
            for (const waiter of held?.waiters ?? []) waiter.reject(error);
          }
          return;
        }
        pending.set(slot, { change, waiters: [...(held?.waiters ?? []), { resolve, reject }] });
        schedule(change.delayMs);
      }))).then(() => undefined);
    },

    flush() {
      flushPending();
      return Promise.resolve();
    },

    discard() {
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
      timerDue = Infinity;
      const dropped = [...pending.values()];
      pending.clear();
      for (const { waiters } of dropped) for (const waiter of waiters) waiter.reject(new Error("The write was discarded."));
      if (usedTimer !== undefined) clearTimeout(usedTimer);
      usedTimer = undefined;
      used.clear();
      touchedOnRead.clear();
    },

    subscribe(onChange) {
      if (typeof window === "undefined") return () => {};
      const onStorage = (event: StorageEvent) => {
        if (event.key === LAST_USED_KEY) return;
        let area: StorageArea = "local";
        try {
          if (event.storageArea && event.storageArea === window.sessionStorage) area = "tab";
        } catch {
          // Without session storage, the event concerns local storage.
        }
        // Tab settings belong to one tab and do not follow others.
        if (area === "tab") return;
        onChange(event.key === null ? null : [{ area, key: event.key }]);
      };
      window.addEventListener("storage", onStorage);
      return () => window.removeEventListener("storage", onStorage);
    },
  };
}

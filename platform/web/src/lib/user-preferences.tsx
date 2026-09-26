import { createContext, useContext, useEffect, useState, type ReactNode } from "react";

const KEY_PREFIX = "lightspeed:user-preferences:";

/** Account-scoped display preferences shared by the session and bot views. */
export interface UserPreferences {
  /** Show context and token usage on finished runs. */
  showRunStatistics: boolean;
  /** Fold a finished run's tool calls, thinking, and interim notes behind
   * one strip; applies when a session or older history loads. */
  collapseCompletedRuns: boolean;
  /** The main menu's dragged width in pixels; null keeps the default. */
  sidebarWidth: number | null;
  /** The main menu shows icons only. */
  sidebarCollapsed: boolean;
  /** The dragged width of every page's list column, so it holds from page
   * to page; null keeps the default. */
  listWidth: number | null;
}

const DEFAULTS: UserPreferences = {
  showRunStatistics: true,
  collapseCompletedRuns: true,
  sidebarWidth: null,
  sidebarCollapsed: false,
  listWidth: null,
};

function storedWidth(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? Math.round(value) : null;
}

const Context = createContext<UserPreferences & {
  setShowRunStatistics: (show: boolean) => void;
  setCollapseCompletedRuns: (collapse: boolean) => void;
  setSidebarWidth: (width: number | null) => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  setListWidth: (width: number | null) => void;
}>({
  ...DEFAULTS,
  setShowRunStatistics: () => {},
  setCollapseCompletedRuns: () => {},
  setSidebarWidth: () => {},
  setSidebarCollapsed: () => {},
  setListWidth: () => {},
});

export function readUserPreferences(userId: string): UserPreferences {
  try {
    const stored: unknown = JSON.parse(window.localStorage.getItem(`${KEY_PREFIX}${userId}`) ?? "null");
    if (!stored || typeof stored !== "object") return DEFAULTS;
    const record = stored as Record<string, unknown>;
    return {
      showRunStatistics: typeof record.showRunStatistics === "boolean"
        ? record.showRunStatistics : DEFAULTS.showRunStatistics,
      collapseCompletedRuns: typeof record.collapseCompletedRuns === "boolean"
        ? record.collapseCompletedRuns : DEFAULTS.collapseCompletedRuns,
      sidebarWidth: storedWidth(record.sidebarWidth),
      sidebarCollapsed: record.sidebarCollapsed === true,
      listWidth: storedWidth(record.listWidth),
    };
  } catch {
    return DEFAULTS;
  }
}

export function UserPreferencesProvider({ userId, children }: { userId: string; children: ReactNode }) {
  return <AccountPreferences key={userId} userId={userId}>{children}</AccountPreferences>;
}

function AccountPreferences({ userId, children }: { userId: string; children: ReactNode }) {
  const [preferences, setPreferences] = useState(() => readUserPreferences(userId));
  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key === null || event.key === `${KEY_PREFIX}${userId}`) setPreferences(readUserPreferences(userId));
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [userId]);
  const update = (patch: Partial<UserPreferences>) => {
    setPreferences((current) => {
      const next = { ...current, ...patch };
      try {
        window.localStorage.setItem(`${KEY_PREFIX}${userId}`, JSON.stringify(next));
      } catch {
        // Keep the current view usable when browser storage is blocked or full.
      }
      return next;
    });
  };
  return (
    <Context.Provider
      value={{
        ...preferences,
        setShowRunStatistics: (showRunStatistics) => update({ showRunStatistics }),
        setCollapseCompletedRuns: (collapseCompletedRuns) => update({ collapseCompletedRuns }),
        setSidebarWidth: (sidebarWidth) => update({ sidebarWidth }),
        setSidebarCollapsed: (sidebarCollapsed) => update({ sidebarCollapsed }),
        setListWidth: (listWidth) => update({ listWidth }),
      }}
    >
      {children}
    </Context.Provider>
  );
}

export function useUserPreferences() { return useContext(Context); }

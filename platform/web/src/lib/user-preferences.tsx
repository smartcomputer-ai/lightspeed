import { createContext, useContext, useEffect, useState, type ReactNode } from "react";

const KEY_PREFIX = "lightspeed:user-preferences:";

/** Account-scoped display preferences shared by the session and bot views. */
export interface UserPreferences {
  /** Show context and token usage on finished runs. */
  showRunStatistics: boolean;
  /** Fold a finished run's tool calls, thinking, and interim notes behind
   * one strip; applies when a session or older history loads. */
  collapseCompletedRuns: boolean;
}

const DEFAULTS: UserPreferences = { showRunStatistics: true, collapseCompletedRuns: true };

const Context = createContext<UserPreferences & {
  setShowRunStatistics: (show: boolean) => void;
  setCollapseCompletedRuns: (collapse: boolean) => void;
}>({
  ...DEFAULTS,
  setShowRunStatistics: () => {},
  setCollapseCompletedRuns: () => {},
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
      }}
    >
      {children}
    </Context.Provider>
  );
}

export function useUserPreferences() { return useContext(Context); }

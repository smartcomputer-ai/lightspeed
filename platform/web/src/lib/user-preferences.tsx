import { createContext, useContext, type ReactNode } from "react";
import { SETTINGS, useSetting } from "@/lib/settings";

/** The signed-in user's display preferences, shared by the session and bot
 * views. Each is a setting of its own (lib/settings/registry.ts); the widths
 * stay on the device, the others follow the user. */
export interface UserPreferences {
  /** Show context and token usage on finished runs. */
  showRunStatistics: boolean;
  /** Show what a session can reach (environment, files, MCP servers, …)
   * in the strip under its header. */
  showSessionResources: boolean;
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
  /** Meta toolbars that expand on hover (page headers) stay expanded, as
   * tags, until unlocked. Set from the lock beside any of them. */
  keepMetaExpanded: boolean;
}

/// Before a user is known: the declared defaults.
const DEFAULTS: UserPreferences = {
  showRunStatistics: SETTINGS.showRunStatistics("").defaultValue,
  showSessionResources: SETTINGS.showSessionResources("").defaultValue,
  collapseCompletedRuns: SETTINGS.collapseCompletedRuns("").defaultValue,
  sidebarWidth: SETTINGS.sidebarWidth("").defaultValue,
  sidebarCollapsed: SETTINGS.sidebarCollapsed("").defaultValue,
  listWidth: SETTINGS.listWidth("").defaultValue,
  keepMetaExpanded: SETTINGS.keepMetaExpanded("").defaultValue,
};

const Context = createContext<UserPreferences & {
  setShowRunStatistics: (show: boolean) => void;
  setShowSessionResources: (show: boolean) => void;
  setCollapseCompletedRuns: (collapse: boolean) => void;
  setSidebarWidth: (width: number | null) => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  setListWidth: (width: number | null) => void;
  setKeepMetaExpanded: (keep: boolean) => void;
}>({
  ...DEFAULTS,
  setShowRunStatistics: () => {},
  setShowSessionResources: () => {},
  setCollapseCompletedRuns: () => {},
  setSidebarWidth: () => {},
  setSidebarCollapsed: () => {},
  setListWidth: () => {},
  setKeepMetaExpanded: () => {},
});

export function UserPreferencesProvider({ userId, children }: { userId: string; children: ReactNode }) {
  const [showRunStatistics, setShowRunStatistics] = useSetting(SETTINGS.showRunStatistics(userId));
  const [showSessionResources, setShowSessionResources] = useSetting(SETTINGS.showSessionResources(userId));
  const [collapseCompletedRuns, setCollapseCompletedRuns] = useSetting(SETTINGS.collapseCompletedRuns(userId));
  const [sidebarWidth, setSidebarWidth] = useSetting(SETTINGS.sidebarWidth(userId));
  const [sidebarCollapsed, setSidebarCollapsed] = useSetting(SETTINGS.sidebarCollapsed(userId));
  const [listWidth, setListWidth] = useSetting(SETTINGS.listWidth(userId));
  const [keepMetaExpanded, setKeepMetaExpanded] = useSetting(SETTINGS.keepMetaExpanded(userId));
  return (
    <Context.Provider
      value={{
        showRunStatistics,
        showSessionResources,
        collapseCompletedRuns,
        sidebarWidth,
        sidebarCollapsed,
        listWidth,
        keepMetaExpanded,
        setShowRunStatistics,
        setShowSessionResources,
        setCollapseCompletedRuns,
        setSidebarWidth,
        setSidebarCollapsed,
        setListWidth,
        setKeepMetaExpanded,
      }}
    >
      {children}
    </Context.Provider>
  );
}

export function useUserPreferences() { return useContext(Context); }

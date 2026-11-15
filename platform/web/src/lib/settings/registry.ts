import { sessionSteerSchema, type MessageAttachment } from "@lightspeed-ai/platform-shared";
import type { AttachmentKind } from "@lightspeed-ai/platform-shared";
import type { RunChoice } from "@/lib/composer-model";
import {
  DEFAULT_SESSION_LIST_PREFERENCES,
  parseSessionListPreferences,
  parseSessionMetadataFilter,
  type SessionListPreferences,
} from "@/lib/sessions/list-preferences";
import type { DesignOverrides } from "@/devtools/design-tokens";
import type { ExpiringEntries } from "./browser-storage";
import { defineSetting, defineSettingFamily, deviceOverride } from "./setting";

/// Every setting the web UI keeps, declared once. Code reads and writes them
/// through the settings store (`settings`, `useSetting`), never through
/// browser storage. The keys are the ones each setting had before the store,
/// so nothing stored is lost, and index.html still reads the theme before
/// first paint.
///
/// Modules imported here must not import the settings in turn: parsers that
/// live beside their feature are imported from modules without that import.

export type Theme = "light" | "dark" | "system";

/// The theme before the user picks one. index.html repeats it for first paint.
export const DEFAULT_THEME: Theme = "dark";

/// Whether to reduce motion: always, never, or as the system asks.
export type ReducedMotion = "on" | "off" | "system";

const boolean = (stored: unknown) => (typeof stored === "boolean" ? stored : undefined);
const width = (stored: unknown) =>
  typeof stored === "number" && Number.isFinite(stored) && stored > 0 ? Math.round(stored) : undefined;
const record = (value: unknown): Record<string, unknown> | undefined =>
  value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : undefined;
const strings = (value: unknown): Record<string, string> | undefined => {
  const object = record(value);
  if (!object) return undefined;
  return Object.fromEntries(Object.entries(object).filter(([, entry]) => typeof entry === "string")) as Record<string, string>;
};

/// A display preference of the signed-in user, kept with the others in one
/// object per user.
const display = (field: string, defaultValue: boolean) =>
  defineSettingFamily(`display.${field}`, (userId: string) => ({
    key: `lightspeed:user-preferences:${userId}`,
    field,
    defaultValue,
    parse: boolean,
    override: "display" as const,
  }));

/// A width dragged on one screen, which would not suit another: kept on the
/// device. Widths used to be stored with the display preferences and are
/// still read from there until they are set again.
const layoutWidth = (field: string) =>
  defineSettingFamily(`layout.${field}`, (userId: string) => ({
    persistence: "device" as const,
    key: `lightspeed:layout:${userId}`,
    field,
    legacy: { key: `lightspeed:user-preferences:${userId}`, field, format: "json" as const },
    defaultValue: null as number | null,
    parse: width,
    isEmpty: (value: number | null) => value === null,
  }));

/// A ready attachment saved beside a composer draft.
export interface StoredAttachment {
  id: string;
  name: string;
  mime: string;
  kind: AttachmentKind;
  size: number;
  blobRef: string;
}

/// A queued message kept for recovery while it is converted to steering.
export interface StoredQueuedRecovery {
  id: string;
  error: string;
  message: { text: string; attachments: MessageAttachment[] };
}

const EMPTY_RUN_CHOICE: RunChoice = Object.freeze({});
const EMPTY_LIST: readonly never[] = Object.freeze([]);
const EMPTY_FILTER: Record<string, string> = Object.freeze({}) as Record<string, string>;
const EMPTY_DESIGN: DesignOverrides = Object.freeze({ shared: {}, light: {}, dark: {} });

/// Drafts and the choices kept beside them, for sessions since deleted too:
/// dropped after a month without use.
export const DRAFT_PREFIX = "lightspeed:sessions:draft:";
const QUEUED_STEERING_PREFIX = "lightspeed:sessions:queued-steering:";
const MONTH_MS = 30 * 24 * 60 * 60 * 1000;
export const EXPIRING_SETTINGS: readonly ExpiringEntries[] = [
  { prefix: DRAFT_PREFIX, maxAgeMs: MONTH_MS },
  { prefix: QUEUED_STEERING_PREFIX, maxAgeMs: MONTH_MS },
];

/// Drafts change on every keystroke: they reach storage a moment later,
/// and at once when the page is hidden.
const DRAFT_WRITE_DELAY_MS = 250;

export const SETTINGS = {
  // Synced: follow the user once settings are saved with the account.

  /// Light, dark, or the system's.
  theme: defineSetting<Theme>("theme", {
    key: "lightspeed:theme",
    format: "text",
    defaultValue: DEFAULT_THEME,
    parse: (stored) => (stored === "light" || stored === "dark" || stored === "system" ? stored : undefined),
    override: "theme",
  }),
  /// Show the starscapes in the sidebar (behind the universe icon) and
  /// behind creating dialogs. The welcome's starscape always shows.
  starscape: defineSetting<boolean>("starscape", {
    key: "lightspeed:starscape",
    format: "text",
    defaultValue: true,
    parse: (stored) => (stored === "on" ? true : stored === "off" ? false : undefined),
    serialize: (on) => (on ? "on" : "off"),
    isEmpty: (on) => on,
    override: "theme",
  }),
  /// Turn animations and transitions off. "system" follows the operating
  /// system's setting. index.html reads it before first paint.
  reducedMotion: defineSetting<ReducedMotion>("reducedMotion", {
    key: "lightspeed:reduced-motion",
    format: "text",
    defaultValue: "system",
    parse: (stored) => (stored === "on" || stored === "off" || stored === "system" ? stored : undefined),
    isEmpty: (choice) => choice === "system",
    override: "display",
  }),
  /// Show context and token usage on finished runs.
  showRunStatistics: display("showRunStatistics", false),
  /// Show what a session can reach (environment, files, MCP servers, …) in
  /// the strip under its header.
  showSessionResources: display("showSessionResources", true),
  /// Fold a finished run's tool calls, thinking, and interim notes behind one
  /// strip; applies when a session or older history loads.
  collapseCompletedRuns: display("collapseCompletedRuns", true),
  /// The main menu shows icons only.
  sidebarCollapsed: display("sidebarCollapsed", false),
  /// Meta toolbars that expand on hover (page headers) stay expanded, as
  /// tags, until unlocked.
  keepMetaExpanded: display("keepMetaExpanded", false),
  /// What the session list includes and which metadata columns it shows, per universe.
  sessionList: defineSettingFamily("sessionList", (universeId: string) => ({
    key: `lightspeed:sessions:list-preferences:${universeId}`,
    defaultValue: DEFAULT_SESSION_LIST_PREFERENCES,
    parse: parseSessionListPreferences,
  })),
  /// The session list's metadata filter, per universe; the URL can also set it.
  sessionMetadataFilter: defineSettingFamily("sessionMetadataFilter", (universeId: string) => ({
    key: `lightspeed:sessions:metadata-filter:${universeId}`,
    defaultValue: EMPTY_FILTER,
    parse: parseSessionMetadataFilter,
  })),
  /// The handle of the universe opened last, for the landing redirect.
  lastUniverse: defineSetting<string | null>("lastUniverse", {
    key: "lightspeed:last-universe",
    format: "text",
    defaultValue: null,
    parse: (stored) => (typeof stored === "string" && stored ? stored : undefined),
    isEmpty: (handle) => !handle,
  }),
  /// The welcome dialog was turned off ("Don't show anymore").
  welcomeDismissed: defineSetting<boolean>("welcomeDismissed", {
    key: "lightspeed:welcome-dismissed",
    format: "text",
    defaultValue: false,
    parse: (stored) => (stored === "true" ? true : undefined),
    serialize: (dismissed) => String(dismissed),
    isEmpty: (dismissed) => !dismissed,
  }),

  // Device: never leave this browser.

  /// The main menu's dragged width in pixels; null keeps the default.
  sidebarWidth: layoutWidth("sidebarWidth"),
  /// The dragged width of every page's list column, so it holds from page to
  /// page; null keeps the default.
  listWidth: layoutWidth("listWidth"),
  /// A composer's unsent text, by draft key (see `sessionDraftKey` in
  /// lib/sessions/draft.ts).
  composerDraft: defineSettingFamily("composerDraft", (draftKey: string) => ({
    persistence: "device" as const,
    key: draftKey,
    format: "text" as const,
    defaultValue: "",
    parse: (stored: unknown) => (typeof stored === "string" ? stored : undefined),
    isEmpty: (text: string) => text === "",
    writeDelayMs: DRAFT_WRITE_DELAY_MS,
  })),
  /// The model and effort a composer sends with, where they differ from the session's.
  composerRunChoice: defineSettingFamily("composerRunChoice", (draftKey: string) => ({
    persistence: "device" as const,
    key: `${draftKey}:run`,
    defaultValue: EMPTY_RUN_CHOICE,
    parse: (stored: unknown): RunChoice | undefined => {
      const value = record(stored);
      if (!value) return undefined;
      return {
        ...(typeof value.model === "string" && value.model ? { model: value.model } : {}),
        ...(typeof value.reasoningEffort === "string" && value.reasoningEffort ? { reasoningEffort: value.reasoningEffort } : {}),
      };
    },
    isEmpty: (choice: RunChoice) => Object.keys(choice).length === 0,
  })),
  /// A composer's uploaded attachments, kept with its draft.
  composerAttachments: defineSettingFamily("composerAttachments", (draftKey: string) => ({
    persistence: "device" as const,
    key: `${draftKey}:attachments`,
    defaultValue: EMPTY_LIST as readonly StoredAttachment[],
    parse: (stored: unknown): readonly StoredAttachment[] | undefined => {
      if (!Array.isArray(stored)) return undefined;
      return stored.flatMap((value): StoredAttachment[] => {
        const item = record(value);
        return item && typeof item.id === "string" && typeof item.name === "string" && typeof item.mime === "string"
          && (item.kind === "image" || item.kind === "document") && typeof item.size === "number"
          && typeof item.blobRef === "string"
          ? [{ id: item.id, name: item.name, mime: item.mime, kind: item.kind, size: item.size, blobRef: item.blobRef }]
          : [];
      });
    },
    isEmpty: (items: readonly StoredAttachment[]) => items.length === 0,
  })),
  /// Queued messages being converted to steering, kept so a reload or a lost
  /// response cannot lose them; by session scope.
  queuedSteering: defineSettingFamily("queuedSteering", (scope: string) => ({
    persistence: "device" as const,
    key: `${QUEUED_STEERING_PREFIX}${scope}`,
    defaultValue: EMPTY_LIST as readonly StoredQueuedRecovery[],
    parse: (stored: unknown): readonly StoredQueuedRecovery[] | undefined => {
      if (!Array.isArray(stored)) return undefined;
      return stored.flatMap((value): StoredQueuedRecovery[] => {
        const item = record(value);
        const input = sessionSteerSchema.safeParse(item?.message);
        if (!item || !input.success || typeof item.id !== "string" || typeof item.error !== "string") return [];
        return [{ id: item.id, error: item.error, message: { text: input.data.text, attachments: input.data.attachments } }];
      });
    },
    isEmpty: (items: readonly StoredQueuedRecovery[]) => items.length === 0,
  })),
  /// Design token overrides from the debug tools' design editor. Every open
  /// window follows them as a control moves, so they reach storage once a frame.
  designOverrides: defineSetting<DesignOverrides>("designOverrides", {
    persistence: "device",
    key: "lightspeed:design-overrides",
    defaultValue: EMPTY_DESIGN,
    parse: (stored) => {
      const value = record(stored);
      const shared = strings(value?.shared);
      const light = strings(value?.light);
      const dark = strings(value?.dark);
      if (!value || !shared || !light || !dark) return undefined;
      // The shadow inputs are migrated by the design tools (loadOverrides).
      return { shared, light, dark, ...(record(value.shadow) ? { shadow: value.shadow as DesignOverrides["shadow"] } : {}) };
    },
    isEmpty: (overrides) =>
      !Object.keys(overrides.shared).length && !Object.keys(overrides.light).length && !Object.keys(overrides.dark).length,
    writeDelayMs: 16,
  }),

  // Tab: last until the tab closes.

  /// The debug toolbar is shown (Ctrl+D).
  debugToolbarOpen: defineSetting<boolean>("debugToolbarOpen", {
    persistence: "tab",
    key: "lightspeed:debug-toolbar",
    format: "text",
    defaultValue: false,
    parse: (stored) => (stored === "1" ? true : stored === "0" ? false : undefined),
    serialize: (open) => (open ? "1" : "0"),
  }),

  /// Whether this device keeps its own values for a group of synced settings.
  /// Not offered in Preferences until settings are saved with the account:
  /// before that, both layers live in the same browser.
  deviceOverride,
} as const;


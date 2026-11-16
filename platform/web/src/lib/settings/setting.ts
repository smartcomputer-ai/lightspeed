/// How a setting is declared. Every setting is declared once, in the
/// registry (registry.ts), and is read and written only through the
/// settings store (store.ts), never through browser storage directly.

/// How long a setting lasts and where it may go.
/// - `synced` follows the user to other devices once settings are saved with
///   the account; until then it is kept in this browser like `device`.
/// - `device` stays in this browser until its site data is cleared, and is
///   never sent anywhere.
/// - `tab` lasts until the browser tab closes (`sessionStorage`). Not called
///   "session": a session in Lightspeed is a conversation with an agent.
export type Persistence = "synced" | "device" | "tab";

/// Synced settings that a device may keep different from the account, as
/// groups. Each group has its own device flag.
export type OverrideGroup = "theme" | "display";

/// Where a value is stored: a storage key, or a field of a JSON object kept
/// under a key that other settings share.
export interface SettingLocation {
  key: string;
  field?: string;
  /// `text` keeps the value as the stored string itself (no field); `json`
  /// keeps it as JSON.
  format: "text" | "json";
}

export interface Setting<T> {
  /// Unique among settings, including the ids of a family's instance.
  readonly id: string;
  readonly persistence: Persistence;
  readonly location: SettingLocation;
  /// An older location, read while `location` holds nothing, so a setting
  /// that moved keeps its value. Writes go to `location` only.
  readonly legacy?: SettingLocation;
  readonly defaultValue: T;
  /// Turns a stored value (the string for `text`, the parsed JSON for
  /// `json`) into a valid one, or rejects it with undefined. A missing or
  /// rejected value falls back to the default.
  readonly parse: (stored: unknown) => T | undefined;
  /// The value as stored: a string for `text`, a JSON value for `json`.
  readonly serialize: (value: T) => unknown;
  /// A value this accepts is not stored: writing it removes the stored value,
  /// so emptied drafts and choices leave nothing behind.
  readonly isEmpty?: (value: T) => boolean;
  /// For a synced setting, the group whose device flag lets this device keep
  /// a value of its own. Absent: the account's value always applies.
  readonly override?: OverrideGroup;
  /// How long the store may hold a write before saving it to storage, for
  /// settings that change many times a second (drafts, design tokens).
  /// Pending writes are saved when the page is hidden.
  readonly writeDelayMs?: number;
}

export interface SettingOptions<T> {
  persistence?: Persistence;
  key: string;
  field?: string;
  format?: "text" | "json";
  legacy?: SettingLocation;
  defaultValue: T;
  parse: (stored: unknown) => T | undefined;
  serialize?: (value: T) => unknown;
  isEmpty?: (value: T) => boolean;
  override?: OverrideGroup;
  writeDelayMs?: number;
}

export function defineSetting<T>(id: string, options: SettingOptions<T>): Setting<T> {
  const format = options.format ?? "json";
  if (format === "text" && options.field !== undefined) {
    throw new Error(`Setting ${id} cannot keep a text value in a field.`);
  }
  const persistence = options.persistence ?? "synced";
  if (options.override && persistence !== "synced") {
    throw new Error(`Setting ${id} belongs to the device already and cannot be overridden.`);
  }
  return Object.freeze({
    id,
    persistence,
    location: { key: options.key, ...(options.field !== undefined ? { field: options.field } : {}), format },
    ...(options.legacy ? { legacy: options.legacy } : {}),
    defaultValue: options.defaultValue,
    parse: options.parse,
    serialize: options.serialize ?? ((value: T) => value),
    ...(options.isEmpty ? { isEmpty: options.isEmpty } : {}),
    ...(options.override ? { override: options.override } : {}),
    ...(options.writeDelayMs ? { writeDelayMs: options.writeDelayMs } : {}),
  });
}

/// A setting declared once and called with ids, such as one per universe or
/// per Lightspeed session. The same ids give the same instance, so hooks see
/// a stable setting.
export function defineSettingFamily<Args extends readonly string[], T>(
  name: string,
  options: (...args: Args) => SettingOptions<T>,
): (...args: Args) => Setting<T> {
  const instances = new Map<string, Setting<T>>();
  return (...args: Args) => {
    const id = `${name}:${JSON.stringify(args)}`;
    let setting = instances.get(id);
    if (!setting) {
      setting = defineSetting(id, options(...args));
      instances.set(id, setting);
    }
    return setting;
  };
}

/// Where a device keeps the flags that turn its own values on, one per group.
export const DEVICE_OVERRIDE_FLAGS_KEY = "lightspeed:device-overrides";
/// Where a device keeps its own values for a group while that group's flag
/// is on: one JSON object, by the account location's address.
export const deviceValuesKey = (group: OverrideGroup) => `lightspeed:device-settings:${group}`;

/// The address of a location inside the device values object.
export function locationAddress(location: SettingLocation): string {
  return location.field === undefined ? location.key : `${location.key}#${location.field}`;
}

/// Whether this device keeps its own values for a group. A device setting
/// itself, so it never syncs.
export const deviceOverride = defineSettingFamily("deviceOverride", (group: OverrideGroup) => ({
  persistence: "device",
  key: DEVICE_OVERRIDE_FLAGS_KEY,
  field: group,
  defaultValue: false,
  parse: (stored) => (typeof stored === "boolean" ? stored : undefined),
  isEmpty: (on) => !on,
}));

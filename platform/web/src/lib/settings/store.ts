import type { SettingsProvider, StorageArea, StoredChange } from "./provider";
import {
  deviceOverride,
  deviceValuesKey,
  locationAddress,
  type OverrideGroup,
  type Setting,
  type SettingLocation,
} from "./setting";

export interface SettingsStore {
  /// The current value from the cache, at once: what rendering and first paint use.
  get<T>(setting: Setting<T>): T;
  /// Waits until the provider has loaded, then gives the value.
  read<T>(setting: Setting<T>): Promise<T>;
  /// Applies the value at once (cache and listeners), then resolves once the
  /// provider has saved it. A failed save rejects, but the value stays for
  /// this page.
  set<T>(setting: Setting<T>, value: T): Promise<void>;
  /// Removes the stored value, so the default applies again.
  reset<T>(setting: Setting<T>): Promise<void>;
  /// Calls back when the value changes: here, in another tab, or later from the server.
  subscribe<T>(setting: Setting<T>, listener: (value: T) => void): () => void;
  /// Resolves once the provider has loaded its values.
  readonly ready: Promise<void>;
  /// Whether this device keeps its own values for a group of synced settings.
  deviceOverride(group: OverrideGroup): boolean;
  /// Turns a group's device values on or off. Turning them on copies the
  /// account's current values, so nothing changes on screen; turning them
  /// off discards them, so the account's values apply again.
  setDeviceOverride(group: OverrideGroup, on: boolean): Promise<void>;
  /// Saves any writes still held back.
  flush(): Promise<void>;
  /// Forgets every value not yet saved, held back or failed, without saving
  /// it. For tests, which start each case on a fresh page.
  discardUnsaved(): void;
}

/// A setting whatever its value type, for code that only reads its declaration.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type AnySetting = Setting<any>;

/// One stored location as a cache identity: the storage area and key.
const slot = (area: StorageArea, key: string) => `${area}\u0000${key}`;
const areaOf = (setting: AnySetting): StorageArea => (setting.persistence === "tab" ? "tab" : "local");
const memoId = (setting: AnySetting, area: StorageArea, location: SettingLocation) =>
  `${setting.id}\u0000${area}\u0000${locationAddress(location)}`;

interface Unsaved {
  raw: string | null;
  /// The save failed; the value stays for this page only.
  failed: boolean;
}

/// The settings store: keeps a setting's value in step across the page and
/// other tabs, and hands storage to a provider.
///
/// Values are read from the provider's local copy on demand, so a read is
/// always current. A written value is kept in memory until its save
/// succeeds, and for the rest of the page if it fails, so blocked or full
/// storage never takes a setting away from the view that changed it.
///
/// A synced setting declared with an override group has two layers: the
/// account's value and this device's. While the group's device flag is on,
/// reads and writes use the device's value; code using settings never knows
/// which layer answered.
export function createSettingsStore(provider: SettingsProvider): SettingsStore {
  const unsaved = new Map<string, Unsaved>();
  /// Parsed values by setting and location, kept while the stored text is
  /// unchanged, so a read gives the same object every time.
  const parsed = new Map<string, { raw: string | null; value: unknown }>();
  const listeners = new Map<string, Set<() => void>>();
  /// Overridable settings this page has used, by id: what turning a group's
  /// flag on copies.
  const seen = new Map<string, AnySetting>();

  const raw = (area: StorageArea, key: string): string | null => {
    const held = unsaved.get(slot(area, key));
    return held ? held.raw : provider.peek(area, key);
  };

  const notify = (area: StorageArea, key: string) => {
    for (const listener of [...(listeners.get(slot(area, key)) ?? [])]) listener();
  };
  const notifyAll = () => {
    for (const set of [...listeners.values()]) for (const listener of [...set]) listener();
  };

  provider.subscribe((changed) => {
    // A value whose save failed gives way to the one another tab stored.
    if (changed === null) {
      for (const [id, held] of unsaved) if (held.failed) unsaved.delete(id);
      notifyAll();
      return;
    }
    for (const { area, key } of changed) {
      if (unsaved.get(slot(area, key))?.failed) unsaved.delete(slot(area, key));
      notify(area, key);
    }
  });

  /// The stored value at a location, before parsing: the text itself, or
  /// the parsed JSON (or its field). Undefined when nothing usable is stored.
  const decode = (text: string | null, location: SettingLocation): unknown => {
    if (text === null) return undefined;
    if (location.format === "text") return text;
    try {
      const json: unknown = JSON.parse(text);
      if (location.field === undefined) return json ?? undefined;
      return json && typeof json === "object" && !Array.isArray(json)
        ? (json as Record<string, unknown>)[location.field]
        : undefined;
    } catch {
      return undefined;
    }
  };

  /// The setting's valid value at a location, or undefined.
  const valueAt = <T>(setting: Setting<T>, area: StorageArea, location: SettingLocation): T | undefined => {
    const text = raw(area, location.key);
    const memo = memoId(setting, area, location);
    const cached = parsed.get(memo);
    if (cached && cached.raw === text) return cached.value as T | undefined;
    const stored = decode(text, location);
    let value: T | undefined;
    try {
      value = stored === undefined ? undefined : setting.parse(stored);
    } catch {
      value = undefined;
    }
    parsed.set(memo, { raw: text, value });
    return value;
  };

  /// Where this device keeps its own value of a setting.
  const deviceLocation = (setting: AnySetting, group: OverrideGroup): SettingLocation => ({
    key: deviceValuesKey(group),
    field: locationAddress(setting.location),
    format: "json",
  });

  const overriding = (setting: AnySetting): OverrideGroup | undefined => {
    const group = setting.persistence === "synced" ? setting.override : undefined;
    if (!group) return undefined;
    seen.set(setting.id, setting);
    return get(deviceOverride(group)) ? group : undefined;
  };

  const accountValue = <T>(setting: Setting<T>): T => {
    const area = areaOf(setting);
    const value = valueAt(setting, area, setting.location)
      ?? (setting.legacy ? valueAt(setting, area, setting.legacy) : undefined);
    return value === undefined ? setting.defaultValue : value;
  };

  function get<T>(setting: Setting<T>): T {
    const group = overriding(setting);
    if (group) {
      // A value this device has not stored yet (a setting first used after
      // the flag was turned on) follows the account until it is set here.
      const own = valueAt(setting, "local", deviceLocation(setting, group));
      if (own !== undefined) return own;
    }
    return accountValue(setting);
  }

  /// The stored text of a location after writing a value (or removing it, for undefined).
  const written = (area: StorageArea, location: SettingLocation, stored: unknown): string | null => {
    if (location.field === undefined) {
      if (stored === undefined) return null;
      return location.format === "text" ? String(stored) : JSON.stringify(stored);
    }
    let object: Record<string, unknown> = {};
    try {
      const current: unknown = JSON.parse(raw(area, location.key) ?? "null");
      if (current && typeof current === "object" && !Array.isArray(current)) object = { ...current as Record<string, unknown> };
    } catch {
      // A broken shared object is replaced; its other fields were unreadable anyway.
    }
    if (stored === undefined) delete object[location.field];
    else object[location.field] = stored;
    return Object.keys(object).length ? JSON.stringify(object) : null;
  };

  const save = (change: StoredChange): Promise<void> => {
    const id = slot(change.area, change.key);
    unsaved.set(id, { raw: change.raw, failed: false });
    notify(change.area, change.key);
    return provider.save([change]).then(
      () => {
        if (unsaved.get(id)?.raw === change.raw) unsaved.delete(id);
      },
      (error: unknown) => {
        const held = unsaved.get(id);
        if (held?.raw === change.raw) held.failed = true;
        throw error;
      },
    );
  };

  /// Writes a value to the layer that answers reads, or removes it.
  const write = <T>(setting: Setting<T>, value: T | undefined): Promise<void> => {
    const group = overriding(setting);
    if (group) {
      // A device value is stored even when it is the default or empty, so it
      // keeps overriding the account's value.
      const location = deviceLocation(setting, group);
      const own = value === undefined ? setting.defaultValue : value;
      const text = written("local", location, setting.serialize(own));
      parsed.set(memoId(setting, "local", location), { raw: text, value: own });
      return save({ area: "local", key: location.key, raw: text, persistence: "device", delayMs: setting.writeDelayMs });
    }
    const area = areaOf(setting);
    const empty = value === undefined || (setting.isEmpty?.(value) ?? false);
    const text = written(area, setting.location, empty ? undefined : setting.serialize(value as T));
    // The value read back is the one written, not a copy parsed from it.
    parsed.set(memoId(setting, area, setting.location), { raw: text, value: empty ? undefined : value });
    const saved = save({ area, key: setting.location.key, raw: text, persistence: setting.persistence, delayMs: setting.writeDelayMs });
    if (!setting.legacy) return saved;
    // The value has moved: the old location must not answer once this one is
    // emptied again.
    const legacy = setting.legacy;
    const before = raw(area, legacy.key);
    const after = written(area, legacy, undefined);
    if (before === after || decode(before, legacy) === undefined) return saved;
    return Promise.all([saved, save({ area, key: legacy.key, raw: after, persistence: setting.persistence })]).then(() => undefined);
  };

  const keysOf = (setting: AnySetting): Array<[StorageArea, string]> => {
    const keys: Array<[StorageArea, string]> = [[areaOf(setting), setting.location.key]];
    if (setting.legacy) keys.push([areaOf(setting), setting.legacy.key]);
    if (setting.persistence === "synced" && setting.override) {
      keys.push(["local", deviceOverride(setting.override).location.key]);
      keys.push(["local", deviceValuesKey(setting.override)]);
    }
    return keys;
  };

  return {
    get,
    read: (setting) => provider.ready.then(() => get(setting)),
    set: (setting, value) => write(setting, value),
    reset: (setting) => write(setting, undefined),
    subscribe(setting, listener) {
      let last = get(setting);
      const onChange = () => {
        const next = get(setting);
        if (Object.is(next, last)) return;
        last = next;
        listener(next);
      };
      const ids = keysOf(setting).map(([area, key]) => slot(area, key));
      for (const id of ids) {
        let set = listeners.get(id);
        if (!set) listeners.set(id, set = new Set());
        set.add(onChange);
      }
      return () => {
        for (const id of ids) {
          const set = listeners.get(id);
          set?.delete(onChange);
          if (set?.size === 0) listeners.delete(id);
        }
      };
    },
    ready: provider.ready,
    deviceOverride: (group) => get(deviceOverride(group)),
    async setDeviceOverride(group, on) {
      if (on === get(deviceOverride(group))) return;
      if (!on) {
        await Promise.all([
          write(deviceOverride(group), false),
          save({ area: "local", key: deviceValuesKey(group), raw: null, persistence: "device" }),
        ]);
        return;
      }
      const copies: Promise<void>[] = [];
      for (const setting of seen.values()) {
        if (setting.override !== group) continue;
        const location = deviceLocation(setting, group);
        const value = accountValue(setting);
        const text = written("local", location, setting.serialize(value));
        parsed.set(memoId(setting, "local", location), { raw: text, value });
        copies.push(save({ area: "local", key: location.key, raw: text, persistence: "device" }));
      }
      copies.push(write(deviceOverride(group), true));
      await Promise.all(copies);
    },
    flush: () => provider.flush(),
    discardUnsaved() {
      provider.discard();
      unsaved.clear();
      parsed.clear();
      notifyAll();
    },
  };
}

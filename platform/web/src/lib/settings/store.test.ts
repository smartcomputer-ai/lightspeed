// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createBrowserStorageProvider, LAST_USED_KEY } from "./browser-storage";
import { SETTINGS } from "./registry";
import { defineSetting, defineSettingFamily } from "./setting";
import { createSettingsStore } from "./store";

const flag = defineSetting<boolean>("test.flag", {
  key: "test:flag",
  format: "text",
  defaultValue: false,
  parse: (stored) => (stored === "true" ? true : stored === "false" ? false : undefined),
  serialize: String,
});
const shared = defineSettingFamily("test.shared", (field: string) => ({
  key: "test:shared",
  field,
  defaultValue: 0,
  parse: (stored: unknown) => (typeof stored === "number" ? stored : undefined),
  override: "display" as const,
}));
const draft = defineSetting<string>("test.draft", {
  persistence: "device",
  key: "test:draft",
  format: "text",
  defaultValue: "",
  parse: (stored) => (typeof stored === "string" ? stored : undefined),
  isEmpty: (text) => text === "",
  writeDelayMs: 100,
});
const open = defineSetting<boolean>("test.open", {
  persistence: "tab",
  key: "test:open",
  format: "text",
  defaultValue: false,
  parse: (stored) => stored === "1",
  serialize: (value) => (value ? "1" : "0"),
});

const fresh = (options: Parameters<typeof createBrowserStorageProvider>[0] = {}) =>
  createSettingsStore(createBrowserStorageProvider(options));

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("reading", () => {
  it.each([
    ["missing", null, false],
    ["unknown", "maybe", false],
    ["valid", "true", true],
  ] as const)("gives the default for a %s value and keeps a valid one", (_case, stored, expected) => {
    if (stored !== null) localStorage.setItem("test:flag", stored);
    expect(fresh().get(flag)).toBe(expected);
  });

  it("gives the default for broken JSON and for a shared object of the wrong shape", () => {
    const store = fresh();
    localStorage.setItem("test:shared", "{broken");
    expect(store.get(shared("a"))).toBe(0);
    localStorage.setItem("test:shared", "[1,2]");
    expect(store.get(shared("a"))).toBe(0);
    localStorage.setItem("test:shared", JSON.stringify({ a: "1", b: 2 }));
    expect(store.get(shared("a"))).toBe(0);
    expect(store.get(shared("b"))).toBe(2);
  });

  it("gives the same object while the stored value is unchanged", () => {
    const store = fresh();
    const columns = defineSetting<string[]>("test.columns", {
      key: "test:columns", defaultValue: [], parse: (value) =>
        Array.isArray(value) && value.every(item => typeof item === "string") ? value : undefined,
    });
    localStorage.setItem("test:columns", JSON.stringify(["name"]));
    const first = store.get(columns);
    expect(store.get(columns)).toBe(first);
    expect(first).toEqual(["name"]);
  });

  it("is ready at once, and read gives what get does", async () => {
    const store = fresh();
    localStorage.setItem("test:flag", "true");
    await store.ready;
    await expect(store.read(flag)).resolves.toBe(true);
  });
});

describe("writing", () => {
  it("applies at once, notifies, and resolves once saved", async () => {
    const store = fresh();
    const listener = vi.fn();
    store.subscribe(flag, listener);
    const saved = store.set(flag, true);
    expect(store.get(flag)).toBe(true);
    expect(listener).toHaveBeenCalledExactlyOnceWith(true);
    await saved;
    expect(localStorage.getItem("test:flag")).toBe("true");
  });

  it("keeps the other fields of a shared object, and drops the key once it is empty", async () => {
    const store = fresh();
    await store.set(shared("a"), 1);
    await store.set(shared("b"), 2);
    expect(JSON.parse(localStorage.getItem("test:shared")!)).toEqual({ a: 1, b: 2 });
    await store.reset(shared("a"));
    expect(JSON.parse(localStorage.getItem("test:shared")!)).toEqual({ b: 2 });
    await store.reset(shared("b"));
    expect(localStorage.getItem("test:shared")).toBeNull();
  });

  it("removes a value declared empty instead of storing it", async () => {
    const store = fresh();
    await store.set(draft, "hello");
    expect(localStorage.getItem("test:draft")).toBe("hello");
    await store.set(draft, "");
    expect(localStorage.getItem("test:draft")).toBeNull();
  });

  it("keeps synced and device settings in local storage and tab settings in session storage", async () => {
    const store = fresh();
    await Promise.all([store.set(flag, true), store.set(draft, "kept"), store.set(open, true)]);
    expect(localStorage.getItem("test:flag")).toBe("true");
    expect(localStorage.getItem("test:draft")).toBe("kept");
    expect(sessionStorage.getItem("test:open")).toBe("1");
    expect(localStorage.getItem("test:open")).toBeNull();
  });

  it("rejects a save that blocked storage refuses, keeps the value for the page, and never throws on read", async () => {
    const store = fresh();
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("blocked"); });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("full"); });
    expect(store.get(flag)).toBe(false);
    await expect(store.set(flag, true)).rejects.toThrow("full");
    expect(store.get(flag)).toBe(true);
  });

  it("works when the storage object itself cannot be reached", async () => {
    const store = fresh();
    vi.spyOn(window, "localStorage", "get").mockImplementation(() => { throw new Error("blocked"); });
    expect(store.get(flag)).toBe(false);
    await expect(store.set(flag, true)).rejects.toThrow();
    expect(store.get(flag)).toBe(true);
  });
});

describe("other tabs", () => {
  it("follows synced and device settings changed in another tab, the theme included", () => {
    const store = fresh();
    const theme = vi.fn();
    const device = vi.fn();
    store.subscribe(SETTINGS.theme, theme);
    store.subscribe(draft, device);
    localStorage.setItem("lightspeed:theme", "light");
    window.dispatchEvent(new StorageEvent("storage", { key: "lightspeed:theme" }));
    localStorage.setItem("test:draft", "from elsewhere");
    window.dispatchEvent(new StorageEvent("storage", { key: "test:draft" }));
    expect(theme).toHaveBeenCalledExactlyOnceWith("light");
    expect(device).toHaveBeenCalledExactlyOnceWith("from elsewhere");
  });

  it("returns to defaults when another tab clears storage, and lets a failed value give way", async () => {
    const store = fresh();
    localStorage.setItem("test:flag", "true");
    const listener = vi.fn();
    store.subscribe(flag, listener);
    localStorage.clear();
    window.dispatchEvent(new StorageEvent("storage", { key: null }));
    expect(listener).toHaveBeenCalledExactlyOnceWith(false);

    const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementationOnce(() => { throw new Error("full"); });
    await expect(store.set(flag, true)).rejects.toThrow();
    setItem.mockRestore();
    localStorage.setItem("test:flag", "false");
    window.dispatchEvent(new StorageEvent("storage", { key: "test:flag" }));
    expect(store.get(flag)).toBe(false);
  });
});

describe("held-back writes", () => {
  it("saves many quick changes as one write, after the delay", async () => {
    vi.useFakeTimers();
    const store = fresh();
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    const saves = ["h", "he", "hel", "hell", "hello"].map((text) => store.set(draft, text));
    expect(store.get(draft)).toBe("hello");
    expect(setItem).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(100);
    await Promise.all(saves);
    expect(setItem).toHaveBeenCalledExactlyOnceWith("test:draft", "hello");
  });

  it.each(["pagehide", "visibilitychange"] as const)("saves a pending write at once on %s", async (event) => {
    vi.useFakeTimers();
    const store = fresh();
    const saved = store.set(draft, "unsent");
    expect(localStorage.getItem("test:draft")).toBeNull();
    if (event === "pagehide") {
      window.dispatchEvent(new Event("pagehide"));
    } else {
      vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
      document.dispatchEvent(new Event("visibilitychange"));
    }
    expect(localStorage.getItem("test:draft")).toBe("unsent");
    await saved;
  });

  it("keeps a pending value over another tab's while it waits", async () => {
    vi.useFakeTimers();
    const store = fresh();
    void store.set(draft, "mine");
    localStorage.setItem("test:draft", "theirs");
    window.dispatchEvent(new StorageEvent("storage", { key: "test:draft" }));
    expect(store.get(draft)).toBe("mine");
    await vi.advanceTimersByTimeAsync(100);
    expect(localStorage.getItem("test:draft")).toBe("mine");
  });
});

describe("device overrides", () => {
  it("uses the account's values while the flag is off", async () => {
    const store = fresh();
    await store.set(shared("a"), 1);
    expect(JSON.parse(localStorage.getItem("test:shared")!)).toEqual({ a: 1 });
    expect(store.deviceOverride("display")).toBe(false);
  });

  it("copies the account's values when turned on, then keeps this device's apart", async () => {
    const store = fresh();
    await store.set(shared("a"), 1);
    const listener = vi.fn();
    store.subscribe(shared("a"), listener);
    await store.setDeviceOverride("display", true);
    expect(store.get(shared("a"))).toBe(1);
    expect(listener).not.toHaveBeenCalled();

    await store.set(shared("a"), 5);
    expect(store.get(shared("a"))).toBe(5);
    expect(JSON.parse(localStorage.getItem("test:shared")!)).toEqual({ a: 1 });
    // The account changes elsewhere; this device keeps its own.
    localStorage.setItem("test:shared", JSON.stringify({ a: 9, b: 3 }));
    window.dispatchEvent(new StorageEvent("storage", { key: "test:shared" }));
    expect(store.get(shared("a"))).toBe(5);
    // A setting not used before the flag went on follows the account until set here.
    expect(store.get(shared("b"))).toBe(3);
  });

  it("returns to the account's values when turned off, and does not bring old device values back", async () => {
    const store = fresh();
    await store.set(shared("a"), 1);
    await store.setDeviceOverride("display", true);
    await store.set(shared("a"), 5);
    await store.setDeviceOverride("display", false);
    expect(store.get(shared("a"))).toBe(1);
    expect(localStorage.getItem("lightspeed:device-settings:display")).toBeNull();
    await store.setDeviceOverride("display", true);
    expect(store.get(shared("a"))).toBe(1);
  });

  it("leaves other groups, device settings and tab settings alone", async () => {
    const store = fresh();
    await store.set(SETTINGS.theme, "light");
    await store.setDeviceOverride("display", true);
    await store.set(SETTINGS.theme, "system");
    await store.set(draft, "device");
    await store.set(open, true);
    expect(localStorage.getItem("lightspeed:theme")).toBe("system");
    expect(localStorage.getItem("test:draft")).toBe("device");
    expect(sessionStorage.getItem("test:open")).toBe("1");
    expect(JSON.parse(localStorage.getItem("lightspeed:device-overrides")!)).toEqual({ display: true });
  });
});

describe("expiry", () => {
  const day = 24 * 60 * 60 * 1000;
  const expiringPrefixes = ["test:drafts:"];

  it("tracks a module’s keys after it loads and expires them on a later page without the module", () => {
    const key = "module-draft:review";
    let now = 100 * day;
    localStorage.setItem(key, "review");
    localStorage.setItem("module-preference", "keep");
    const provider = createBrowserStorageProvider({ now: () => now });
    expect(localStorage.getItem(LAST_USED_KEY)).toBeNull();

    provider.registerExpiry(["module-draft:"]);
    expect(JSON.parse(localStorage.getItem(LAST_USED_KEY)!)).toEqual({ [key]: now });

    now += 31 * day;
    createBrowserStorageProvider({ now: () => now });
    expect(localStorage.getItem(key)).toBeNull();
    expect(localStorage.getItem("module-preference")).toBe("keep");
    expect(localStorage.getItem(LAST_USED_KEY)).toBeNull();
  });

  it("starts the clock for entries stored before expiry, and drops entries unused past their age", () => {
    let now = 1_000 * day;
    localStorage.setItem("test:drafts:old", "x");
    localStorage.setItem("test:other", "kept");
    fresh({ expiringPrefixes, now: () => now });
    expect(JSON.parse(localStorage.getItem(LAST_USED_KEY)!)).toEqual({ "test:drafts:old": now });

    now += 31 * day;
    fresh({ expiringPrefixes, now: () => now });
    expect(localStorage.getItem("test:drafts:old")).toBeNull();
    expect(localStorage.getItem("test:other")).toBe("kept");
    expect(localStorage.getItem(LAST_USED_KEY)).toBeNull();
  });

  it("counts a read or a write as use", async () => {
    vi.useFakeTimers();
    let now = 1_000 * day;
    const setting = defineSetting<string>("test.expiring", {
      persistence: "device",
      key: "test:drafts:a",
      format: "text",
      defaultValue: "",
      parse: (stored) => (typeof stored === "string" ? stored : undefined),
    });
    localStorage.setItem("test:drafts:a", "x");
    const store = fresh({ expiringPrefixes, now: () => now });
    now += 20 * day;
    expect(store.get(setting)).toBe("x");
    await vi.advanceTimersByTimeAsync(1_000);
    expect(JSON.parse(localStorage.getItem(LAST_USED_KEY)!)).toEqual({ "test:drafts:a": now });

    now += 20 * day;
    fresh({ expiringPrefixes, now: () => now });
    expect(localStorage.getItem("test:drafts:a")).toBe("x");
  });
});

describe("first paint", () => {
  const html = readFileSync(path.resolve(import.meta.dirname, "../../../index.html"), "utf8");
  const script = /<script>([\s\S]*?)<\/script>/.exec(html)![1]!;
  const paint = () => {
    document.documentElement.classList.remove("dark");
    new Function(script)();
    return document.documentElement.classList.contains("dark");
  };

  it("applies the stored theme before any code loads", async () => {
    expect(paint()).toBe(true);
    const store = fresh();
    await store.set(SETTINGS.theme, "light");
    expect(paint()).toBe(false);
    await store.set(SETTINGS.theme, "dark");
    expect(paint()).toBe(true);
  });

  it("applies this device's own theme while it keeps one", async () => {
    const store = fresh();
    await store.set(SETTINGS.theme, "dark");
    store.get(SETTINGS.theme);
    await store.setDeviceOverride("theme", true);
    await store.set(SETTINGS.theme, "light");
    expect(localStorage.getItem("lightspeed:theme")).toBe("dark");
    expect(paint()).toBe(false);
  });

  it("applies the reduced motion choice, following the system by default", async () => {
    const motion = () => {
      paint();
      return document.documentElement.dataset.motion;
    };
    expect(motion()).toBe("full");
    const store = fresh();
    await store.set(SETTINGS.reducedMotion, "on");
    expect(motion()).toBe("reduce");
    store.get(SETTINGS.reducedMotion);
    await store.setDeviceOverride("display", true);
    await store.set(SETTINGS.reducedMotion, "off");
    expect(localStorage.getItem("lightspeed:reduced-motion")).toBe("on");
    expect(motion()).toBe("full");
  });
});

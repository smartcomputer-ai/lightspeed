// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SubAgentIcon, SharedIcon, EnvironmentIcon } from "@/icons";
import { UserPreferencesProvider } from "@/lib/user-preferences";
import { fitMetaItems, MetaToolbar, type MetaItem, type MetaItemForm, type MetaMeasures } from "./meta-toolbar";

/// Measures where every tag is `tag` wide, shrunk tags `condensed` and
/// marks `mark`, with a 20px overflow and a 4px gap.
function measuresOf(items: MetaItem[], widths: Partial<Record<MetaItemForm, number>>): MetaMeasures {
  return { widths: new Map(items.map((item) => [item.key, widths])), overflow: 20, gap: 4 };
}

const item = (key: string, extra: Partial<MetaItem> = {}): MetaItem => ({ key, label: key, icon: SubAgentIcon, ...extra });

describe("fitMetaItems", () => {
  it("shows only the items of the place", () => {
    const items = [item("all"), item("list", { places: ["list"] }), item("page", { places: ["header", "strip"] }), item("strip", { places: ["strip"] })];
    const measures = measuresOf(items, { tag: 50, condensed: 20, mark: 20 });
    expect([...fitMetaItems(items, "list", 1000, measures).forms.keys()]).toEqual(["all", "list"]);
    expect([...fitMetaItems(items, "header", 1000, measures).forms.keys()]).toEqual(["all", "page"]);
    expect([...fitMetaItems(items, "strip", 1000, measures).forms.keys()]).toEqual(["all", "page", "strip"]);
  });

  it("keeps every tag whole while they fit", () => {
    const items = [item("a"), item("b"), item("c")];
    // 3 × 50 + 2 gaps of 4.
    const fit = fitMetaItems(items, "strip", 158, measuresOf(items, { tag: 50, condensed: 20 }));
    expect([...fit.forms.values()]).toEqual(["tag", "tag", "tag"]);
    expect(fit.hidden).toEqual([]);
  });

  it("shrinks the least important tags to icon and value before hiding any", () => {
    const items = [item("a", { priority: 2 }), item("b", { priority: 1 }), item("c", { priority: 3 })];
    // b shrinks: 50 + 20 + 50 + 8 = 128.
    const fit = fitMetaItems(items, "strip", 130, measuresOf(items, { tag: 50, condensed: 20 }));
    expect(Object.fromEntries(fit.forms)).toEqual({ a: "tag", b: "condensed", c: "tag" });
    expect(fit.hidden).toEqual([]);
  });

  it("shrinks the later of equally important tags first", () => {
    const items = [item("a"), item("b"), item("c")];
    const fit = fitMetaItems(items, "strip", 130, measuresOf(items, { tag: 50, condensed: 20 }));
    expect(Object.fromEntries(fit.forms)).toEqual({ a: "tag", b: "tag", c: "condensed" });
  });

  it("gives room left over back to tags that fit whole again", () => {
    const items = [item("a", { priority: 1 }), item("b", { priority: 0 }), item("c", { priority: 2 })];
    const measures: MetaMeasures = {
      widths: new Map([["a", { tag: 100, condensed: 20 }], ["b", { tag: 40, condensed: 20 }], ["c", { tag: 50, condensed: 20 }]]),
      overflow: 20,
      gap: 4,
    };
    // Shrinking b is not enough, so a shrinks too; then b fits whole again: 20 + 40 + 50 + 8 = 118.
    const fit = fitMetaItems(items, "strip", 120, measures);
    expect(Object.fromEntries(fit.forms)).toEqual({ a: "condensed", b: "tag", c: "tag" });
  });

  it("brings an item back from the overflow when room is left over", () => {
    const items = [item("a", { priority: 1 }), item("b", { priority: 0 }), item("c", { priority: 2 })];
    const measures: MetaMeasures = {
      widths: new Map([["a", { mark: 60 }], ["b", { mark: 10 }], ["c", { mark: 20 }]]),
      overflow: 20,
      gap: 4,
    };
    // Hiding b is not enough, so a goes too; then b fits back: 20 + 10 + 20 + 2 gaps = 58.
    const fit = fitMetaItems(items, "header", 60, measures);
    expect([...fit.forms.keys()]).toEqual(["b", "c"]);
    expect(fit.hidden).toEqual(["a"]);
  });

  it("does not shrink a tag without an icon", () => {
    const items = [item("a", { priority: 1 }), item("b", { icon: undefined })];
    const fit = fitMetaItems(items, "strip", 80, measuresOf(items, { tag: 50, condensed: 20 }));
    expect(Object.fromEntries(fit.forms)).toEqual({ a: "condensed", b: "tag" });
  });

  it("moves the least important into the overflow, listed in item order", () => {
    const items = [item("a", { priority: 1 }), item("b", { priority: 3 }), item("c", { priority: 0 }), item("d", { priority: 2 })];
    // Marks of 20: b and d with the overflow: 20 + 20 + 20 + 2 gaps = 68.
    const fit = fitMetaItems(items, "header", 70, measuresOf(items, { mark: 20 }));
    expect([...fit.forms.keys()]).toEqual(["b", "d"]);
    expect(fit.hidden).toEqual(["a", "c"]);
  });

  it("always keeps the most important item", () => {
    const items = [item("a", { priority: 1 }), item("b", { priority: 5 })];
    const fit = fitMetaItems(items, "strip", 10, measuresOf(items, { tag: 50, condensed: 20 }));
    expect(Object.fromEntries(fit.forms)).toEqual({ b: "condensed" });
    expect(fit.hidden).toEqual(["a"]);
  });

  it("counts a gap between the start and the end side", () => {
    const items = [item("a"), item("b", { align: "end" })];
    // 20 + 20 + one gap between them + one between the sides = 48.
    expect(fitMetaItems(items, "header", 48, measuresOf(items, { mark: 20 })).hidden).toEqual([]);
    expect(fitMetaItems(items, "header", 46, measuresOf(items, { mark: 20 })).hidden).toEqual(["b"]);
  });

  it("shows everything when not laid out", () => {
    const items = [item("a"), item("b")];
    const fit = fitMetaItems(items, "strip", 0, measuresOf(items, { tag: 500, condensed: 200 }));
    expect([...fit.forms.values()]).toEqual(["tag", "tag"]);
  });
});

describe("MetaToolbar", () => {
  let root: Root;
  let container: HTMLDivElement;
  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });
  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
  });

  const items: MetaItem[] = [
    { key: "agents", label: "3 sub-agents", icon: SubAgentIcon, value: 3, text: "3 sub-agents" },
    { key: "shared", label: "Shared", icon: SharedIcon, text: "Shared", places: ["strip"] },
    { key: "env", label: "Environment: build-box", icon: EnvironmentIcon, text: "build-box", tone: "danger", align: "end" },
  ];

  async function render(node: React.ReactNode) {
    await act(async () => root.render(<MemoryRouter>{node}</MemoryRouter>));
  }
  /// The items drawn, by accessible name, outside the measuring layer.
  const drawn = () =>
    [...container.querySelectorAll<HTMLElement>("[data-slot=meta-toolbar] > [aria-label], [data-slot=meta-toolbar] > span > [aria-label]")]
      .map((element) => element.getAttribute("aria-label"));

  it("draws compact marks as icon and value, with the end side last", async () => {
    await render(<MetaToolbar items={items} place="list" label="Session details" />);
    const toolbar = container.querySelector("[data-slot=meta-toolbar]")!;
    expect(toolbar.getAttribute("role")).toBe("group");
    expect(toolbar.getAttribute("aria-label")).toBe("Session details");
    expect(drawn()).toEqual(["3 sub-agents", "Environment: build-box"]);
    expect(container.querySelector('[aria-label="3 sub-agents"]')!.textContent).toBe("3");
  });

  it("draws full tags with their text", async () => {
    await render(<MetaToolbar items={items} place="strip" />);
    expect(drawn()).toEqual(["3 sub-agents", "Shared", "Environment: build-box"]);
    expect(container.querySelector('[aria-label="Environment: build-box"]')!.textContent).toBe("build-box");
  });

  it("makes items act only when interactive", async () => {
    const onSelect = vi.fn();
    const acting = [{ ...items[0]!, onSelect }, { ...items[1]!, href: "/u/demo/sessions/parent" }];
    await render(<MetaToolbar items={acting} place="strip" />);
    const button = container.querySelector<HTMLButtonElement>('button[aria-label="3 sub-agents"]')!;
    await act(async () => button.click());
    expect(onSelect).toHaveBeenCalledOnce();
    expect(container.querySelector('a[aria-label="Shared"]')!.getAttribute("href")).toBe("/u/demo/sessions/parent");

    await render(<MetaToolbar items={acting} place="strip" interactive={false} />);
    expect(container.querySelector("[data-slot=meta-toolbar] button, [data-slot=meta-toolbar] a")).toBeNull();
  });

  it("colors a tone always, or only while its row is highlighted", async () => {
    const icon = () => container.querySelector('[data-slot=meta-toolbar] > span > [aria-label="Environment: build-box"] svg')!.getAttribute("class")!.split(/\s+/);
    await render(<MetaToolbar items={items} place="list" tint="always" />);
    expect(icon()).toContain("text-danger");

    await render(<MetaToolbar items={items} place="list" />);
    expect(icon()).not.toContain("text-danger");
    expect(icon()).toEqual(expect.arrayContaining(["group-hover/row:text-danger", "group-data-highlighted/row:text-danger"]));
  });

  it("switches a header between icons and tags with its expand toggle", async () => {
    const onExpandedChange = vi.fn();
    await render(<MetaToolbar items={items} place="header" expandable onExpandedChange={onExpandedChange} />);
    const toggle = () => container.querySelector<HTMLButtonElement>('button[aria-pressed]')!;
    const agents = () => container.querySelector('[data-slot=meta-toolbar] > [aria-label="3 sub-agents"]')!.textContent;
    expect(toggle().getAttribute("aria-pressed")).toBe("false");
    expect(container.querySelector("[data-slot=meta-toolbar]")!.getAttribute("data-mode")).toBe("compact");
    expect(agents()).toBe("3");

    await act(async () => toggle().click());
    expect(onExpandedChange).toHaveBeenLastCalledWith(true);
    expect(toggle().getAttribute("aria-pressed")).toBe("true");
    expect(container.querySelector("[data-slot=meta-toolbar]")!.getAttribute("data-mode")).toBe("full");
    expect(agents()).toBe("3 sub-agents");

    await act(async () => toggle().click());
    expect(container.querySelector("[data-slot=meta-toolbar]")!.getAttribute("data-mode")).toBe("compact");
  });

  it("expands while hovered, after a short rest, and collapses after the pointer leaves", async () => {
    vi.useFakeTimers();
    try {
      await render(<MetaToolbar items={items} place="header" expandOnHover />);
      const toolbar = () => container.querySelector<HTMLElement>("[data-slot=meta-toolbar]")!;
      const mode = () => toolbar().getAttribute("data-mode");
      // The icon marks explain nothing on hover: hovering shows their words.
      expect(container.querySelectorAll("[data-slot=meta-toolbar] [data-slot=tooltip-trigger]")).toHaveLength(0);

      await act(async () => toolbar().dispatchEvent(new MouseEvent("pointerover", { bubbles: true, relatedTarget: document.body })));
      expect(mode()).toBe("compact");
      await act(async () => vi.advanceTimersByTime(200));
      expect(mode()).toBe("full");
      expect(container.querySelector('[data-slot=meta-toolbar] > [aria-label="3 sub-agents"]')!.textContent).toBe("3 sub-agents");

      await act(async () => toolbar().dispatchEvent(new MouseEvent("pointerout", { bubbles: true, relatedTarget: document.body })));
      expect(mode()).toBe("full");
      await act(async () => vi.advanceTimersByTime(300));
      expect(mode()).toBe("compact");
    } finally {
      vi.useRealTimers();
    }
  });

  it("locks every hover-expanding toolbar expanded, from the lock shown while expanded", async () => {
    vi.useFakeTimers();
    localStorage.clear();
    try {
      const page = (key: string) => (
        <UserPreferencesProvider userId="alice">
          <MetaToolbar key={key} items={items} place="header" expandOnHover />
        </UserPreferencesProvider>
      );
      const toolbar = () => container.querySelector<HTMLElement>("[data-slot=meta-toolbar]")!;
      const lock = () => container.querySelector<HTMLButtonElement>('button[aria-pressed][aria-label*="details"]');
      await render(page("first"));
      expect(lock()).toBeNull();

      await act(async () => toolbar().dispatchEvent(new MouseEvent("pointerover", { bubbles: true, relatedTarget: document.body })));
      await act(async () => vi.advanceTimersByTime(200));
      expect(lock()!.getAttribute("aria-pressed")).toBe("false");
      await act(async () => lock()!.click());
      expect(lock()!.getAttribute("aria-pressed")).toBe("true");

      // Another toolbar (another page) starts expanded, the pointer elsewhere.
      await render(page("second"));
      expect(toolbar().getAttribute("data-mode")).toBe("full");
      expect(lock()!.getAttribute("aria-pressed")).toBe("true");

      // Unlocked under the pointer, it collapses at once and stays collapsed
      // while the pointer stays.
      await act(async () => toolbar().dispatchEvent(new MouseEvent("pointerover", { bubbles: true, relatedTarget: document.body })));
      await act(async () => vi.advanceTimersByTime(200));
      await act(async () => lock()!.click());
      expect(toolbar().getAttribute("data-mode")).toBe("compact");
      await act(async () => vi.advanceTimersByTime(500));
      expect(toolbar().getAttribute("data-mode")).toBe("compact");

      // Leaving and coming back expands it on hover again.
      await act(async () => toolbar().dispatchEvent(new MouseEvent("pointerout", { bubbles: true, relatedTarget: document.body })));
      await act(async () => vi.advanceTimersByTime(300));
      await act(async () => toolbar().dispatchEvent(new MouseEvent("pointerover", { bubbles: true, relatedTarget: document.body })));
      await act(async () => vi.advanceTimersByTime(200));
      expect(toolbar().getAttribute("data-mode")).toBe("full");
    } finally {
      vi.useRealTimers();
      localStorage.clear();
    }
  });

  it("offers no toggle unless expandable", async () => {
    await render(<MetaToolbar items={items} place="header" />);
    expect(container.querySelector("button[aria-pressed]")).toBeNull();
    // Without hover-expanding, the icon marks explain themselves.
    expect(container.querySelectorAll("[data-slot=meta-toolbar] [data-slot=tooltip-trigger]").length).toBeGreaterThan(0);
  });

  it("renders nothing without items for the place", async () => {
    await render(<MetaToolbar items={[items[1]!]} place="header" />);
    expect(container.querySelector("[data-slot=meta-toolbar]")).toBeNull();
  });
});

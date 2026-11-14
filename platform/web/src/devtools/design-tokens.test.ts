// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
  DEFAULT_SHADOWS,
  isDefaultShadow,
  EMPTY_OVERRIDES,
  RADIUS_STEPS,
  SHADOW_STEPS,
  SHADOW_TOKENS,
  BACKDROP_KEY,
  SHEEN_ROLES,
  SIDEBAR_LOGO_TOKENS,
  sheenToken,
  SURFACE_COLOR_SOURCES,
  FONT_WEIGHT_STEPS,
  ITEM_WEIGHT_TOKENS,
  TRANSITION_TOKENS,
  CURSOR_STEPS,
  NOTE_TONES,
  noteToken,
  CURSOR_TOKENS,
  NEUTRAL_FILTER,
  backdropFilterValue,
  parseBackdropFilter,
  SURFACE_ROLES,
  applyOverrides,
  exportCss,
  loadOverrides,
  withoutDefaults,
  overridesStyleSheet,
  parseSurfaceColor,
  sameCss,
  saveOverrides,
  shadowValues,
  surfaceColorValue,
  surfaceToken,
  type DesignOverrides,
} from "./design-tokens";

const sample: DesignOverrides = {
  shared: { "--radius": "1rem" },
  light: { "--primary": "oklch(0.5 0.2 260)" },
  dark: { "--primary": "oklch(0.8 0.1 260)" },
};

afterEach(() => {
  document.head.innerHTML = "";
  localStorage.clear();
});

describe("design tokens", () => {
  it("generates each theme's shadow scale in index.css from its default parameters", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    const blocks = { light: /\n:root \{([\s\S]*?)\n\}/.exec(css)![1]!, dark: /\n\.dark \{([\s\S]*?)\n\}/.exec(css)![1]! };
    for (const theme of ["light", "dark"] as const) {
      const generated = shadowValues(DEFAULT_SHADOWS[theme]);
      for (const token of SHADOW_TOKENS) {
        const declared = new RegExp(`${token}: ([^;]+);`).exec(blocks[theme])?.[1];
        expect(generated[token], `${theme} ${token}`).toBe(declared);
      }
    }
  });

  it("declares every surface token in index.css in forms the editor reads", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    const declared = (token: string) => [...css.matchAll(new RegExp(`${token}: ([^;]+);`, "g"))].map((match) => match[1]!);
    for (const role of SURFACE_ROLES) {
      for (const part of ["background", "border"] as const) {
        const values = declared(surfaceToken(role.key, part));
        // Declared for :root and again for .dark, so an element with its own dark class re-resolves them.
        expect(values, `${role.key} ${part}`).toHaveLength(2);
        for (const value of values) expect(parseSurfaceColor(value), value).not.toBeNull();
      }
      const [width] = declared(surfaceToken(role.key, "border-width"));
      expect(width, `${role.key} border width`).toMatch(/^\d+(\.\d+)?px$/);
      const [radius] = declared(surfaceToken(role.key, "radius"));
      expect(RADIUS_STEPS.some((step) => sameCss(step.value, radius!)), radius).toBe(true);
      for (const shadow of declared(surfaceToken(role.key, "shadow"))) {
        expect(SHADOW_STEPS.some((step) => sameCss(step.value, shadow)), shadow).toBe(true);
      }
      if (!role.focus) continue;
      for (const part of ["focus-border", "focus-ring", "invalid-ring"] as const) {
        const values = declared(surfaceToken(role.key, part));
        expect(values, `${role.key} ${part}`).toHaveLength(2);
        for (const value of values) expect(parseSurfaceColor(value), value).not.toBeNull();
      }
      expect(declared(surfaceToken(role.key, "focus-ring-width"))[0]).toMatch(/^\d+(\.\d+)?px$/);
    }
  });

  it("draws every field's focus state with the field-focus utility", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    expect(css).toContain("@utility field-focus {");
    expect(css).toContain("@utility field-invalid {");
    // The components the Inputs surface lists, each focusing through the shared tokens.
    for (const name of ["input", "textarea", "select", "input-group", "combobox"]) {
      const source = readFileSync(path.resolve(import.meta.dirname, `../components/ui/${name}.tsx`), "utf8");
      expect(source, name).toMatch(/:field-focus\b/);
      expect(source, name).toMatch(/:field-invalid\b/);
      expect(source, name).not.toMatch(/(focus(-visible|-within)?|invalid\]?):(inset-)?ring-(ring|destructive)\//);
    }
  });

  it("draws every editable surface color from a source the editor's picker offers", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    const offered = new Set<string>(["transparent", ...SURFACE_COLOR_SOURCES]);
    const editable = [
      ...SURFACE_ROLES.flatMap((role) => [
        surfaceToken(role.key, "background"),
        surfaceToken(role.key, "border"),
        ...(role.focus ? [surfaceToken(role.key, "focus-border"), surfaceToken(role.key, "focus-ring"), surfaceToken(role.key, "invalid-ring")] : []),
      ]),
      surfaceToken(BACKDROP_KEY, "background"),
      ...SHEEN_ROLES.map((role) => sheenToken(role.key)),
    ];
    for (const token of editable) {
      for (const [, value] of css.matchAll(new RegExp(`${token}: ([^;]+);`, "g"))) {
        const source = parseSurfaceColor(value!)?.source;
        expect(source && offered.has(source), `${token}: ${value}`).toBe(true);
      }
    }
  });

  it("declares every note tone's colors in both themes", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    for (const { tone } of NOTE_TONES) {
      for (const part of ["background", "border", "title"] as const) {
        const name = noteToken(tone, part);
        expect([...css.matchAll(new RegExp(`${name}: ([^;]+);`, "g"))], name).toHaveLength(2);
      }
    }
  });

  it("declares every cursor role once, as a cursor the editor offers", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    for (const { token } of CURSOR_TOKENS) {
      const values = [...css.matchAll(new RegExp(`${token}: ([^;]+);`, "g"))].map((match) => match[1]!);
      expect(values, token).toHaveLength(1);
      expect(CURSOR_STEPS.map((step) => step.value), token).toContain(values[0]);
    }
  });

  it("declares every standard transition time in milliseconds", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    for (const { token } of TRANSITION_TOKENS) {
      const values = [...css.matchAll(new RegExp(`${token}: ([^;]+);`, "g"))].map((match) => match[1]!);
      expect(values, token).toHaveLength(1);
      expect(values[0], token).toMatch(/^\d+ms$/);
    }
  });

  it("declares list and menu weights as weight steps the editor offers", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    for (const { token } of ITEM_WEIGHT_TOKENS) {
      const values = [...css.matchAll(new RegExp(`${token}: ([^;]+);`, "g"))].map((match) => match[1]!);
      expect(values, token).toHaveLength(1);
      expect(FONT_WEIGHT_STEPS.some((step) => sameCss(step.value, values[0]!)), `${token}: ${values[0]}`).toBe(true);
    }
  });

  it("declares the backdrop and the menu effects in index.css", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    const declared = (token: string) => [...css.matchAll(new RegExp(`${token}: ([^;]+);`, "g"))].map((match) => match[1]!);
    const colors = declared(surfaceToken(BACKDROP_KEY, "background"));
    expect(colors).toHaveLength(2);
    // Editable by the configurator: a known color at an opacity.
    for (const value of colors) expect(parseSurfaceColor(value), value).not.toBeNull();
    for (const key of [BACKDROP_KEY, ...SURFACE_ROLES.filter((role) => role.effects).map((role) => role.key)]) {
      const filters = declared(surfaceToken(key, "filter"));
      // Shared by both themes, in the one form the editor writes (so never a neutral non-`none`).
      expect(filters, key).toHaveLength(1);
      const parsed = parseBackdropFilter(filters[0]!);
      expect(parsed, `${key}: ${filters[0]}`).not.toBeNull();
      expect(backdropFilterValue(parsed!), key).toBe(filters[0]);
      expect(css, key).toContain(`@utility surface-filter-${key} {`);
    }
  });

  it("applies each surface's backdrop filter on the surface itself", () => {
    const uses: Record<string, string[]> = {
      backdrop: ["dialog", "alert-dialog", "sheet"],
      menu: ["dropdown-menu", "select", "combobox"],
      popover: ["popover"],
      dialog: ["dialog", "alert-dialog", "sheet"],
    };
    for (const [key, files] of Object.entries(uses)) {
      for (const name of files) {
        const source = readFileSync(path.resolve(import.meta.dirname, `../components/ui/${name}.tsx`), "utf8");
        expect(source, `${name} ${key}`).toContain(`surface-filter-${key}`);
        // A blur layer under the content would paint over the inset border.
        expect(source, name).not.toMatch(/before:backdrop-|backdrop-(blur|saturate)-\(/);
      }
    }
  });

  it("writes a neutral backdrop filter as none and reads back what it writes", () => {
    expect(backdropFilterValue({ blur: 0, saturate: 100 })).toBe("none");
    expect(backdropFilterValue({ blur: 12, saturate: 100 })).toBe("blur(12px)");
    expect(backdropFilterValue({ blur: 0, saturate: 45 })).toBe("saturate(45%)");
    expect(backdropFilterValue({ blur: 40, saturate: 150 })).toBe("blur(40px) saturate(150%)");
    for (const value of ["none", "blur(12px)", "saturate(45%)", "blur(40px) saturate(150%)"]) {
      expect(backdropFilterValue(parseBackdropFilter(value)!)).toBe(value);
    }
    expect(parseBackdropFilter("blur(0px) saturate(100%)")).toEqual(NEUTRAL_FILTER);
    expect(parseBackdropFilter("brightness(2)")).toBeNull();
  });

  it("declares each sheen's color and start per theme and draws it as a gradient", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    for (const role of SHEEN_ROLES) {
      const values = [...css.matchAll(new RegExp(`${sheenToken(role.key)}: ([^;]+);`, "g"))].map((match) => match[1]!);
      expect(values, role.key).toHaveLength(2);
      for (const value of values) expect(parseSurfaceColor(value), value).not.toBeNull();
      expect(css).toContain(`@utility sheen-${role.key} {`);
      const starts = [...css.matchAll(new RegExp(`${sheenToken(role.key, "start")}: ([^;]+);`, "g"))].map((match) => match[1]!);
      expect(starts, role.key).toHaveLength(2);
      for (const start of starts) expect(start).toMatch(/^\d+(\.\d+)?%$/);
      expect(css).toContain(
        `linear-gradient(90deg, transparent var(${sheenToken(role.key, "start")}), var(${sheenToken(role.key)}) 100%)`,
      );
      if (!role.background) continue;
      // The surface's own background, per theme, in a form the color row reads (a theme color or a literal).
      const backgrounds = [...css.matchAll(new RegExp(`${role.background}: ([^;]+);`, "g"))].map((match) => match[1]!);
      expect(backgrounds, role.background).toHaveLength(2);
    }
  });

  it("declares the sidebar logo's color and opacity per theme and its geometry once", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    const declared = (token: string) => [...css.matchAll(new RegExp(`${token}: ([^;]+);`, "g"))].map((match) => match[1]!);
    const colors = declared(SIDEBAR_LOGO_TOKENS.color);
    expect(colors).toHaveLength(2);
    for (const value of colors) expect(parseSurfaceColor(value), value).not.toBeNull();
    const opacities = declared(SIDEBAR_LOGO_TOKENS.opacity);
    expect(opacities).toHaveLength(2);
    for (const value of opacities) expect(value).toMatch(/^(0|1)?(\.\d+)?$/);
    expect(declared(SIDEBAR_LOGO_TOKENS.rotate)).toEqual([expect.stringMatching(/^-?\d+(\.\d+)?deg$/)]);
    expect(declared(SIDEBAR_LOGO_TOKENS.scale)).toEqual([expect.stringMatching(/^\d+(\.\d+)?$/)]);
    expect(declared(SIDEBAR_LOGO_TOKENS.offset)).toEqual([expect.stringMatching(/^-?\d+(\.\d+)?%$/)]);
    expect(css).toContain("@utility sidebar-logo {");
  });

  it("declares the icon scale and applies it to every icon role", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    expect(css).toMatch(/--icon-scale: \d+(\.\d+)?;/);
    expect(css).toMatch(/svg\[data-icon-role\] \{\s*zoom: var\(--icon-scale\);/);
  });

  it("round-trips surface colors and leaves other colors custom", () => {
    expect(surfaceColorValue({ source: "white", percent: 40 })).toBe("color-mix(in oklab, white 40%, transparent)");
    expect(parseSurfaceColor("black")).toEqual({ source: "black", percent: 100 });
    expect(surfaceColorValue({ source: "--popover", percent: 70 })).toBe("color-mix(in oklab, var(--popover) 70%, transparent)");
    expect(parseSurfaceColor("color-mix(in oklab, var(--popover) 70%, transparent)")).toEqual({ source: "--popover", percent: 70 });
    expect(surfaceColorValue({ source: "--card", percent: 100 })).toBe("var(--card)");
    expect(parseSurfaceColor(" var(--card) ")).toEqual({ source: "--card", percent: 100 });
    expect(surfaceColorValue({ source: "--card", percent: 0 })).toBe("color-mix(in oklab, var(--card) 0%, transparent)");
    expect(parseSurfaceColor("color-mix(in oklab, var(--card) 0%, transparent)")).toEqual({ source: "--card", percent: 0 });
    expect(surfaceColorValue({ source: "transparent", percent: 50 })).toBe("transparent");
    expect(parseSurfaceColor("transparent")).toEqual({ source: "transparent", percent: 0 });
    expect(parseSurfaceColor("oklch(0.5 0.1 200)")).toBeNull();
    expect(sameCss("calc(var(--radius) * 0.8)", "calc(var(--radius) * .8)")).toBe(true);
  });

  it("scales shadow alpha and geometry and tints the color", () => {
    const values = shadowValues({ color: "#ff0000", opacity: 2, size: 2 });
    expect(values["--shadow-xs"]).toBe("0 2px 4px 0 rgb(255 0 0 / 0.1)");
    expect(shadowValues({ ...DEFAULT_SHADOWS.dark, opacity: 10 })["--shadow-2xl"]).toContain("/ 1)");
  });

  it("moves shadows up and down by scaling their vertical offset", () => {
    const lg = (offsetY?: number) => shadowValues({ ...DEFAULT_SHADOWS.light, offsetY })["--shadow-lg"];
    expect(lg(undefined)).toBe(lg(1));
    expect(lg(0)).toBe("0 0 7.5px -1.5px rgb(0 0 0 / 0.1), 0 0 3px -2px rgb(0 0 0 / 0.1)");
    expect(lg(-1)).toBe("0 -5px 7.5px -1.5px rgb(0 0 0 / 0.1), 0 -2px 3px -2px rgb(0 0 0 / 0.1)");
    expect(isDefaultShadow({ ...DEFAULT_SHADOWS.dark, offsetY: undefined }, "dark")).toBe(true);
    expect(isDefaultShadow({ ...DEFAULT_SHADOWS.light, offsetY: 0.5 }, "light")).toBe(false);
  });

  it("scopes overrides so they outrank index.css in both themes", () => {
    const sheet = overridesStyleSheet(sample);
    expect(sheet).toContain(":root:root {\n  --radius: 1rem;\n}");
    expect(sheet).toContain(":root:not(.dark) {\n  --primary: oklch(0.5 0.2 260);\n}");
    expect(sheet).toContain(".dark.dark {\n  --primary: oklch(0.8 0.1 260);\n}");
    expect(overridesStyleSheet(EMPTY_OVERRIDES)).toBe("");
  });

  it("exports rules shaped like index.css", () => {
    expect(exportCss(sample)).toBe(
      ":root {\n    --radius: 1rem;\n    --primary: oklch(0.5 0.2 260);\n}\n\n.dark {\n    --primary: oklch(0.8 0.1 260);\n}",
    );
    expect(exportCss(EMPTY_OVERRIDES)).toBe("/* No changes yet. */");
  });

  it("injects, replaces and removes one style element", () => {
    applyOverrides(sample);
    applyOverrides({ ...sample, shared: { "--radius": "2rem" } });
    const styles = document.head.querySelectorAll("style#lightspeed-design-overrides");
    expect(styles).toHaveLength(1);
    expect(styles[0]!.textContent).toContain("--radius: 2rem;");
    applyOverrides(EMPTY_OVERRIDES);
    expect(document.getElementById("lightspeed-design-overrides")).toBeNull();
  });

  it("persists overrides and clears storage when they are empty", () => {
    saveOverrides(sample);
    expect(loadOverrides()).toEqual(sample);
    saveOverrides(EMPTY_OVERRIDES);
    expect(localStorage.getItem("lightspeed:design-overrides")).toBeNull();
    localStorage.setItem("lightspeed:design-overrides", "{not json");
    expect(loadOverrides()).toEqual(EMPTY_OVERRIDES);
  });
});

describe("withoutDefaults", () => {
  const defaults = {
    light: { "--radius": "0.6rem", "--note-info-border": "color-mix(in oklab, var(--info) 40%, transparent)" },
    dark: { "--radius": "0.6rem", "--note-info-border": "color-mix(in oklab, var(--info) 0%, transparent)" },
  };

  it("drops overrides the stylesheet has made its defaults, so they no longer count or export", () => {
    const pruned = withoutDefaults({
      shared: { "--radius": " 0.6rem", "--spacing": "0.3rem" },
      light: { "--note-info-border": "color-mix(in oklab, var(--info) 40%, transparent)" },
      dark: { "--note-info-border": "color-mix(in oklab,  var(--info) 0%, transparent)" },
    }, defaults);
    expect(pruned).toEqual({ shared: { "--spacing": "0.3rem" }, light: {}, dark: {} });
    expect(exportCss(pruned)).not.toContain("--note-info-border");
  });

  it("keeps changes from the defaults, per theme", () => {
    const overrides = { shared: {}, light: {}, dark: { "--note-info-border": "color-mix(in oklab, var(--info) 40%, transparent)" } };
    expect(withoutDefaults(overrides, defaults)).toEqual(overrides);
  });

  it("keeps a theme's shadow inputs only while a shadow variable they made remains", () => {
    const params = { color: "#112233", opacity: 2, size: 1 };
    const shadow = { light: params, dark: params };
    expect(withoutDefaults({ ...EMPTY_OVERRIDES, shadow }, defaults).shadow).toBeUndefined();
    expect(withoutDefaults({ ...EMPTY_OVERRIDES, dark: { [SHADOW_TOKENS[0]]: "0 1px red" }, shadow }, defaults).shadow)
      .toEqual({ dark: params });
  });
});

describe("shadow overrides saved before the themes had their own", () => {
  it("become both themes' shadows", () => {
    const params = { color: "#112233", opacity: 2, size: 1 };
    localStorage.setItem("lightspeed:design-overrides", JSON.stringify({
      shared: { "--radius": "1rem", "--shadow-lg": "0 1px red" },
      light: {},
      dark: { "--primary": "red" },
      shadow: params,
    }));
    expect(loadOverrides()).toEqual({
      shared: { "--radius": "1rem" },
      light: { "--shadow-lg": "0 1px red" },
      dark: { "--primary": "red", "--shadow-lg": "0 1px red" },
      shadow: { light: params, dark: params },
    });
  });

  it("leave current overrides alone", () => {
    const current = { shared: {}, light: { "--shadow-lg": "0 1px red" }, dark: {}, shadow: { light: { color: "#000000", opacity: 2, size: 1 } } };
    saveOverrides(current);
    expect(loadOverrides()).toEqual(current);
  });
});

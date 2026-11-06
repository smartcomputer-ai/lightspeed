// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
  DEFAULT_SHADOW,
  EMPTY_OVERRIDES,
  RADIUS_STEPS,
  SHADOW_STEPS,
  SHADOW_TOKENS,
  SURFACE_ROLES,
  applyOverrides,
  exportCss,
  loadOverrides,
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
  it("generates index.css's shadow scale from the default parameters", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    const generated = shadowValues(DEFAULT_SHADOW);
    // 2xs has no blur; index.css writes it in Tailwind's short form. The
    // `@theme inline` aliases (`var(--shadow-xs)`) are skipped.
    for (const token of SHADOW_TOKENS.filter((name) => name !== "--shadow-2xs")) {
      const declared = new RegExp(`${token}: (?!var)([^;]+);`).exec(css)?.[1];
      expect(generated[token], token).toBe(declared);
    }
  });

  it("declares every surface token in index.css in forms the editor reads", () => {
    const css = readFileSync(path.resolve(import.meta.dirname, "../index.css"), "utf8");
    const declared = (token: string) => [...css.matchAll(new RegExp(`${token}: ([^;]+);`, "g"))].map((match) => match[1]!);
    for (const role of SURFACE_ROLES) {
      for (const part of ["background", "border"] as const) {
        const values = declared(surfaceToken(role.key, part));
        // Declared for :root and again for .dark, so popups with their own dark class re-resolve them.
        expect(values, `${role.key} ${part}`).toHaveLength(2);
        for (const value of values) expect(parseSurfaceColor(value), value).not.toBeNull();
      }
      const [width] = declared(surfaceToken(role.key, "border-width"));
      expect(width).toBe("1px");
      const [radius] = declared(surfaceToken(role.key, "radius"));
      expect(RADIUS_STEPS.some((step) => sameCss(step.value, radius!)), radius).toBe(true);
      const [shadow] = declared(surfaceToken(role.key, "shadow"));
      expect(SHADOW_STEPS.some((step) => sameCss(step.value, shadow!)), shadow).toBe(true);
    }
  });

  it("round-trips surface colors and leaves other colors custom", () => {
    expect(surfaceColorValue({ source: "--popover", percent: 70 })).toBe("color-mix(in oklab, var(--popover) 70%, transparent)");
    expect(parseSurfaceColor("color-mix(in oklab, var(--popover) 70%, transparent)")).toEqual({ source: "--popover", percent: 70 });
    expect(surfaceColorValue({ source: "--card", percent: 100 })).toBe("var(--card)");
    expect(parseSurfaceColor(" var(--card) ")).toEqual({ source: "--card", percent: 100 });
    expect(surfaceColorValue({ source: "--card", percent: 0 })).toBe("transparent");
    expect(parseSurfaceColor("transparent")).toEqual({ source: "transparent", percent: 0 });
    expect(parseSurfaceColor("oklch(0.5 0.1 200)")).toBeNull();
    expect(sameCss("calc(var(--radius) * 0.8)", "calc(var(--radius) * .8)")).toBe(true);
  });

  it("scales shadow alpha and geometry and tints the color", () => {
    const values = shadowValues({ color: "#ff0000", opacity: 2, size: 2 });
    expect(values["--shadow-xs"]).toBe("0 2px 4px 0 rgb(255 0 0 / 0.1)");
    expect(shadowValues({ ...DEFAULT_SHADOW, opacity: 10 })["--shadow-2xl"]).toContain("/ 1)");
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

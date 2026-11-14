import { expect, it } from "vitest";
import { idColor, idHue, idHueColor } from "./identity-colors";

it("gives an id the same hue every time, in 0–359", () => {
  for (const id of ["implementer", "ci-watch", "", "a", "x".repeat(500), "ünïcødé · 漢字"]) {
    const hue = idHue(id);
    expect(hue).toBe(idHue(id));
    expect(Number.isInteger(hue)).toBe(true);
    expect(hue).toBeGreaterThanOrEqual(0);
    expect(hue).toBeLessThan(360);
  }
});

it("keeps the hues bots already have", () => {
  // Changing the hash would recolour every existing bot; these are the
  // hues bots had before the hash moved here.
  expect(["implementer", "planner", "pr-reviewer"].map(idHue)).toEqual([25, 331, 257]);
});

it("spreads similar ids across the wheel", () => {
  const hues = new Set(Array.from({ length: 50 }, (_, index) => idHue(`bot-${index}`)));
  expect(hues.size).toBeGreaterThan(40);
});

it("draws the hue at the theme's id-colour lightness and chroma", () => {
  expect(idHueColor(120)).toBe("oklch(var(--id-color-lightness) var(--id-color-chroma) 120)");
  expect(idColor("planner")).toBe(idHueColor(idHue("planner")));
});

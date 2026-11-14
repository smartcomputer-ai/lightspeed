import { mkdirSync } from "node:fs";
import path from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { SCREENS, THEMES, VIEWPORT, type Theme } from "./screens";

/// One test per screen and theme. Each writes `<id>@<viewport>-<theme>.png`
/// into SCREENSHOTS_OUT. This harness comes from the current tree and drives
/// both builds of a comparison, so both sides capture the same screens.
const out = process.env.SCREENSHOTS_OUT;
if (!process.env.SCREENSHOTS_BASE_URL || !out) {
  throw new Error("Run through `npm run screenshots:demo` or `npm run screendiff:demo`.");
}
const themes = (process.env.SCREENSHOTS_THEMES?.split(",") ?? THEMES) as Theme[];

/// Everything the page could vary on is pinned from outside the app, so it
/// works for any commit, including ones that predate this harness.
const FIXED_TIME = new Date("2026-09-01T09:00:00.000Z");
const STILL_CSS = `*, *::before, *::after {
  animation-duration: 0s !important; animation-delay: 0s !important;
  transition-duration: 0s !important; transition-delay: 0s !important;
  caret-color: transparent !important;
}`;

mkdirSync(out, { recursive: true });

for (const theme of themes) {
  for (const screen of SCREENS) {
    test(`${screen.id} (${theme})`, async ({ page }) => {
      await pin(page, theme);
      const response = await page.goto(screen.path);
      expect(response?.ok(), `GET ${screen.path}`).toBe(true);
      await page.addStyleTag({ content: STILL_CSS });
      await settle(page);
      await page.screenshot({ path: path.join(out, `${screen.id}@${VIEWPORT.name}-${theme}.png`), fullPage: true, animations: "disabled", caret: "hide" });
    });
  }
}

async function pin(page: Page, theme: Theme) {
  await page.emulateMedia({ colorScheme: theme, reducedMotion: "reduce" });
  await page.clock.setFixedTime(FIXED_TIME);
  await page.addInitScript((stored) => {
    localStorage.setItem("lightspeed:theme", stored);
    // The welcome dialog would cover every screen; it is dismissed for good.
    localStorage.setItem("lightspeed:welcome-dismissed", "true");
    // Seeded PRNG and counter UUIDs: ids and jitter repeat run to run.
    let seed = 0x2f6b9a1d;
    Math.random = () => {
      seed = (seed + 0x6d2b79f5) | 0;
      let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
    let uuid = 0;
    crypto.randomUUID = () => `00000000-0000-4000-8000-${(++uuid).toString(16).padStart(12, "0")}` as `${string}-${string}-${string}-${string}-${string}`;
  }, theme);
}

/// Ready when fonts are in, nothing reports loading, and two consecutive
/// frames match. The demo backend runs in the page, so there is no network
/// idle to wait for.
async function settle(page: Page) {
  await page.evaluate(() => document.fonts.ready);
  await expect(page.getByText(/^Loading…?$/)).toHaveCount(0);
  await expect(page.locator('[aria-busy="true"]')).toHaveCount(0);
  let previous = await page.screenshot({ fullPage: true, animations: "disabled", caret: "hide" });
  for (let attempt = 0; attempt < 10; attempt++) {
    await page.waitForTimeout(150);
    const next = await page.screenshot({ fullPage: true, animations: "disabled", caret: "hide" });
    if (next.equals(previous)) return;
    previous = next;
  }
  throw new Error("The page kept changing; mask or pin whatever moves.");
}

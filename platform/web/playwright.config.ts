import { defineConfig } from "@playwright/test";
import { VIEWPORT } from "./e2e/screens";

/// Browser tests against the built demo. `scripts/screendiff.mjs` serves the
/// build and sets the environment. Specs are `*.pw.ts` so Vitest's default
/// `*.test`/`*.spec` glob skips them.
export default defineConfig({
  testDir: "e2e",
  testMatch: "*.pw.ts",
  // Playwright empties this before each run; it must not be test-results/,
  // where one side's screenshots wait for the other side's capture.
  outputDir: "node_modules/.cache/playwright",
  fullyParallel: true,
  forbidOnly: true,
  retries: 0,
  timeout: 60_000,
  reporter: [["list"]],
  use: {
    baseURL: process.env.SCREENSHOTS_BASE_URL,
    browserName: "chromium",
    viewport: { width: VIEWPORT.width, height: VIEWPORT.height },
    // High-DPI captures (2880×1800 for the desktop viewport), so small text
    // and one-pixel borders diff at the resolution people review on.
    deviceScaleFactor: VIEWPORT.scale,
    locale: "en-US",
    timezoneId: "UTC",
    reducedMotion: "reduce",
  },
});

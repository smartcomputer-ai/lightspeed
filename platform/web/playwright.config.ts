import { defineConfig } from "@playwright/test";
import { VIEWPORT } from "./e2e/screens";

// Screenshot comparisons supply their own build, including historical versions.
// Functional specs run against the current distribution in demo mode.
const screenshots = Boolean(process.env.SCREENSHOTS_OUT);
const demoUrl = "http://127.0.0.1:4187/demo/";
export default defineConfig({
  testDir: "e2e",
  testMatch: screenshots ? "visual.pw.ts" : "modules.pw.ts",
  // Playwright empties this before each run; it must not be test-results/,
  // where one side's screenshots wait for the other side's capture.
  outputDir: "node_modules/.cache/playwright",
  fullyParallel: true,
  forbidOnly: true,
  retries: 0,
  timeout: 60_000,
  reporter: [["list"]],
  webServer: screenshots ? undefined : {
    command: "npm run dev:demo -- --host 127.0.0.1 --port 4187",
    url: demoUrl,
    timeout: 90_000,
  },
  use: {
    baseURL: screenshots ? process.env.SCREENSHOTS_BASE_URL : demoUrl,
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

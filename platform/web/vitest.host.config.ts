import path from "node:path";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Exercise the shell and its services in a checkout with no module catalog.
export default defineConfig({
  base: "/app/",
  plugins: [react()],
  define: { __LIGHTSPEED_VERSION__: JSON.stringify("0.0.0-test") },
  resolve: { alias: { "@": path.join(import.meta.dirname, "src") } },
  test: {
    setupFiles: ["./src/test/fresh-settings.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});

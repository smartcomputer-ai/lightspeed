import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The SPA lives under /app on the platform origin (path-based routing).
// In dev, vite proxies API calls to the local server so the better-auth
// cookie flow works without CORS.
export default defineConfig({
  base: "/app/",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(import.meta.dirname, "./src"),
    },
  },
  server: {
    port: 5173,
    // Fail instead of hopping to 5174: only :5173 is a trusted auth origin,
    // so a silently shifted port would break sign-in.
    strictPort: true,
    proxy: {
      "/api": "http://localhost:3000",
    },
  },
});

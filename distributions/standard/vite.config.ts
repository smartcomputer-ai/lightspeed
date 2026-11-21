/// <reference types="vitest/config" />
import type { ModuleRouteEntries } from "@lightspeed-ai/platform-shared";
import { readFileSync } from "node:fs";
import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { standardModules } from "./catalog.js";
import { todoGuard } from "../../platform/web/build/todo-guard.js";
import { defineConfig, type Plugin } from "vite";

const distributionDir = import.meta.dirname;
const webRoot = path.resolve(distributionDir, "../../platform/web");

// The SPA lives under /app on the platform origin (path-based routing).
// In dev, vite proxies API calls to the local server so the better-auth
// cookie flow works without CORS.
//
// `--mode demo` is the second build path: the same app with the in-browser
// backend (src/demo) swapped in as the entry, no proxy, and its own output
// directory, so it can be published as a static site.
export default defineConfig(({ mode }) => {
  const demo = mode === "demo";
  return {
    root: webRoot,
    base: demo ? "/demo/" : "/app/",
    define: {
      __LIGHTSPEED_VERSION__: JSON.stringify(releaseVersion()),
    },
    plugins: [react(), tailwindcss(), todoGuard(webRoot), moduleEntries("web"), moduleEntries("demo")],
    resolve: {
      alias: {
        "/distribution-entry.ts": path.join(distributionDir, demo ? "demo-main.ts" : "main.ts"),
        "@distribution": distributionDir,
        "@": path.join(webRoot, "src"),
      },
    },
    build: demo ? { outDir: "dist-demo" } : undefined,
    test: {
      setupFiles: ["./src/test/fresh-settings.ts"],
      include: [
        "src/**/*.test.{ts,tsx}",
        "../../modules/*/{components,demo,web}/**/*.test.{ts,tsx}",
        "../../distributions/standard/test/web/**/*.test.{ts,tsx}",
      ],
    },
    server: {
      port: demo ? 5175 : 5173,
      // Fail instead of hopping to 5174: only :5173 is a trusted auth origin,
      // so a silently shifted port would break sign-in.
      strictPort: true,
      proxy: demo
        ? undefined
        : {
            "/api": `http://localhost:${process.env.PORT ?? "3000"}`,
            // Device login starts at the public origin; the backend redirects
            // this entry point to the SPA's /app/device approval page.
            "/device": `http://localhost:${process.env.PORT ?? "3000"}`,
          },
    },
  };
});

// Import the chosen native entry points at build time. An omitted module has
// no import (including its lazy page chunks) in the distribution.
function moduleEntries(kind: "web" | "demo"): Plugin {
  const name = `virtual:lightspeed/${kind}-module-entries`;
  const internalId = "\0" + name;
  return {
    name: `lightspeed-${kind}-module-entries`,
    resolveId(id) { if (id === name) return internalId; },
    load(id) {
      if (id !== internalId) return;
      const imports: string[] = [];
      const entries: string[] = [];
      const importEntry = (specifier: string) => {
        const source = specifier.startsWith(".") ? path.resolve(distributionDir, specifier) : specifier;
        const binding = `module${imports.length}`;
        imports.push(`import ${binding} from ${JSON.stringify(source)};`);
        return binding;
      };
      const importDemoRoutes = (routes: ModuleRouteEntries<string>) => {
        const factories: string[] = [];
        if (routes.browserRoutes) factories.push(`browserRoutes: ${importEntry(routes.browserRoutes)}`);
        if (routes.publicOperations) factories.push(`publicOperations: ${importEntry(routes.publicOperations)}`);
        return `{ ${factories.join(", ")} }`;
      };
      for (const module of standardModules) {
        const entry = module[kind];
        if (!entry) continue;
        const value = typeof entry === "string" ? importEntry(entry) : importDemoRoutes(entry);
        entries.push(`{ id: ${JSON.stringify(module.id)}, module: ${value} }`);
      }
      return [...imports, `export default [${entries.join(", ")}];`].join("\n");
    },
  };
}

/// The version shown in the UI. Release builds export
/// LIGHTSPEED_RELEASE_VERSION (scripts/release/build-dist.sh); other builds
/// fall back to the product version in release/metadata.env.
function releaseVersion(): string {
  const explicit = process.env.LIGHTSPEED_RELEASE_VERSION?.trim();
  if (explicit) return explicit;
  try {
    const metadata = readFileSync(path.resolve(import.meta.dirname, "../../release/metadata.env"), "utf8");
    return metadata.match(/^LIGHTSPEED_PRODUCT_VERSION=(.+)$/m)?.[1]?.trim() ?? "0.0.0";
  } catch {
    return "0.0.0";
  }
}

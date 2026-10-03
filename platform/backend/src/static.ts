import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { Context, Hono } from "hono";

const distDir = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "web",
  "dist",
);

const contentTypes: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".map": "application/json",
  ".woff2": "font/woff2",
  ".txt": "text/plain; charset=utf-8",
};

/// Serves the built SPA under /app with an index.html fallback for client
/// routes. Hand-rolled (no cwd-relative serve-static): paths resolve from
/// this module's location, so it works from npm scripts, tsx, and the image.
export function registerWebApp(app: Hono): void {
  app.get("/app/*", serveSpa);
  app.get("/app", serveSpa);
}

async function serveSpa(c: Context): Promise<Response> {
  const requested = c.req.path.replace(/^\/app\/?/, "");
  const resolved = path.normalize(path.join(distDir, requested));
  if (resolved.startsWith(distDir + path.sep) && requested !== "") {
    try {
      const info = await stat(resolved);
      if (info.isFile()) {
        const ext = path.extname(resolved);
        return c.body(new Uint8Array(await readFile(resolved)), 200, {
          "content-type": contentTypes[ext] ?? "application/octet-stream",
          // Vite asset names are content-hashed; everything else no-cache.
          "cache-control": requested.startsWith("assets/")
            ? "public, max-age=31536000, immutable"
            : "no-cache",
        });
      }
    } catch {
      // fall through to the SPA shell
    }
  }
  try {
    const index = await readFile(path.join(distDir, "index.html"));
    return c.body(new Uint8Array(index), 200, {
      "content-type": "text/html; charset=utf-8",
      "cache-control": "no-cache",
    });
  } catch {
    return c.html(
      `<!doctype html><meta charset="utf-8"><title>Lightspeed</title>
<body style="font-family:system-ui;display:grid;place-items:center;min-height:90vh">
<div><h1>Lightspeed</h1><p>Frontend not built — run <code>npm run build:web</code>.</p></div></body>`,
    );
  }
}

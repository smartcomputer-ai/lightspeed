import { timingSafeEqual } from "node:crypto";
import { readFile } from "node:fs/promises";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { stripTypeScriptTypes } from "node:module";
import { normalize, relative } from "node:path";
import { Readable } from "node:stream";
import { fileURLToPath } from "node:url";

import { HTTP_OPERATIONS } from "@lightspeed-ai/sdk/server";

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`Missing ${name}`);
  return value;
}

const password = Buffer.from(required("INSPECTOR_PASSWORD"));
const port = Number(process.env.PORT ?? 18090);

const endpoint = required("LIGHTSPEED_API_URL").replace(/\/$/, "");
const runtimeToken = required("LIGHTSPEED_RUNTIME_TOKEN");
const universe = required("LIGHTSPEED_UNIVERSE");

/** Stands in for your application's login. Any username, one shared password. */
function signedIn(authorization: string | null | undefined): boolean {
  const [scheme, encoded] = (authorization ?? "").split(" ");
  if (scheme !== "Basic" || !encoded) return false;
  const supplied = Buffer.from(Buffer.from(encoded, "base64").toString().split(":").slice(1).join(":"));
  return supplied.length === password.length && timingSafeEqual(supplied, password);
}

const allowedOperations = [
  "session/list", "session/read", "session/events/read", "session/runs/list", "session/runs/read",
  "session/runs/cancel", "session/runs/approvals/decide", "blobs/read",
] as const;

async function forward(request: Request): Promise<Response> {
  const url = new URL(request.url);
  const allowed = allowedOperations.some((id) => {
    const binding = HTTP_OPERATIONS[id];
    const route = binding.path.replace("{universe}", encodeURIComponent(universe));
    return request.method === binding.method && new RegExp("^" + route.replace(/\{[^}]+\}/g, "[^/]+") + "$").test(url.pathname);
  });
  if (!allowed) return new Response("not found", { status: 404 });
  const headers = new Headers();
  headers.set("authorization", `Bearer ${runtimeToken}`);
  for (const name of ["content-type", "accept", "range"]) {
    const value = request.headers.get(name);
    if (value) headers.set(name, value);
  }
  const init: RequestInit & { duplex?: "half" } = { method: request.method, headers, signal: request.signal };
  if (request.body) { init.body = request.body; init.duplex = "half"; }
  return fetch(endpoint + url.pathname.slice("/api/v1".length) + url.search, init);
}

const here = new URL("./", import.meta.url);
const sdkDist = fileURLToPath(new URL("../../dist/", import.meta.url));

/** Serves the SDK's own modules so the page needs no bundler. */
async function sdkModule(path: string): Promise<Response> {
  const file = normalize(sdkDist + path);
  if (!file.endsWith(".js") || relative(sdkDist, file).startsWith("..")) {
    return new Response("not found", { status: 404 });
  }
  return new Response(await readFile(file), { headers: { "content-type": "text/javascript" } });
}

async function route(request: Request): Promise<Response> {
  const { pathname } = new URL(request.url);
  if (!signedIn(request.headers.get("authorization"))) {
    return new Response("sign in required", {
      status: 401,
      headers: { "www-authenticate": 'Basic realm="Run inspector"' },
    });
  }
  if (pathname.startsWith("/api/v1/")) return forward(request);
  if (pathname === "/connection") return Response.json({ universe });
  if (pathname === "/") {
    return new Response(await readFile(new URL("index.html", here)), {
      headers: { "content-type": "text/html; charset=utf-8" },
    });
  }
  if (pathname === "/app.js") {
    const source = await readFile(new URL("app.ts", here), "utf8");
    return new Response(stripTypeScriptTypes(source), { headers: { "content-type": "text/javascript" } });
  }
  if (pathname.startsWith("/sdk/")) return sdkModule(pathname.slice("/sdk/".length));
  return new Response("not found", { status: 404 });
}

/** Adapts Node's server to the standard Request and Response the proxy uses. */
async function handle(incoming: IncomingMessage, outgoing: ServerResponse): Promise<void> {
  const closed = new AbortController();
  outgoing.on("close", () => closed.abort());
  const init: RequestInit = {
    method: incoming.method ?? "GET",
    headers: incoming.headers as Record<string, string>,
    signal: closed.signal,
  };
  if (incoming.method !== "GET" && incoming.method !== "HEAD") {
    init.body = Readable.toWeb(incoming) as ReadableStream;
    (init as { duplex?: string }).duplex = "half";
  }
  const response = await route(new Request(`http://${incoming.headers.host}${incoming.url}`, init));
  outgoing.writeHead(response.status, Object.fromEntries(response.headers));
  if (!response.body) { outgoing.end(); return; }
  Readable.fromWeb(response.body as import("node:stream/web").ReadableStream).pipe(outgoing);
}

createServer((incoming, outgoing) => {
  handle(incoming, outgoing).catch((error: unknown) => {
    console.error(error);
    if (!outgoing.headersSent) outgoing.writeHead(500);
    outgoing.end();
  });
}).listen(port, "127.0.0.1", () => {
  console.log(`Run inspector on http://127.0.0.1:${port}`);
});

import { timingSafeEqual } from "node:crypto";
import { readFile } from "node:fs/promises";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { stripTypeScriptTypes } from "node:module";
import { normalize, relative } from "node:path";
import { Readable } from "node:stream";
import { fileURLToPath } from "node:url";
import { LightspeedClient, type LightspeedClientOptions } from "@lightspeed-ai/sdk";
import { createRpcProxy } from "@lightspeed-ai/sdk/server";

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`Missing ${name}`);
  return value;
}

const password = Buffer.from(required("INSPECTOR_PASSWORD"));
const port = Number(process.env.PORT ?? 18090);

const options: LightspeedClientOptions = {
  endpoint: required("LIGHTSPEED_API_URL"),
  apiKey: required("LIGHTSPEED_API_KEY"),
};
if (process.env.LIGHTSPEED_UNIVERSE) options.universe = process.env.LIGHTSPEED_UNIVERSE;

/** Stands in for your application's login. Any username, one shared password. */
function signedIn(authorization: string | null | undefined): boolean {
  const [scheme, encoded] = (authorization ?? "").split(" ");
  if (scheme !== "Basic" || !encoded) return false;
  const supplied = Buffer.from(Buffer.from(encoded, "base64").toString().split(":").slice(1).join(":"));
  return supplied.length === password.length && timingSafeEqual(supplied, password);
}

const rpc = createRpcProxy({
  client: new LightspeedClient(options),
  methods: [
    "session/list",
    "session/read",
    "session/events/read",
    "session/runs/list",
    "session/runs/read",
    "session/runs/cancel",
    "session/runs/approvals/decide",
    "blobs/read",
  ],
  authorize: ({ request }) =>
    signedIn(request.headers.get("authorization")) ? {} : { deny: "unauthenticated" },
});

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
  if (pathname === "/rpc") return rpc(request);
  if (!signedIn(request.headers.get("authorization"))) {
    return new Response("sign in required", {
      status: 401,
      headers: { "www-authenticate": 'Basic realm="Run inspector"' },
    });
  }
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
  if (incoming.method === "POST") {
    init.body = Readable.toWeb(incoming) as ReadableStream;
    (init as { duplex?: string }).duplex = "half";
  }
  const response = await route(new Request(`http://${incoming.headers.host}${incoming.url}`, init));
  outgoing.writeHead(response.status, Object.fromEntries(response.headers));
  outgoing.end(Buffer.from(await response.arrayBuffer()));
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

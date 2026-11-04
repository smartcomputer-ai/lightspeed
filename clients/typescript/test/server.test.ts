import { describe, expect, it, vi } from "vitest";
import { LightspeedClient, LightspeedRpcError, type Method } from "../src/index.js";
import { createRpcProxy, type RpcProxyOptions } from "../src/server.js";

interface Forwarded {
  method: string;
  params: unknown;
  headers: Headers;
}

/** A gateway behind the proxy, recording what actually reached it. */
function upstream(respond: (rpc: { id: number; method: string }) => Response | Promise<Response> = ok) {
  const forwarded: Forwarded[] = [];
  const fetchImpl = (async (_input: RequestInfo | URL, init?: RequestInit) => {
    const rpc = JSON.parse(init?.body as string) as { id: number; method: string; params: unknown };
    forwarded.push({ method: rpc.method, params: rpc.params, headers: new Headers(init?.headers) });
    return respond(rpc);
  }) as typeof fetch;
  const client = new LightspeedClient({
    endpoint: "http://internal.example/rpc",
    apiKey: "lsk_secret",
    fetch: fetchImpl,
  });
  return { client, forwarded };
}

function ok(rpc: { id: number }): Response {
  return Response.json({ id: rpc.id, result: { result: { fine: true }, notifications: [] } });
}

const post = (body: unknown, headers: Record<string, string> = {}) =>
  new Request("http://app.example/api/lightspeed", {
    method: "POST",
    headers: { "content-type": "application/json", ...headers },
    body: JSON.stringify(body),
  });

const methods: readonly Method[] = ["session/read", "session/list", "blobs/read"];
const allow: RpcProxyOptions["authorize"] = () => ({});

describe("createRpcProxy", () => {
  it("forwards an allowed call and returns the gateway's result", async () => {
    const { client, forwarded } = upstream();
    const handler = createRpcProxy({ client, methods, authorize: allow });

    const response = await handler(post({ id: 7, method: "session/read", params: { sessionId: "s" } }));

    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.json()).toEqual({ id: 7, result: { result: { fine: true }, notifications: [] } });
    expect(forwarded).toMatchObject([{ method: "session/read", params: { sessionId: "s" } }]);
    expect(forwarded[0]?.headers.get("authorization")).toBe("Bearer lsk_secret");
  });

  it("refuses a method not on the list without consulting authorize or the gateway", async () => {
    const { client, forwarded } = upstream();
    const authorize = vi.fn(allow);
    const handler = createRpcProxy({ client, methods, authorize });

    for (const method of ["session/delete", "deployment/api-keys/create", "__proto__", "constructor", 42]) {
      const body = (await (await handler(post({ id: 1, method, params: {} }))).json()) as {
        error: { code: number; data: { kind: string } };
      };
      expect(body.error).toMatchObject({ code: -32003, data: { kind: "forbidden" } });
    }
    expect(authorize).not.toHaveBeenCalled();
    expect(forwarded).toEqual([]);
  });

  it("allows nothing when no methods are listed", async () => {
    const { client, forwarded } = upstream();
    const handler = createRpcProxy({ client, methods: [], authorize: allow });
    await handler(post({ id: 1, method: "session/read", params: { sessionId: "s" } }));
    expect(forwarded).toEqual([]);
  });

  it("gives a denial as an error the browser's client understands", async () => {
    const { client, forwarded } = upstream();
    const handler = createRpcProxy({
      client,
      methods,
      authorize: ({ params }) =>
        (params as { sessionId: string }).sessionId === "mine" ? {} : { deny: "forbidden", message: "not yours" },
    });
    const browser = new LightspeedClient({
      endpoint: "http://app.example/api/lightspeed",
      fetch: (async (input: RequestInfo | URL, init?: RequestInit) =>
        handler(new Request(input, init))) as typeof fetch,
    });

    await expect(browser.call("session/read", { sessionId: "theirs" })).rejects.toMatchObject({
      name: "LightspeedRpcError",
      kind: "forbidden",
      message: "not yours",
    });
    expect(forwarded).toEqual([]);
    await expect(browser.call("session/read", { sessionId: "mine" })).resolves.toMatchObject({ result: { fine: true } });
  });

  it("reports an unauthenticated caller by kind", async () => {
    const { client } = upstream();
    const handler = createRpcProxy({ client, methods, authorize: () => ({ deny: "unauthenticated" }) });
    const body = (await (await handler(post({ id: 1, method: "session/read" }))).json()) as {
      error: { code: number; data: { kind: string } };
    };
    expect(new LightspeedRpcError(body.error as never).kind).toBe("unauthenticated");
  });

  it("passes the caller, method and raw params to authorize", async () => {
    const { client } = upstream();
    const authorize = vi.fn(allow);
    const handler = createRpcProxy({ client, methods, authorize });
    const request = post({ id: 1, method: "session/read", params: { sessionId: 12 } });

    await handler(request);

    expect(authorize).toHaveBeenCalledWith({ request, method: "session/read", params: { sessionId: 12 } });
  });

  it("does not forward anything identifying from the browser", async () => {
    const { client, forwarded } = upstream();
    const handler = createRpcProxy({ client, methods, authorize: allow });

    await handler(
      post(
        { id: 1, method: "session/read", params: { sessionId: "s" } },
        {
          authorization: "Bearer lsk_stolen",
          "x-lightspeed-universe": "other-universe",
          "x-lightspeed-actor": "someone-else",
          cookie: "session=abc",
        },
      ),
    );

    const headers = forwarded[0]?.headers;
    expect(headers?.get("authorization")).toBe("Bearer lsk_secret");
    expect(headers?.has("x-lightspeed-universe")).toBe(false);
    expect(headers?.has("x-lightspeed-actor")).toBe(false);
    expect(headers?.has("cookie")).toBe(false);
  });

  it("sends the actor authorize chose, per request", async () => {
    const { client, forwarded } = upstream();
    const handler = createRpcProxy({
      client,
      methods,
      authorize: ({ request }) => ({ actor: request.headers.get("x-test-user") ?? "nobody" }),
    });

    await handler(post({ id: 1, method: "session/read" }, { "x-test-user": "alice" }));
    await handler(post({ id: 2, method: "session/read" }, { "x-test-user": "bob" }));

    expect(forwarded.map((call) => call.headers.get("x-lightspeed-actor"))).toEqual(["alice", "bob"]);
  });

  it("forwards the params authorize returned instead of the browser's", async () => {
    const { client, forwarded } = upstream();
    const handler = createRpcProxy({
      client,
      methods,
      authorize: ({ params }) => ({
        params: { ...(params as object), metadata: { owner: "alice" } },
      }),
    });

    await handler(post({ id: 1, method: "session/list", params: { limit: 5, metadata: { owner: "bob" } } }));

    expect(forwarded[0]?.params).toEqual({ limit: 5, metadata: { owner: "alice" } });
  });

  it("forwards nothing when authorize throws", async () => {
    const { client, forwarded } = upstream();
    const handler = createRpcProxy({
      client,
      methods,
      authorize: () => {
        throw new Error("login service down");
      },
    });

    await expect(handler(post({ id: 1, method: "session/read" }))).rejects.toThrow("login service down");
    expect(forwarded).toEqual([]);
  });

  it("passes the gateway's own errors through", async () => {
    const { client } = upstream((rpc) =>
      Response.json({ id: rpc.id, error: { code: -32004, message: "gone", data: { kind: "notFound", message: "gone" } } }),
    );
    const handler = createRpcProxy({ client, methods, authorize: allow });

    expect(await (await handler(post({ id: 9, method: "session/read" }))).json()).toEqual({
      id: 9,
      error: { code: -32004, message: "gone", data: { kind: "notFound", message: "gone" } },
    });
  });

  it("does not reveal the gateway's address when it is unreachable", async () => {
    const { client } = upstream(() => new Response("", { status: 404 }));
    const handler = createRpcProxy({ client, methods, authorize: allow });

    const response = await handler(post({ id: 1, method: "session/read" }));

    expect(response.status).toBe(502);
    expect(await response.text()).not.toContain("internal.example");
  });

  it("stops waiting on the gateway when the browser goes away", async () => {
    let seen: AbortSignal | undefined;
    const fetchImpl = ((_input: RequestInfo | URL, init?: RequestInit) => {
      seen = init?.signal ?? undefined;
      return new Promise((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => reject(init.signal?.reason));
      });
    }) as typeof fetch;
    const client = new LightspeedClient({ endpoint: "http://internal.example/rpc", fetch: fetchImpl });
    const handler = createRpcProxy({ client, methods, authorize: allow });
    const browser = new AbortController();
    const request = new Request("http://app.example/api/lightspeed", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ id: 1, method: "session/read" }),
      signal: browser.signal,
    });

    const pending = handler(request);
    await vi.waitFor(() => expect(seen).toBeDefined());
    browser.abort();

    expect((await pending).status).toBe(499);
  });

  describe("refuses requests that could come from elsewhere or are malformed", () => {
    const { client, forwarded } = upstream();
    const handler = createRpcProxy({ client, methods, authorize: allow, maxBodyBytes: 200 });
    const raw = (init: RequestInit) => handler(new Request("http://app.example/rpc", init));
    const body = JSON.stringify({ id: 1, method: "session/read" });

    it.each([
      ["a GET", { method: "GET" }, 405],
      ["a form post", { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body }, 415],
      ["a plain text post", { method: "POST", headers: { "content-type": "text/plain" }, body }, 415],
      ["no content type", { method: "POST", body: new TextEncoder().encode(body) }, 415],
      ["a body over the limit", { method: "POST", headers: { "content-type": "application/json" }, body: "x".repeat(201) }, 413],
    ] as Array<[string, RequestInit, number]>)("%s", async (_name, init, status) => {
      expect((await raw(init)).status).toBe(status);
      expect(forwarded).toEqual([]);
    });

    it("an oversized body with no declared length", async () => {
      const stream = new ReadableStream({
        start(controller) {
          for (let chunk = 0; chunk < 5; chunk += 1) {
            controller.enqueue(new Uint8Array(100));
          }
          controller.close();
        },
      });
      const response = await raw({
        method: "POST",
        headers: { "content-type": "application/json" },
        body: stream,
        duplex: "half",
      } as RequestInit);
      expect(response.status).toBe(413);
    });

    it.each([
      ["broken JSON", "{nope"],
      ["no method", JSON.stringify({ id: 1 })],
      ["not an object", "[]"],
      ["null", "null"],
    ])("%s", async (_name, text) => {
      const response = await raw({ method: "POST", headers: { "content-type": "application/json; charset=utf-8" }, body: text });
      expect(response.status).toBe(200);
      expect(await response.json()).toMatchObject({ error: { data: { kind: expect.stringMatching(/invalidRequest|forbidden/) } } });
      expect(forwarded).toEqual([]);
    });
  });
});

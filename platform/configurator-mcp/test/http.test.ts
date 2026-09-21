import { request as httpRequest } from "node:http";
import { Client, StreamableHTTPClientTransport } from "@modelcontextprotocol/client";
import { afterEach, describe, expect, it } from "vitest";
import type { ConfiguratorConfig } from "../src/config.js";
import { GENERATED_TOOLS } from "../src/generated/tools.js";
import { startConfigurator, type RunningConfigurator } from "../src/transport.js";

interface UpstreamRequest {
  method: string;
  headers: Headers;
  params: unknown;
}

const running: RunningConfigurator[] = [];

afterEach(async () => {
  await Promise.all(running.splice(0).map((server) => server.close()));
});

describe("Streamable HTTP configurator", () => {
  it("initializes, lists all tools, and forwards a universe call in api-key mode", async () => {
    const upstream: UpstreamRequest[] = [];
    const server = await start("authenticated", fakeUpstream(upstream));
    const { client, transport } = mcpClient(server, { authorization: "Bearer lsk_alpha" });

    await client.connect(transport as Parameters<typeof client.connect>[0]);
    expect(client.getProtocolEra()).toBe("modern");
    const listed = await client.listTools();
    expect(listed.tools).toHaveLength(GENERATED_TOOLS.length);
    expect(listed.tools.some((tool) => tool.name.startsWith("lightspeed_deployment_"))).toBe(false);
    expect(listed.tools.find((tool) => tool.name === "lightspeed_session_config_put")?.description)
      .toContain("omitted features are revoked");
    await expect(
      client.callTool({ name: "lightspeed_deployment_universes_list", arguments: {} }),
    ).rejects.toThrow(/unknown tool/);

    const result = await client.callTool({
      name: "lightspeed_models_list",
      arguments: { selectableOnly: true },
    });
    // One representation only: the JSON text block, no structuredContent.
    expect(result.structuredContent).toBeUndefined();
    const content = result.content as Array<{ type: string; text: string }>;
    expect(content).toHaveLength(1);
    expect(JSON.parse(content[0]?.text ?? "")).toEqual({
      result: { models: [], providers: [] },
      notifications: [],
    });
    expect(upstream.filter((request) => request.method === "models/list")).toHaveLength(1);
    expect(upstream.every((request) => request.headers.get("authorization") === "Bearer lsk_alpha"))
      .toBe(true);
    expect(upstream.find((request) => request.method === "models/list")?.params).toEqual({
      selectableOnly: true,
    });
    await client.close();
  });

  it("keeps concurrent authenticated universes isolated", async () => {
    const upstream: UpstreamRequest[] = [];
    const server = await start("authenticated", fakeUpstream(upstream, 5));
    const a = mcpClient(server, {
      "x-lightspeed-universe": universeA,
      authorization: "Bearer lsk_service",
      "x-lightspeed-principal": "user:00000000-0000-4000-8000-000000000003",
    });
    const b = mcpClient(server, {
      "x-lightspeed-universe": universeB,
      authorization: "Bearer lsk_service",
      "x-lightspeed-principal": "user:00000000-0000-4000-8000-000000000004",
    });

    await Promise.all([
      a.client.connect(a.transport as Parameters<typeof a.client.connect>[0]),
      b.client.connect(b.transport as Parameters<typeof b.client.connect>[0]),
    ]);
    await Promise.all([
      a.client.callTool({ name: "lightspeed_session_list", arguments: {} }),
      b.client.callTool({ name: "lightspeed_session_list", arguments: {} }),
    ]);

    const calls = upstream.filter((request) => request.method === "session/list");
    expect(calls).toHaveLength(2);
    expect(calls.map((call) => call.headers.get("x-lightspeed-universe")).sort()).toEqual(
      [universeA, universeB].sort(),
    );
    const callA = calls.find(
      (call) => call.headers.get("x-lightspeed-universe") === universeA,
    );
    const callB = calls.find(
      (call) => call.headers.get("x-lightspeed-universe") === universeB,
    );
    expect(callA?.headers.get("x-lightspeed-principal")).toBe("user:00000000-0000-4000-8000-000000000003");
    expect(callB?.headers.get("x-lightspeed-principal")).toBe("user:00000000-0000-4000-8000-000000000004");
    await Promise.all([a.client.close(), b.client.close()]);
  });

  it("authenticates protocol-only requests upstream and rejects invalid credentials", async () => {
    const server = await start("authenticated", async (_input, init) => {
      const body = JSON.parse(String(init?.body)) as { id: number | string };
      return jsonResponse({
        id: body.id,
        error: {
          code: -32010,
          message: "invalid api key",
          data: { kind: "rejected", message: "invalid api key" },
        },
      });
    });
    const response = await fetch(`${server.url}/mcp`, {
      method: "POST",
      headers: mcpHeaders({ authorization: "Bearer lsk_revoked" }),
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "initialize",
        params: {
          protocolVersion: "2025-06-18",
          capabilities: {},
          clientInfo: { name: "test", version: "1" },
        },
      }),
    });
    expect(response.status).toBe(401);
    expect(await response.text()).not.toContain("lsk_revoked");
  });

  it("rejects disallowed origins before contacting Lightspeed", async () => {
    const upstream: UpstreamRequest[] = [];
    const server = await start("single", fakeUpstream(upstream), ["https://allowed.example"]);
    const response = await fetch(`${server.url}/mcp`, {
      method: "POST",
      headers: mcpHeaders({ origin: "https://evil.example" }),
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping" }),
    });
    expect(response.status).toBe(403);
    expect(upstream).toHaveLength(0);
  });

  it("rejects disallowed hosts and oversized request bodies before upstream dispatch", async () => {
    const upstream: UpstreamRequest[] = [];
    const server = await start("single", fakeUpstream(upstream), [], 256);
    const badHost = await rawPost(
      `${server.url}/mcp`,
      { ...mcpHeaders({}), host: "evil.example" },
      JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping" }),
    );
    expect(badHost.status).toBe(403);

    const oversized = await fetch(`${server.url}/mcp`, {
      method: "POST",
      headers: mcpHeaders({}),
      body: JSON.stringify({ jsonrpc: "2.0", id: 2, method: "ping", padding: "x".repeat(512) }),
    });
    expect(oversized.status).toBe(413);
    expect(upstream).toHaveLength(0);
  });

  it("aborts an in-flight upstream request when the MCP client disconnects", async () => {
    let observedAbort!: () => void;
    const aborted = new Promise<void>((resolve) => {
      observedAbort = resolve;
    });
    const server = await start("single", async (_input, init) => {
      return await new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener(
          "abort",
          () => {
            observedAbort();
            reject(init.signal?.reason ?? new Error("aborted"));
          },
          { once: true },
        );
      });
    });
    const controller = new AbortController();
    const request = fetch(`${server.url}/mcp`, {
      method: "POST",
      headers: mcpHeaders({}),
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping" }),
      signal: controller.signal,
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    controller.abort();
    await expect(request).rejects.toThrow();
    await expect(aborted).resolves.toBeUndefined();
  });

  it("rejects oversized upstream responses without returning their body", async () => {
    const server = await start(
      "single",
      async (_input, init) => {
        const request = JSON.parse(String(init?.body)) as { id: number | string };
        return jsonResponse({
          id: request.id,
          result: {
            result: { padding: "sensitive-response".repeat(200) },
            notifications: [],
          },
        });
      },
      [],
      1024,
    );
    const response = await fetch(`${server.url}/mcp`, {
      method: "POST",
      headers: mcpHeaders({}),
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping" }),
    });
    expect(response.status).toBe(502);
    expect(await response.text()).not.toContain("sensitive-response");
  });
});

async function start(
  authMode: ConfiguratorConfig["authMode"],
  fetchImpl: typeof fetch,
  allowedOrigins: string[] = [],
  maxBodyBytes = 1024 * 1024,
): Promise<RunningConfigurator> {
  const config: ConfiguratorConfig = {
    bindHost: "127.0.0.1",
    bindPort: 0,
    allowedHosts: ["127.0.0.1", "localhost"],
    allowedOrigins,
    rpcEndpoint: "http://lightspeed.test/rpc",
    authMode,
    maxBodyBytes,
    upstreamTimeoutMs: 5_000,
    shutdownTimeoutMs: 1_000,
  };
  const server = await startConfigurator({ config, fetch: fetchImpl });
  running.push(server);
  return server;
}

function mcpClient(server: RunningConfigurator, headers: Record<string, string>) {
  const client = new Client(
    { name: "configurator-test", version: "1" },
    { versionNegotiation: { mode: "auto" } },
  );
  const transport = new StreamableHTTPClientTransport(new URL(`${server.url}/mcp`), {
    requestInit: { headers },
  });
  return { client, transport };
}

function fakeUpstream(requests: UpstreamRequest[], delayMs = 0): typeof fetch {
  return async (_input, init) => {
    const body = JSON.parse(String(init?.body)) as {
      id: number | string;
      method: string;
      params: unknown;
    };
    if (delayMs > 0) {
      await new Promise((resolve) => setTimeout(resolve, delayMs));
    }
    requests.push({ method: body.method, params: body.params, headers: new Headers(init?.headers) });
    const result =
      body.method === "initialize"
        ? {
            protocolVersion: "1",
            serverInfo: { name: "lightspeed-test", version: "1" },
            capabilities: {
              notifications: false,
              historyRead: true,
              eventLog: true,
              localExecution: false,
            },
          }
        : body.method === "models/list"
          ? { models: [], providers: [] }
          : body.method === "session/list"
            ? { sessions: [] }
            : {};
    return jsonResponse({ id: body.id, result: { result, notifications: [] } });
  };
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function mcpHeaders(extra: Record<string, string>): Record<string, string> {
  return {
    "content-type": "application/json",
    accept: "application/json, text/event-stream",
    ...extra,
  };
}

function rawPost(
  url: string,
  headers: Record<string, string>,
  body: string,
): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const request = httpRequest(url, { method: "POST", headers }, (response) => {
      const chunks: Buffer[] = [];
      response.on("data", (chunk: Buffer) => chunks.push(chunk));
      response.on("end", () => {
        resolve({
          status: response.statusCode ?? 0,
          body: Buffer.concat(chunks).toString("utf8"),
        });
      });
    });
    request.once("error", reject);
    request.end(body);
  });
}

const universeA = "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f";
const universeB = "b87266e0-e98f-45c8-b8b6-bc8bfdd80132";

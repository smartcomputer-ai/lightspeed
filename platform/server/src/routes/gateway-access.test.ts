import { Hono } from "hono";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { EffectiveAccess } from "@lightspeed-ai/agent-client";
import type { ApiVariables, AppContext } from "../context.js";
import { requestIdentity } from "../runtime-client.js";
import { gatewayRoutes } from "./gateway.js";

vi.mock("./universes.js", () => ({
  universeForSession: vi.fn(async () => ({
    universe: {
      lightspeedUniverseId: "universe",
      gatewayUrl: "https://engine.example/rpc",
    },
    slug: "test",
    role: "viewer",
  })),
}));
afterEach(() => vi.unstubAllGlobals());
function app() {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (_c, next) => {
    await requestIdentity.run(
      {
        principal: { id: "11111111-1111-4111-8111-111111111111" },
      } as EffectiveAccess,
      next,
    );
  });
  app.route(
    "/",
    gatewayRoutes({
      env: {
        lightspeedApiUrl: "https://engine.example/rpc",
        lightspeedApiKey: "lsk_fixture",
      },
    } as AppContext),
  );
  return app;
}
describe("action permission previews", () => {
  it("forwards as the logged-in actor and preserves target-specific core decisions", async () => {
    const preview = {
      actions: ["read"],
      resources: [
        { resource: { kind: "session", id: "other" }, actions: ["read"] },
      ],
    };
    const fetch = vi.fn(async (_url: unknown, init: RequestInit) => {
      expect(new Headers(init.headers).get("x-lightspeed-principal")).toBe(
        "user:11111111-1111-4111-8111-111111111111",
      );
      const rpc = JSON.parse(String(init.body));
      expect(rpc.method).toBe("access/read");
      expect(rpc.params).toEqual({
        resources: [{ kind: "session", id: "other" }],
        sessionDeleteCascade: true,
      });
      return Response.json({
        id: rpc.id,
        result: { result: preview, notifications: [] },
      });
    });
    vi.stubGlobal("fetch", fetch);
    const response = await app().request("/universe/access", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        resources: [{ kind: "session", id: "other" }],
        sessionDeleteCascade: true,
      }),
    });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(preview);
  });
  it("decides resources for the default agent identity when asked", async () => {
    const fetch = vi.fn(async (_url: unknown, init: RequestInit) => {
      const rpc = JSON.parse(String(init.body));
      expect(rpc.params).toEqual({
        resources: [{ kind: "environment", id: "production" }],
        as: "execution_service",
      });
      return Response.json({
        id: rpc.id,
        result: { result: { actions: [], resources: [] }, notifications: [] },
      });
    });
    vi.stubGlobal("fetch", fetch);
    const response = await app().request("/universe/access", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        resources: [{ kind: "environment", id: "production" }],
        as: "execution_service",
      }),
    });
    expect(response.status).toBe(200);
    expect(fetch).toHaveBeenCalledTimes(1);
  });
  it.each([
    { principalId: "another-user" },
    { as: "someone-else" },
    { resources: [{ kind: "session", id: "" }] },
    {
      resources: Array.from({ length: 101 }, () => ({
        kind: "session",
        id: "one",
      })),
    },
  ])(
    "rejects actor overrides and malformed or unbounded targets",
    async (body) => {
      const fetch = vi.fn();
      vi.stubGlobal("fetch", fetch);
      const response = await app().request("/universe/access", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      });
      expect(response.status).toBe(400);
      expect(fetch).not.toHaveBeenCalled();
    },
  );
});

it("forwards the named content resource without changing the actor", async () => {
  const fetch = vi.fn(async (_url: unknown, init: RequestInit) => {
    expect(new Headers(init.headers).get("x-lightspeed-principal")).toBe(
      "user:11111111-1111-4111-8111-111111111111",
    );
    const rpc = JSON.parse(String(init.body));
    expect(rpc.method).toBe("blobs/read");
    expect(rpc.params).toEqual({
      blobRef: "sha256:test",
      resource: { kind: "session", id: "shared" },
    });
    return Response.json({
      id: rpc.id,
      result: { result: { bytesBase64: "YQ==" }, notifications: [] },
    });
  });
  vi.stubGlobal("fetch", fetch);
  const response = await app().request(
    "/universe/blobs/sha256:test?resourceKind=session&resourceId=shared",
  );
  expect(response.status).toBe(200);
  expect(fetch).toHaveBeenCalledTimes(1);
});

it.each([
  "resourceKind=session",
  "resourceId=shared",
  "resourceKind=workspace&resourceId=shared",
])("refuses incomplete or unsupported content context: %s", async (query) => {
  const fetch = vi.fn();
  vi.stubGlobal("fetch", fetch);
  expect((await app().request(`/universe/blobs/ref?${query}`)).status).toBe(
    400,
  );
  expect(fetch).not.toHaveBeenCalled();
});

it("reads workspace content by its path rather than an unscoped digest", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_url: unknown, init: RequestInit) => {
      const rpc = JSON.parse(String(init.body));
      expect(rpc.method).toBe("vfs/workspaces/files/read");
      expect(rpc.params).toEqual({
        workspaceId: "docs",
        path: "notes/hello world.txt",
      });
      return Response.json({
        id: rpc.id,
        result: { result: { bytesBase64: "YQ==" }, notifications: [] },
      });
    }),
  );
  expect(
    (
      await app().request(
        "/universe/workspaces/docs/files/notes/hello%20world.txt",
      )
    ).status,
  ).toBe(200);
});

it("preserves the workspace source and revision when editing a shared file", async () => {
  const calls: { method: string; params: Record<string, unknown> }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_url: unknown, init: RequestInit) => {
      const rpc = JSON.parse(String(init.body));
      calls.push(rpc);
      const responses: Record<string, unknown> = {
        "vfs/workspaces/read": {
          workspace: { revision: 4, headSnapshotRef: "sha256:head" },
        },
        "vfs/snapshots/read": {
          manifest: {
            schema_version: "lightspeed.vfs.snapshot.v1",
            root: {
              entries: {
                "old.txt": {
                  kind: "file",
                  blob_ref: "sha256:old",
                  size_bytes: 3,
                  executable: false,
                },
              },
            },
            totals: { files: 1, bytes: 3 },
          },
        },
        "blobs/put": { blobs: [{ blobRef: "sha256:new", bytes: 3 }] },
        "vfs/snapshots/commit": { snapshotRef: "sha256:next" },
        "vfs/workspaces/update": {
          workspace: { revision: 5, headSnapshotRef: "sha256:next" },
        },
      };
      return Response.json({
        id: rpc.id,
        result: { result: responses[rpc.method], notifications: [] },
      });
    }),
  );
  const response = await app().request(
    "/universe/workspaces/shared/files/new.txt",
    {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ contentText: "new", expectedRevision: 4 }),
    },
  );
  expect(response.status).toBe(200);
  expect(
    calls.find((call) => call.method === "vfs/snapshots/commit")?.params,
  ).toMatchObject({
    sourceWorkspaceId: "shared",
    manifest: {
      root: {
        entries: {
          "old.txt": { blob_ref: "sha256:old" },
          "new.txt": { blob_ref: "sha256:new" },
        },
      },
    },
  });
  expect(calls.at(-1)?.params).toEqual({
    workspaceId: "shared",
    snapshotRef: "sha256:next",
    expectedRevision: 4,
  });
});

it("forwards privileged-read provenance and isolates concurrent ordinary requests", async () => {
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    const privileged = rpc.params.blobRef === "private";
    // Interleave responses to catch accidental process-wide provenance state.
    await new Promise((resolve) => setTimeout(resolve, privileged ? 10 : 20));
    return Response.json({ id: rpc.id, result: { result: { bytesBase64: "YQ==" } } }, {
      headers: privileged ? { "x-lightspeed-privileged-read": "true" } : {},
    });
  }));
  const [privateRead, ordinaryRead] = await Promise.all([
    app().request("/universe/blobs/private?resourceKind=session&resourceId=restricted"),
    app().request("/universe/blobs/ordinary?resourceKind=session&resourceId=shared"),
  ]);
  expect(privateRead.headers.get("x-lightspeed-privileged-read")).toBe("true");
  expect(ordinaryRead.headers.has("x-lightspeed-privileged-read")).toBe(false);
});

describe("operational resource creation", () => {
  const restricted = { visibility: "restricted" };
  function capture(result: unknown) {
    const calls: { method: string; params: Record<string, unknown> }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: unknown, init: RequestInit) => {
        const rpc = JSON.parse(String(init.body));
        calls.push(rpc);
        return Response.json({
          id: rpc.id,
          result: { result, notifications: [] },
        });
      }),
    );
    return calls;
  }
  async function post(path: string, body: unknown, method = "POST") {
    return app().request(path, {
      method,
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  }
  it("forwards the audience of a new workspace", async () => {
    const calls = capture({ workspace: { workspaceId: "notes" } });
    const response = await post("/universe/workspaces", {
      workspaceId: "notes",
      access: restricted,
    });
    expect(response.status).toBe(201);
    expect(calls[0]).toMatchObject({
      method: "vfs/workspaces/create",
      params: { workspaceId: "notes", access: restricted },
    });
  });
  it("forwards the audience of provisioned and external environments", async () => {
    const calls = capture({ environment: { environmentId: "env" } });
    expect(
      (
        await post("/universe/environments", {
          requestId: "req",
          bindingId: "incus",
          templateId: "ubuntu",
          access: restricted,
        })
      ).status,
    ).toBe(201);
    expect(
      (
        await post("/universe/environments/external", {
          endpoint: "wss://envd.example.com/ws",
          access: restricted,
        })
      ).status,
    ).toBe(201);
    expect(calls.map((call) => [call.method, call.params.access])).toEqual([
      ["environments/create", restricted],
      ["environments/external/create", restricted],
    ]);
  });
  it("passes an MCP server's audience beside the record, never inside it", async () => {
    const calls = capture({ server: { serverId: "github" } });
    const document = {
      serverId: "github",
      serverUrl: "https://mcp.example.com/mcp",
      defaultServerLabel: "github",
    };
    expect(
      (await post("/universe/mcp-servers", { ...document, access: restricted }))
        .status,
    ).toBe(201);
    // A replaced document may echo the view's summary; it is not input.
    const summary = {
      root: { kind: "mcp_server", id: "github" },
      owner: "11111111-1111-4111-8111-111111111111",
      visibility: "universe",
    };
    expect(
      (
        await post(
          "/universe/mcp-servers/github",
          { ...document, revision: 2, access: summary },
          "PUT",
        )
      ).status,
    ).toBe(200);
    expect(calls[0]?.params).toEqual({ server: document, access: restricted });
    expect(calls[1]?.params).toEqual({ server: document, expectedRevision: 2 });
  });
  it("rejects an unknown audience before calling the runtime", async () => {
    const calls = capture({});
    const response = await post("/universe/workspaces", {
      workspaceId: "notes",
      access: { visibility: "private" },
    });
    expect(response.status).toBe(400);
    expect(calls).toEqual([]);
  });
});

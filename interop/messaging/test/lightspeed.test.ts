import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { LightspeedRpcError, type LightspeedClient, type SessionView } from "@lightspeed/agent-client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { SessionRecipe } from "../src/config.js";
import {
  extractAssistantText,
  LightspeedSessionBridge,
  sessionStartConfig,
} from "../src/lightspeed.js";
import { JsonBridgeStore } from "../src/store.js";

function sessionFixture(): SessionView {
  return {
    activeContext: {
      revision: 3,
      items: [
        { id: "u1", type: "userMessage", text: "question" },
        { id: "a1", type: "assistantMessage", text: "old answer" },
        { id: "a2", type: "assistantMessage", text: "fallback answer" },
      ],
    },
    configRevision: 0,
    createdAtMs: 1,
    id: "session_1",
    runs: [
      {
        id: "run_1",
        input: [{ type: "text", text: "question" }],
        items: [
          { id: "u1", type: "userMessage", text: "question" },
          { id: "a1", type: "assistantMessage", text: "old answer" },
        ],
        status: "completed",
      },
      {
        id: "run_2",
        input: [{ type: "text", text: "follow up" }],
        items: [
          { id: "u2", type: "userMessage", text: "follow up" },
          { id: "a3", type: "assistantMessage", text: "first part" },
          { id: "a4", type: "assistantMessage", text: "second part" },
        ],
        status: "completed",
      },
    ],
    status: "idle",
    updatedAtMs: 2,
  };
}

describe("extractAssistantText", () => {
  it("joins every assistant message from the matching run", () => {
    expect(extractAssistantText(sessionFixture(), "run_2")).toBe("first part\n\nsecond part");
  });

  it("falls back to the latest active-context assistant text", () => {
    expect(extractAssistantText(sessionFixture(), "missing")).toBe("fallback answer");
  });
});

describe("sessionStartConfig", () => {
  it("defaults the messaging toolset on with no recipe", () => {
    expect(sessionStartConfig(null)).toEqual({ tools: { messaging: true } });
  });

  it("preserves recipe config and adds messaging when absent", () => {
    const recipe: SessionRecipe = {
      mounts: [],
      mcp: [],
      config: { model: { providerId: "p", apiKind: "k", model: "m" }, tools: { webSearch: true } },
    };
    expect(sessionStartConfig(recipe)).toEqual({
      model: { providerId: "p", apiKind: "k", model: "m" },
      tools: { webSearch: true, messaging: true },
    });
  });

  it("lets a recipe disable messaging explicitly", () => {
    const recipe: SessionRecipe = { mounts: [], mcp: [], config: { tools: { messaging: false } } };
    expect(sessionStartConfig(recipe).tools?.messaging).toBe(false);
  });
});

interface RecordedCall {
  method: string;
  params: Record<string, unknown>;
}

class FakeClient {
  readonly calls: RecordedCall[] = [];
  readonly environments = new Map<string, Record<string, unknown>>();

  async call(method: string, params: Record<string, unknown>): Promise<unknown> {
    this.calls.push({ method, params });
    if (method === "session/environments/read") {
      const environment = this.environments.get(String(params.envId));
      if (!environment) {
        throw new LightspeedRpcError({
          code: -32004,
          message: "environment not found",
        });
      }
      return { result: { environment } };
    }
    if (method === "session/read") {
      return { result: { session: sessionFixture() } };
    }
    return { result: {} };
  }

  async startRun(
    sessionId: string,
    input: unknown[],
    options: Record<string, unknown> = {},
  ): Promise<unknown> {
    this.calls.push({ method: "run/start", params: { sessionId, input, options } });
    return { result: { run: { id: "run_1", input, items: [], status: "running" } } };
  }

  async awaitRun(
    sessionId: string,
    runId: string,
    options: { after?: unknown } = {},
  ): Promise<unknown> {
    this.calls.push({
      method: "awaitRun",
      params: { sessionId, runId, after: options.after ?? null },
    });
    return { state: { status: "completed" }, cursor: { seq: 1 }, page: { result: {} } };
  }
}

describe("LightspeedSessionBridge.ensureSession", () => {
  let dir: string;
  let store: JsonBridgeStore;

  beforeEach(async () => {
    dir = await mkdtemp(path.join(tmpdir(), "bridge-ls-"));
    store = new JsonBridgeStore(path.join(dir, "state.json"));
  });

  afterEach(async () => {
    await rm(dir, { recursive: true, force: true });
  });

  function bridge(client: FakeClient): LightspeedSessionBridge {
    return new LightspeedSessionBridge(client as unknown as LightspeedClient, store, {
      endpoint: "http://test",
      cwd: null,
      waitMs: 1,
      eventLimit: 1,
      sessionPrefix: "test",
    });
  }

  it("starts, mounts, and links once per session in order", async () => {
    const client = new FakeClient();
    const recipe: SessionRecipe = {
      config: { tools: { filesystem: "readOnly" } },
      mounts: [
        { mountPath: "/workspace", source: { workspaceId: "ws-1" }, access: "readWrite" },
      ],
      mcp: [{ serverId: "github", allowedTools: ["search"] }],
    };
    const ls = bridge(client);
    await ls.ensureSession("session_x", recipe);
    await ls.ensureSession("session_x", recipe);

    expect(client.calls.map((call) => call.method)).toEqual([
      "session/start",
      "vfs/mount/put",
      "session/mcp/link",
    ]);
    expect(client.calls[0]?.params.config).toEqual({
      tools: { filesystem: "readOnly", messaging: true },
    });
    expect(client.calls[1]?.params).toMatchObject({
      sessionId: "session_x",
      mountPath: "/workspace",
      source: { type: "workspace", workspaceId: "ws-1" },
      access: "readWrite",
    });
    expect(client.calls[2]?.params).toMatchObject({
      sessionId: "session_x",
      serverId: "github",
      allowedTools: ["search"],
    });
  });

  it("starts with the default config and no provisioning when no recipe", async () => {
    const client = new FakeClient();
    await bridge(client).ensureSession("session_y");
    expect(client.calls.map((call) => call.method)).toEqual(["session/start"]);
    expect(client.calls[0]?.params.config).toEqual({ tools: { messaging: true } });
  });

  it("attaches a missing recipe environment", async () => {
    const client = new FakeClient();
    const recipe: SessionRecipe = {
      mounts: [],
      mcp: [],
      environments: [
        {
          envId: "devbox",
          providerId: "hetzner-devbox",
          targetId: "local",
          activate: true,
        },
      ],
    };
    await bridge(client).ensureSession("session_env", recipe);

    expect(client.calls.map((call) => call.method)).toEqual([
      "session/start",
      "session/environments/read",
      "session/environments/attach",
    ]);
    expect(client.calls[2]?.params).toMatchObject({
      sessionId: "session_env",
      envId: "devbox",
      providerId: "hetzner-devbox",
      request: { type: "target", targetId: "local" },
      activate: true,
    });
  });

  it("activates an existing inactive recipe environment", async () => {
    const client = new FakeClient();
    client.environments.set("devbox", { envId: "devbox", active: false });
    const recipe: SessionRecipe = {
      mounts: [],
      mcp: [],
      environments: [
        {
          envId: "devbox",
          providerId: "hetzner-devbox",
          targetId: "local",
          activate: true,
        },
      ],
    };
    await bridge(client).ensureSession("session_env", recipe);

    expect(client.calls.map((call) => call.method)).toEqual([
      "session/start",
      "session/environments/read",
      "session/environments/activate",
    ]);
    expect(client.calls[2]?.params).toMatchObject({
      sessionId: "session_env",
      envId: "devbox",
    });
  });

  it("awaits each submitted run from a fresh cursor", async () => {
    const client = new FakeClient();
    await store.getOrCreateBinding("conversation", {
      channel: "telegram",
      accountId: "default",
      chatId: "chat",
      sessionId: "session_submit",
      activation: "dm",
    });
    await store.updateCursor("conversation", { seq: 99 });

    const reply = await bridge(client).submitTurn({
      provider: "telegram",
      accountId: "default",
      conversationKey: "conversation",
      sessionId: "session_submit",
      submissionParts: ["message_1"],
      text: "hello",
    });

    expect(client.calls.find((call) => call.method === "awaitRun")?.params.after).toBeNull();
    expect(reply.text).toBe("old answer");
  });
});

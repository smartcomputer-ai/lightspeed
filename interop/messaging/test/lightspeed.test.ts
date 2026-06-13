import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import type { LightspeedClient, SessionView } from "@lightspeed/agent-client";
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
  async call(method: string, params: Record<string, unknown>): Promise<unknown> {
    this.calls.push({ method, params });
    return { result: {} };
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
      config: { tools: { host: "readOnly" } },
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
      tools: { host: "readOnly", messaging: true },
    });
    expect(client.calls[1]?.params).toMatchObject({
      sessionId: "session_x",
      mountPath: "/workspace",
      source: { workspaceId: "ws-1" },
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
});

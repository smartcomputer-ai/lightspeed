import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { type LightspeedClient, type ProfileSource, type SessionView } from "@lightspeed/agent-client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
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
  it("defaults the messaging toolset on with no profile", () => {
    expect(sessionStartConfig()).toEqual({ tools: { messaging: true } });
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
    return new LightspeedSessionBridge(client as unknown as LightspeedClient, {
      endpoint: "http://test",
      cwd: null,
      waitMs: 1,
      eventLimit: 1,
      sessionPrefix: "test",
    });
  }

  it("starts with an inline profile once per session", async () => {
    const client = new FakeClient();
    const profile: ProfileSource = {
      kind: "inline" as const,
      profile: {
        config: { tools: { filesystem: "readOnly" } },
        mounts: [
          {
            mountPath: "/workspace",
            source: { type: "workspace" as const, workspaceId: "ws-1" },
            access: "readWrite" as const,
          },
        ],
        mcp: [{ serverId: "github", allowedTools: ["search"] }],
      },
    };
    const ls = bridge(client);
    await ls.ensureSession("session_x", profile);
    await ls.ensureSession("session_x", profile);

    expect(client.calls.map((call) => call.method)).toEqual(["session/start"]);
    expect(client.calls[0]?.params.profile).toEqual(profile);
  });

  it("starts with the default config and no provisioning when no profile", async () => {
    const client = new FakeClient();
    await bridge(client).ensureSession("session_y");
    expect(client.calls.map((call) => call.method)).toEqual(["session/start"]);
    expect(client.calls[0]?.params.config).toEqual({ tools: { messaging: true } });
  });

  it("passes profile environments through session start", async () => {
    const client = new FakeClient();
    const profile: ProfileSource = {
      kind: "inline" as const,
      profile: {
        environments: [
          {
            envId: "devbox",
            providerId: "hetzner-devbox",
            targetId: "local",
            activate: true,
          },
        ],
      },
    };
    await bridge(client).ensureSession("session_env", profile);

    expect(client.calls.map((call) => call.method)).toEqual(["session/start"]);
    expect(client.calls[0]?.params.profile).toEqual(profile);
  });

  it("passes named profiles through session start", async () => {
    const client = new FakeClient();
    await bridge(client).ensureSession("session_profile", { kind: "named", profileId: "support" });

    expect(client.calls.map((call) => call.method)).toEqual(["session/start"]);
    expect(client.calls[0]?.params).toMatchObject({
      sessionId: "session_profile",
      profile: { kind: "named", profileId: "support" },
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

import { describe, expect, it, vi } from "vitest";
import type { SessionView } from "@lightspeed/agent-client";
import {
  createLightspeedActivities,
  extractAssistantText,
  runUsedMessagingTool,
} from "../src/activities/lightspeed.js";
import {
  CHANNEL_TOOL_DEADLINE_MS,
  CHANNEL_TOOL_DESCRIPTIONS,
  CHANNEL_TOOL_SCHEMAS,
} from "../src/contracts/tools.js";

describe("ensureManagedSession", () => {
  it("stores schemas and creates the immutable managed session", async () => {
    const requests: Array<{ headers: Headers; body: Record<string, unknown> }> = [];
    const toolAssetCount =
      Object.keys(CHANNEL_TOOL_SCHEMAS).length + Object.keys(CHANNEL_TOOL_DESCRIPTIONS).length;
    const fetch = vi.fn<typeof globalThis.fetch>(async (_input, init) => {
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      requests.push({ headers: new Headers(init?.headers), body });
      const method = body.method;
      if (method === "blobs/put") {
        return Response.json({
          id: body.id,
          result: {
            result: {
              blobs: Array.from({ length: toolAssetCount }, (_, index) => ({
                blobRef: `blob:tool-asset-${index}`,
              })),
            },
          },
        });
      }
      return Response.json({
        id: body.id,
        result: {
          result: {
            session: { id: "channel:v1:telegram:one", createdAtMs: 1234 },
          },
        },
      });
    });
    const activities = createLightspeedActivities({
      endpoint: "http://lightspeed.test/rpc",
      fetch,
    });

    const result = await activities.ensureManagedSession({
      universeId: "6f3fac85-1ec8-4c27-9c97-f403355d5e6f",
      sessionId: "channel:v1:telegram:one",
      displayName: "Telegram chat",
      profileId: "channels-default",
      controller: {
        workflowId: "lsbot.channels.v1/workflow",
        workflowKind: "channelSessionWorkflowV1",
      },
    });

    expect(result).toEqual({ sessionId: "channel:v1:telegram:one", createdAtMs: 1234 });
    expect(requests).toHaveLength(2);
    expect(requests[0]?.headers.get("x-lightspeed-universe")).toBe(
      "6f3fac85-1ec8-4c27-9c97-f403355d5e6f",
    );
    expect(requests[0]?.body.method).toBe("blobs/put");
    expect(
      ((requests[0]?.body.params as { blobs?: unknown[] } | undefined)?.blobs ?? []),
    ).toHaveLength(toolAssetCount);
    expect(requests[1]?.body).toMatchObject({
      method: "session/managed/start",
      params: {
        sessionId: "channel:v1:telegram:one",
        displayName: "Telegram chat",
        profile: { kind: "named", profileId: "channels-default" },
        workflowTools: {
          version: 1,
          lifecycleController: {
            workflowId: "lsbot.channels.v1/workflow",
            workflowKind: "channelSessionWorkflowV1",
          },
        },
      },
    });
    const workflowTools = (
      requests[1]?.body.params as
        | { workflowTools?: { tools?: Array<Record<string, unknown>> } }
        | undefined
    )?.workflowTools?.tools;
    expect(workflowTools).toHaveLength(4);
    for (const tool of workflowTools?.slice(0, 3) ?? []) {
      expect(tool).toMatchObject({
        target: { type: "bound", dispatch: "push" },
        completion: {
          type: "joined",
          deadlineAfterMs: CHANNEL_TOOL_DEADLINE_MS,
          replySchemaRef: "blob:tool-asset-4",
        },
      });
      expect(tool.completion).not.toHaveProperty("keySource");
      expect(tool.completion).not.toHaveProperty("maxPromises");
    }
    expect(workflowTools?.[3]).toMatchObject({
      target: { type: "bound", dispatch: "pull" },
      completion: { type: "accepted" },
    });
  });

  it("reads and writes JSON through universe-scoped CAS calls", async () => {
    const requests: Array<Record<string, unknown>> = [];
    const fetch = vi.fn<typeof globalThis.fetch>(async (_input, init) => {
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      requests.push(body);
      if (body.method === "blobs/read") {
        const bytes = Buffer.from(JSON.stringify({ text: "hello" }), "utf8");
        return Response.json({
          id: body.id,
          result: {
            result: {
              blobRef: `sha256:${"a".repeat(64)}`,
              bytes: bytes.byteLength,
              bytesBase64: bytes.toString("base64"),
            },
          },
        });
      }
      return Response.json({
        id: body.id,
        result: {
          result: {
            blobs: [{ blobRef: `sha256:${"b".repeat(64)}`, bytes: 42 }],
          },
        },
      });
    });
    const activities = createLightspeedActivities({
      endpoint: "http://lightspeed.test/rpc",
      fetch,
    });
    const universeId = "6f3fac85-1ec8-4c27-9c97-f403355d5e6f";

    await expect(
      activities.readJsonBlob({ universeId, blobRef: `sha256:${"a".repeat(64)}` }),
    ).resolves.toEqual({ text: "hello" });
    await expect(
      activities.putJsonBlob({ universeId, value: { provider: "telegram", messageIds: ["42"] } }),
    ).resolves.toEqual({ blobRef: `sha256:${"b".repeat(64)}` });
    expect(requests.map((request) => request.method)).toEqual(["blobs/read", "blobs/put"]);
  });

  it("starts a run with stable submission and terminal notification ids", async () => {
    let request: Record<string, unknown> | undefined;
    const fetch = vi.fn<typeof globalThis.fetch>(async (_input, init) => {
      request = JSON.parse(String(init?.body)) as Record<string, unknown>;
      return Response.json({
        id: request.id,
        result: {
          result: {
            run: { id: "17", status: "queued", source: { type: "input", items: [] } },
          },
        },
      });
    });
    const activities = createLightspeedActivities({
      endpoint: "http://lightspeed.test/rpc",
      fetch,
    });

    await expect(
      activities.startChannelRun({
        universeId: "6f3fac85-1ec8-4c27-9c97-f403355d5e6f",
        sessionId: "channel:v1:telegram:one",
        submissionId: "channels-v1-submission",
        terminalToken: "channels-terminal-v1-token",
        items: [{ type: "text", text: "hello" }],
      }),
    ).resolves.toEqual({ runId: "17" });
    expect(request).toMatchObject({
      method: "session/runs/start",
      params: {
        sessionId: "channel:v1:telegram:one",
        source: { type: "input", items: [{ type: "text", text: "hello" }] },
        submissionId: "channels-v1-submission",
        notifyOnTerminal: { token: "channels-terminal-v1-token" },
      },
    });
  });

  it("appends idempotent room context and returns activation text", async () => {
    let request: Record<string, unknown> | undefined;
    const fetch = vi.fn<typeof globalThis.fetch>(async (_input, init) => {
      request = JSON.parse(String(init?.body)) as Record<string, unknown>;
      return Response.json({
        id: request.id,
        result: {
          result: {
            contextRevision: 9,
            results: [
              {
                key: "channel.room.v1.abc",
                status: "applied",
                activationText: "transcribed context",
              },
            ],
          },
        },
      });
    });
    const activities = createLightspeedActivities({
      endpoint: "http://lightspeed.test/rpc",
      fetch,
    });

    await expect(
      activities.appendChannelContext({
        universeId: "6f3fac85-1ec8-4c27-9c97-f403355d5e6f",
        sessionId: "channel:v1:telegram:one",
        entries: [
          { key: "channel.room.v1.abc", item: { type: "text", text: "room context" } },
        ],
      }),
    ).resolves.toEqual({
      contextRevision: 9,
      results: [
        {
          key: "channel.room.v1.abc",
          status: "applied",
          activationText: "transcribed context",
        },
      ],
    });
    expect(request).toMatchObject({
      method: "session/context/append",
      params: {
        sessionId: "channel:v1:telegram:one",
        entries: [
          { key: "channel.room.v1.abc", item: { type: "text", text: "room context" } },
        ],
      },
    });
  });

  it("removes bounded room context idempotently", async () => {
    let request: Record<string, unknown> | undefined;
    const fetch = vi.fn<typeof globalThis.fetch>(async (_input, init) => {
      request = JSON.parse(String(init?.body)) as Record<string, unknown>;
      return Response.json({
        id: request.id,
        result: {
          result: {
            contextRevision: 10,
            results: [
              { key: "channel.room.v1.old", status: "removed" },
              { key: "channel.room.v1.gone", status: "absent" },
            ],
          },
        },
      });
    });
    const activities = createLightspeedActivities({
      endpoint: "http://lightspeed.test/rpc",
      fetch,
    });

    await expect(
      activities.removeChannelContext({
        universeId: "6f3fac85-1ec8-4c27-9c97-f403355d5e6f",
        sessionId: "channel:v1:telegram:one",
        keys: ["channel.room.v1.old", "channel.room.v1.gone"],
      }),
    ).resolves.toEqual({
      contextRevision: 10,
      results: [
        { key: "channel.room.v1.old", status: "removed" },
        { key: "channel.room.v1.gone", status: "absent" },
      ],
    });
    expect(request).toMatchObject({
      method: "session/context/remove",
      params: {
        sessionId: "channel:v1:telegram:one",
        keys: ["channel.room.v1.old", "channel.room.v1.gone"],
      },
    });
  });

  it("reconciles successful messaging tools from the authoritative run log", () => {
    const session = {
      runs: [
        {
          id: "run_1",
          entries: [
            { kind: { type: "message", role: "assistant" }, text: "first" },
            { kind: { type: "message", role: "assistant" }, text: "second" },
            { kind: { type: "toolCall", name: "message_noop", callId: "call-1" } },
            { kind: { type: "toolResult", callId: "call-1", isError: false } },
          ],
        },
      ],
    } as unknown as SessionView;
    expect(runUsedMessagingTool(session, "run_1")).toBe(true);
    expect(extractAssistantText(session, "run_1")).toBe("first\n\nsecond");
  });

  it("maps numeric terminal run ids to projected session run ids", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>(async () =>
      Response.json({
        id: 1,
        result: {
          result: {
            session: {
              runs: [
                {
                  id: "run_7",
                  entries: [
                    {
                      kind: { type: "message", role: "assistant" },
                      text: "projected assistant response",
                    },
                  ],
                },
              ],
            },
          },
        },
      }),
    );
    const activities = createLightspeedActivities({
      endpoint: "http://lightspeed.test/rpc",
      fetch,
    });

    await expect(
      activities.reconcileTerminalRun({
        universeId: "6f3fac85-1ec8-4c27-9c97-f403355d5e6f",
        sessionId: "channel:v1:telegram:one",
        runId: 7,
        status: "completed",
      }),
    ).resolves.toEqual({ action: "deliver", text: "projected assistant response" });
  });
});

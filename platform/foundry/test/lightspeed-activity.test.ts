import { describe, expect, it, vi } from "vitest";
import { createFoundryLightspeedActivities } from "../src/activities/lightspeed.js";
import {
  FOUNDRY_TOOL_DESCRIPTIONS,
  FOUNDRY_TOOL_SCHEMAS,
} from "../src/contracts/foundry.js";

describe("Foundry Lightspeed activities", () => {
  it("applies a resolved manager profile and installs pull-accepted tools", async () => {
    const requests: Record<string, unknown>[] = [];
    const assetCount =
      Object.keys(FOUNDRY_TOOL_SCHEMAS).length + Object.keys(FOUNDRY_TOOL_DESCRIPTIONS).length;
    const fetch = vi.fn<typeof globalThis.fetch>(async (_input, init) => {
      const request = JSON.parse(String(init?.body)) as Record<string, unknown>;
      requests.push(request);
      if (request.method === "profiles/read") {
        return rpc(request, {
          profile: {
            profileId: "manager",
            revision: 4,
            createdAtMs: 1,
            updatedAtMs: 2,
            activeEnvironmentId: "profile-environment-must-not-win",
            instructions: { type: "text", text: "Base manager instructions." },
            config: {
              features: {
                environments: { jobs: true },
                fleet: { profiles: {}, spawn: {} },
              },
            },
          },
        });
      }
      if (request.method === "blobs/put") {
        return rpc(request, {
          blobs: Array.from({ length: assetCount }, (_, index) => ({
            blobRef: `sha256:${index.toString(16).padStart(64, "0")}`,
            bytes: 10,
          })),
        });
      }
      return rpc(request, {
        session: {
          id: "foundry:v1:hello",
          status: "idle",
          managed: true,
          configRevision: 1,
          createdAtMs: 1,
          updatedAtMs: 1,
          activeContext: { revision: 1, entries: [] },
        },
      });
    });
    const activities = createFoundryLightspeedActivities({
      endpoint: "http://lightspeed.test/rpc",
      fetch,
    });

    await expect(
      activities.ensureFoundrySession({
        universeId: "6f3fac85-1ec8-4c27-9c97-f403355d5e6f",
        sessionId: "foundry:v1:hello",
        displayName: "foundry hello",
        managerProfileId: "manager",
        packName: "hello",
        repoUrl: "https://github.com/acme/hello.git",
        runtimeTarget: { kind: "kubernetes", name: "hello-prod" },
        appliedProfileRevision: 3,
        controller: { workflowId: "foundry/hello", workflowKind: "foundryPackWorkflowV1" },
      }),
    ).resolves.toEqual({ profileRevision: 4 });

    expect(requests.map((request) => request.method)).toEqual([
      "profiles/read",
      "blobs/put",
      "session/managed/start",
      "session/profiles/apply",
    ]);
    const managed = requests[2]?.params as {
      profile?: { profile?: { instructions?: { text?: string }; activeEnvironmentId?: string } };
      workflowTools?: { tools?: Array<Record<string, unknown>> };
    };
    expect(managed.profile?.profile?.instructions?.text).toContain(
      "Repository: https://github.com/acme/hello.git",
    );
    expect(managed.profile?.profile?.instructions?.text).toContain(
      "Registered runtime target: kubernetes:hello-prod",
    );
    expect(managed.profile?.profile).not.toHaveProperty("activeEnvironmentId");
    expect(managed.workflowTools?.tools).toHaveLength(2);
    for (const tool of managed.workflowTools?.tools ?? []) {
      expect(tool).toMatchObject({
        target: { type: "bound", dispatch: "pull" },
        completion: { type: "accepted" },
      });
    }
  });

  it("starts an event run with a small instruction and a referenced document", async () => {
    let request: Record<string, unknown> | undefined;
    const fetch = vi.fn<typeof globalThis.fetch>(async (_input, init) => {
      request = JSON.parse(String(init?.body)) as Record<string, unknown>;
      return rpc(request, { run: { id: "run_9", status: "queued", source: { type: "input", items: [] } } });
    });
    const activities = createFoundryLightspeedActivities({ endpoint: "http://lightspeed.test/rpc", fetch });
    const ref = `sha256:${"a".repeat(64)}`;

    await expect(
      activities.startFoundryRun({
        universeId: "6f3fac85-1ec8-4c27-9c97-f403355d5e6f",
        sessionId: "foundry:v1:hello",
        event: { version: 1, id: "delivery-9", ref },
        submissionId: "submission",
        terminalToken: "terminal",
      }),
    ).resolves.toEqual({ runId: "run_9" });
    expect(request).toMatchObject({
      method: "session/runs/start",
      params: {
        source: { type: "input", items: [{ type: "text" }, { type: "textRef", blobRef: ref }] },
        submissionId: "submission",
        notifyOnTerminal: { token: "terminal" },
      },
    });
  });
});

function rpc(request: Record<string, unknown>, result: unknown): Response {
  return Response.json({ id: request.id, result: { result } });
}

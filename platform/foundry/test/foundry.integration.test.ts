import { fileURLToPath } from "node:url";
import { TestWorkflowEnvironment } from "@temporalio/testing";
import { Worker } from "@temporalio/worker";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import type { EmissionEnvelope } from "../src/contracts/emissions.js";
import {
  FOUNDRY_ACTIVITY_TASK_QUEUE,
  FOUNDRY_EVENT_RESOLVE_TOOL_ID,
  FOUNDRY_EVENT_SIGNAL,
  FOUNDRY_PACK_WORKFLOW,
  FOUNDRY_RELEASE_RECORD_TOOL_ID,
  FOUNDRY_STATE_QUERY,
  FOUNDRY_WORKFLOW_TASK_QUEUE,
  foundryEventTerminalToken,
  foundryPackWorkflowId,
  foundrySessionId,
  type FoundryEvent,
  type FoundryPackStartV1,
} from "../src/contracts/foundry.js";
import type { FoundryPackSnapshot } from "../src/workflows/foundry-pack.js";

const runIntegration = process.env.FOUNDRY_TEMPORAL_INTEGRATION === "1";
const universeId = "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f";
const packId = "0b54d227-08a2-45a8-9b3f-6a4c21d1a111";
const packName = "hello";
const eventRef = `sha256:${"a".repeat(64)}`;
const resolveRef = `sha256:${"b".repeat(64)}`;
const releaseRef = `sha256:${"c".repeat(64)}`;

describe.runIf(runIntegration)("foundry pack workflow", () => {
  let env: TestWorkflowEnvironment;

  beforeAll(async () => {
    env = await TestWorkflowEnvironment.createLocal();
  }, 120_000);

  afterAll(async () => {
    await env.teardown();
  });

  it("deduplicates events, drives the manager, pull-reconciles facts, and handles direct runs", async () => {
    const calls: string[] = [];
    const releases: unknown[] = [];
    const workflowWorker = await Worker.create({
      connection: env.nativeConnection,
      namespace: env.namespace ?? "default",
      taskQueue: FOUNDRY_WORKFLOW_TASK_QUEUE,
      workflowsPath: fileURLToPath(new URL("./workflows.ts", import.meta.url)),
    });
    const activityWorker = await Worker.create({
      connection: env.nativeConnection,
      namespace: env.namespace ?? "default",
      taskQueue: FOUNDRY_ACTIVITY_TASK_QUEUE,
      activities: {
        ensureFoundrySession: async () => {
          calls.push("ensureSession");
          return { profileRevision: 7 };
        },
        resolveFoundryEnvironment: async () => ({ environmentId: "environment_test" }),
        activateEnvironment: async () => {
          calls.push("activateEnvironment");
        },
        readFoundrySessionStatus: async () => ({ status: "idle" }),
        startFoundryRun: async () => {
          calls.push("startRun");
          return { runId: "run_1" };
        },
        readWorkflowToolInvocations: async ({ afterSeq }: { afterSeq: number }) =>
          afterSeq === 0
            ? {
                nextSeq: 10,
                invocations: [
                  {
                    invocationId: `wti:sha256:${"d".repeat(64)}`,
                    toolId: FOUNDRY_EVENT_RESOLVE_TOOL_ID,
                    runId: "run_1",
                    argumentsRef: resolveRef,
                  },
                ],
              }
            : {
                nextSeq: 20,
                invocations: [
                  {
                    invocationId: `wti:sha256:${"e".repeat(64)}`,
                    toolId: FOUNDRY_RELEASE_RECORD_TOOL_ID,
                    runId: "run_2",
                    argumentsRef: releaseRef,
                  },
                ],
              },
        readJsonBlob: async ({ blobRef }: { blobRef: string }) => {
          if (blobRef === resolveRef) {
            return { eventId: "delivery-1", outcome: "handled", summary: "change integrated" };
          }
          if (blobRef === releaseRef) {
            return {
              sourceCommit: "abc123",
              artifactDigest: "sha256:image",
              target: "packs-prod",
              outcome: "succeeded",
              initiatedBy: "operator",
              smokePassed: true,
              detailsRef: null,
            };
          }
          throw new Error(`unexpected blob ${blobRef}`);
        },
        recordRelease: async (input: unknown) => releases.push(input),
      },
    });
    const workflowRun = workflowWorker.run();
    const activityRun = activityWorker.run();

    try {
      const start: FoundryPackStartV1 = {
        version: 1,
        universeId,
        packId,
        packName,
        repoUrl: "https://github.com/smartcomputer-ai/ls-pack-hello.git",
        managerProfileId: "foundry-manager",
        enabled: true,
      };
      const event: FoundryEvent = { version: 1, id: "delivery-1", ref: eventRef };
      const handle = await env.client.workflow.signalWithStart(FOUNDRY_PACK_WORKFLOW, {
        workflowId: foundryPackWorkflowId(universeId, packName),
        taskQueue: FOUNDRY_WORKFLOW_TASK_QUEUE,
        args: [start],
        signal: FOUNDRY_EVENT_SIGNAL,
        signalArgs: [event],
      });

      await eventually(
        () => handle.query<FoundryPackSnapshot>(FOUNDRY_STATE_QUERY),
        (state) => state.activeEvent?.id === event.id,
      );
      await handle.signal(FOUNDRY_EVENT_SIGNAL, event);
      await handle.signal("deliver_emission", terminalEmission(1, foundryEventTerminalToken(event.id)));

      const completed = await eventually(
        () => handle.query<FoundryPackSnapshot>(FOUNDRY_STATE_QUERY),
        (state) => state.eventsProcessed === 1,
      );
      expect(completed.duplicateEventCount).toBe(1);
      expect(completed.recentEvents[0]).toMatchObject({
        id: event.id,
        status: "handled",
        runId: "run_1",
        summary: "change integrated",
      });
      expect(completed.appliedProfileRevision).toBe(7);
      expect(completed.environmentId).toBe("environment_test");

      await handle.signal("deliver_emission", terminalEmission(2, "foundry-direct-terminal-v1-test"));
      await eventually(
        () => Promise.resolve(releases.length),
        (count) => count === 1,
      );
      expect(releases[0]).toMatchObject({
        packId,
        release: { sourceCommit: "abc123", outcome: "succeeded" },
      });
      expect(calls).toEqual(["ensureSession", "activateEnvironment", "startRun"]);

      const history = await handle.fetchHistory();
      await Worker.runReplayHistory(
        { workflowsPath: fileURLToPath(new URL("./workflows.ts", import.meta.url)) },
        history,
        foundryPackWorkflowId(universeId, packName),
      );
    } finally {
      workflowWorker.shutdown();
      activityWorker.shutdown();
      await Promise.all([workflowRun, activityRun]);
    }
  }, 60_000);
});

function terminalEmission(runId: number, token: string): EmissionEnvelope {
  return {
    emission_id: `emission:sha256:${runId.toString(16).padStart(64, "0")}`,
    producer: {
      kind: "session",
      universe_id: universeId,
      session_id: foundrySessionId(packName),
      log_seq: runId,
    },
    body: {
      kind: "run_terminal",
      token,
      run_id: runId,
      status: "completed",
      output_ref: null,
      failure_message_ref: null,
    },
  };
}

async function eventually<T>(read: () => Promise<T>, ready: (value: T) => boolean): Promise<T> {
  const deadline = Date.now() + 20_000;
  for (;;) {
    const value = await read();
    if (ready(value)) return value;
    if (Date.now() >= deadline) throw new Error("timed out waiting for workflow state");
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

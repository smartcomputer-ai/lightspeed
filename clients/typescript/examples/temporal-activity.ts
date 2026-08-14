import { Context } from "@temporalio/activity";
import { LightspeedClient, type EventCursor } from "@lightspeed/agent-client";

export interface RunLightspeedStepInput {
  endpoint: string;
  sessionId: string;
  prompt: string;
  submissionId: string;
  after?: EventCursor | null;
}

export async function runLightspeedStep(input: RunLightspeedStepInput) {
  const activity = Context.current();
  const lightspeed = new LightspeedClient(input.endpoint);

  const session = await lightspeed.call("session/start", {
    sessionId: input.sessionId,
    cwd: null,
    config: null,
  });

  const run = await lightspeed.startRun(
    session.result.session.id,
    [{ type: "text", text: input.prompt }],
    { submissionId: input.submissionId },
  );

  return lightspeed.awaitRun(session.result.session.id, run.result.run.id, {
    after: input.after ?? null,
    waitMs: 30_000,
    heartbeat: (cursor) => activity.heartbeat({ cursor }),
  });
}

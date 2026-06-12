import { Context } from "@temporalio/activity";
import { ForgeClient, type EventCursor } from "@forge/agent-client";

export interface RunForgeStepInput {
  endpoint: string;
  sessionId: string;
  prompt: string;
  submissionId: string;
  after?: EventCursor | null;
}

export async function runForgeStep(input: RunForgeStepInput) {
  const activity = Context.current();
  const forge = new ForgeClient(input.endpoint);

  const session = await forge.call("session/start", {
    sessionId: input.sessionId,
    cwd: null,
    config: null,
  });

  const run = await forge.startRun(
    session.result.session.id,
    [{ type: "text", text: input.prompt }],
    { submissionId: input.submissionId },
  );

  return forge.awaitRun(session.result.session.id, run.result.run.id, {
    after: input.after ?? null,
    waitMs: 30_000,
    heartbeat: (cursor) => activity.heartbeat({ cursor }),
  });
}

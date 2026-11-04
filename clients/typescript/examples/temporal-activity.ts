import { Context } from "@temporalio/activity";
import { LightspeedClient, type EventCursor } from "@lightspeed-ai/sdk";

export interface RunLightspeedStepInput {
  sessionId: string;
  prompt: string;
  submissionId: string;
  after?: EventCursor | null;
}

function required(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`Missing ${name}`);
  return value;
}

export async function runLightspeedStep(input: RunLightspeedStepInput) {
  const activity = Context.current();
  // Read the key from the worker's environment: activity input is stored in
  // the workflow's history.
  const lightspeed = new LightspeedClient({
    endpoint: required("LIGHTSPEED_API_URL"),
    apiKey: required("LIGHTSPEED_API_KEY"),
  });

  const { session } = await lightspeed.startSession({ sessionId: input.sessionId });

  const { run } = await lightspeed.startRun(session.id, input.prompt, {
    submissionId: input.submissionId,
  });

  return lightspeed.awaitRun(session.id, run.id, {
    after: input.after ?? null,
    signal: activity.cancellationSignal,
    heartbeat: (cursor) => activity.heartbeat({ cursor }),
  });
}

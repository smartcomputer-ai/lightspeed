import type { WorkflowClient } from "@temporalio/client";
import {
  CHANNEL_INBOUND_SIGNAL,
  CHANNEL_SESSION_WORKFLOW,
  CHANNELS_WORKFLOW_TASK_QUEUE,
  type AdmittedChannelInboundV1,
  type ChannelSessionStartV1,
  parseAdmittedChannelInboundV1,
} from "../contracts/channel.js";
import { channelSessionIdentity } from "../identity/ids.js";
import { channelSessionSearchAttributes } from "../contracts/search-attributes.js";

type ChannelSessionWorkflow = (start: ChannelSessionStartV1) => Promise<never>;

export interface SignalInboundResult {
  workflowId: string;
  signaledRunId: string;
}

/**
 * Durably admit one already-authorized provider event into Channels.
 * Provider acknowledgement is safe only after this call resolves.
 */
export async function signalInbound(
  client: WorkflowClient,
  start: ChannelSessionStartV1,
  rawInbound: unknown,
): Promise<SignalInboundResult> {
  const inbound = parseAdmittedChannelInboundV1(rawInbound);
  const identity = channelSessionIdentity({
    universeId: start.universeId,
    provider: start.initialRoute.provider,
    accountId: start.initialRoute.accountId,
    sessionKey: start.sessionKey,
  });
  assertIdentity(start, inbound, identity);

  const handle = await client.signalWithStart<ChannelSessionWorkflow, [AdmittedChannelInboundV1]>(
    CHANNEL_SESSION_WORKFLOW,
    {
      workflowId: identity.workflowId,
      taskQueue: CHANNELS_WORKFLOW_TASK_QUEUE,
      args: [start],
      signal: CHANNEL_INBOUND_SIGNAL,
      signalArgs: [inbound],
      typedSearchAttributes: channelSessionSearchAttributes(start),
    },
  );
  return { workflowId: identity.workflowId, signaledRunId: handle.signaledRunId };
}

function assertIdentity(
  start: ChannelSessionStartV1,
  inbound: AdmittedChannelInboundV1,
  expected: ReturnType<typeof channelSessionIdentity>,
): void {
  if (start.sessionId !== expected.sessionId) {
    throw new TypeError(`sessionId must be ${expected.sessionId}`);
  }
  if (start.deliveryTaskQueue !== expected.deliveryTaskQueue) {
    throw new TypeError(`deliveryTaskQueue must be ${expected.deliveryTaskQueue}`);
  }
  if (start.scope !== (inbound.isDirect ? "direct" : "group")) {
    throw new TypeError("channel session scope must match the inbound route scope");
  }
  if (
    inbound.route.provider !== start.initialRoute.provider ||
    inbound.route.accountId !== start.initialRoute.accountId ||
    inbound.route.chatId !== start.initialRoute.chatId ||
    inbound.route.threadId !== start.initialRoute.threadId
  ) {
    throw new TypeError("inbound route must match the channel session route");
  }
}

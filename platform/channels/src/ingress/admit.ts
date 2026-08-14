import type { WorkflowClient } from "@temporalio/client";
import type { NormalizedInboundV1 } from "../contracts/channel.js";
import type {
  ChannelControlPlane,
  ResolvedChannelBinding,
} from "../control-plane/bindings.js";
import { channelSessionIdentity } from "../identity/ids.js";
import { signalInbound, type SignalInboundResult } from "./signal.js";

export type AdmitInboundResult =
  | { status: "unbound" }
  | { status: "pairing_required" }
  | { status: "pairing_pending" }
  | { status: "paired"; binding: ResolvedChannelBinding }
  | {
      status: "admitted";
      binding: ResolvedChannelBinding;
      workflowId: string;
      signaledRunId: string;
    };

export async function admitInbound(
  client: WorkflowClient,
  controlPlane: ChannelControlPlane,
  inbound: NormalizedInboundV1,
): Promise<AdmitInboundResult> {
  const decision = await controlPlane.admit(inbound);
  if (decision.status !== "bound") {
    return decision;
  }
  const binding = decision.binding;
  const identity = channelSessionIdentity({
    universeId: binding.universeId,
    provider: inbound.route.provider,
    accountId: inbound.route.accountId,
    sessionKey: binding.sessionKey,
  });
  const signaled: SignalInboundResult = await signalInbound(
    client,
    {
      version: 1,
      universeId: binding.universeId,
      bindingId: binding.bindingId,
      displayName: `${inbound.route.provider}: ${binding.universeName}`,
      ...(binding.profileId === null ? {} : { profileId: binding.profileId }),
      sessionId: identity.sessionId,
      sessionKey: binding.sessionKey,
      scope: inbound.isDirect ? "direct" : "group",
      activation: binding.activation,
      access: binding.access,
      initialRoute: inbound.route,
      deliveryTaskQueue: identity.deliveryTaskQueue,
    },
    { ...inbound, authorization: binding.authorization },
  );
  return { status: "admitted", binding, ...signaled };
}

export const PAIRING_REQUIRED_REPLY =
  "This chat is not paired yet. Send the pairing code to connect it.";
export const PAIRING_CONFIRMED_REPLY =
  "Paired. You can now message Lightspeed from this chat.";

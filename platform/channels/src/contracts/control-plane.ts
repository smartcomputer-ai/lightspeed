import type { ChannelRoute } from "./channel.js";

export interface AssertBindingActiveInput {
  bindingId: string;
  route: ChannelRoute;
  scope: "direct" | "group";
}

export interface ControlPlaneActivities {
  assertBindingActive(input: AssertBindingActiveInput): Promise<void>;
}

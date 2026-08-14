import type { ChannelRoute } from "./channel.js";

export interface MaintainChannelTypingInput {
  route: ChannelRoute;
}

export interface ChannelPresenceActivities {
  maintainChannelTyping(input: MaintainChannelTypingInput): Promise<void>;
}

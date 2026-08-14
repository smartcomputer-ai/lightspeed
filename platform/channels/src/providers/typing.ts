import { Context } from "@temporalio/activity";
import type { ChannelProvider } from "../contracts/channel.js";
import type {
  ChannelPresenceActivities,
  MaintainChannelTypingInput,
} from "../contracts/presence.js";

export interface TypingLoopRuntime {
  cancelled: Promise<never>;
  heartbeat(): void;
}

export interface TypingActivityConfig {
  provider: ChannelProvider;
  accountId: string;
  intervalMs: number;
  pulse(input: MaintainChannelTypingInput): Promise<void>;
  clear(input: MaintainChannelTypingInput): Promise<void>;
}

export function createTypingActivities(config: TypingActivityConfig): ChannelPresenceActivities {
  return {
    maintainChannelTyping: (input) => {
      const context = Context.current();
      return runTypingLoop(input, config, {
        cancelled: context.cancelled,
        heartbeat: () => context.heartbeat(),
      });
    },
  };
}

export async function runTypingLoop(
  input: MaintainChannelTypingInput,
  config: TypingActivityConfig,
  runtime: TypingLoopRuntime,
): Promise<void> {
  if (
    input.route.provider !== config.provider ||
    input.route.accountId !== config.accountId
  ) {
    throw new TypeError("typing activity is routed to the wrong provider worker");
  }
  try {
    for (;;) {
      await config.pulse(input);
      runtime.heartbeat();
      await Promise.race([delay(config.intervalMs), runtime.cancelled]);
    }
  } finally {
    try {
      await config.clear(input);
    } catch {
      // Presence is best effort, particularly while a provider is reconnecting.
    }
  }
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

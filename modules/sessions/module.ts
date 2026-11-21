import type { PlatformModule } from "@lightspeed-ai/platform-shared";

export const sessions = {
  id: "sessions",
  name: "Sessions",
  description: "Browse conversations, start sessions, and follow runs.",
  defaultEnabled: true,
} as const satisfies PlatformModule;

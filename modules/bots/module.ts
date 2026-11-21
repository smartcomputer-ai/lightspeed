import type { PlatformModule } from "@lightspeed-ai/platform-shared";

export const bots = {
  id: "bots",
  name: "Bots",
  description: "Manage bots, their triggers, activity, and conversations.",
  defaultEnabled: true,
  requiresFeature: "bots",
} as const satisfies PlatformModule;

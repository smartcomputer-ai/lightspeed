import type { PlatformModule } from "@lightspeed-ai/platform-shared";

export const sessions = {
  id: "sessions",
  name: "Session Browser",
  description: "Show the standalone page for browsing and starting sessions.",
  defaultEnabled: true,
} as const satisfies PlatformModule;

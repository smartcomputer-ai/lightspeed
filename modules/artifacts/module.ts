import type { PlatformModule } from "@lightspeed-ai/platform-shared";

export const artifacts = {
  id: "artifacts",
  name: "Artifacts",
  description: "Publish and browse files in a workspace’s artifacts folder.",
  defaultEnabled: true,
} as const satisfies PlatformModule;

import type { PlatformModule } from "@lightspeed-ai/platform-shared";

export const artifacts = {
  id: "artifacts",
  name: "Artifacts",
  description: "Publish a copy of a workspace file for other people in this universe.",
  defaultEnabled: true,
  permissions: { read: "read", publish: "useResource", retract: "configureResource" },
  operations: [
    { id: "list", method: "GET", path: "/", permission: "read" },
    { id: "get", method: "GET", path: "/:artifactId", permission: "read" },
    { id: "content", method: "GET", path: "/:artifactId/content", permission: "read" },
    { id: "publish", method: "POST", path: "/", permission: "publish" },
    { id: "retract", method: "DELETE", path: "/:artifactId", permission: "retract" },
  ],
} as const satisfies PlatformModule;

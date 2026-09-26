import type { MethodGroup } from "@lightspeed-ai/agent-client";

/**
 * What each core method group lets a key call. Deployment groups address the
 * deployment and only a deployment key holds them. A `Record` keeps this in
 * step with the contract: a new group fails the build until it is labelled.
 */
export const METHOD_GROUPS: Record<MethodGroup, { label: string; deployment?: true }> = {
  session: { label: "Sessions and runs, reading blobs" },
  "blobs/put": { label: "Uploading blobs" },
  vfs: { label: "Workspaces" },
  profiles: { label: "Profiles" },
  models: { label: "Models" },
  mcp: { label: "MCP servers" },
  environments: { label: "Environments" },
  bots: { label: "Bots" },
  channels: { label: "Channel accounts and pairings" },
  "channels/inbound": { label: "Delivering channel messages" },
  auth: { label: "Credentials and OAuth" },
  "auth/lease": { label: "Leasing credentials" },
  "deployment/universes": { label: "Universes", deployment: true },
  "deployment/api-keys": { label: "API keys", deployment: true },
  "deployment/environment-providers": { label: "Environment providers", deployment: true },
  "deployment/channels": { label: "Channel account discovery", deployment: true },
};

export const METHOD_GROUP_NAMES = Object.keys(METHOD_GROUPS) as MethodGroup[];

/** The groups a key of this scope may hold. */
export function groupsFor(scope: "deployment" | "universe"): MethodGroup[] {
  return METHOD_GROUP_NAMES.filter((group) => scope === "deployment" || !METHOD_GROUPS[group].deployment);
}

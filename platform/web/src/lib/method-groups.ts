import type { MethodGroup } from "@lightspeed-ai/agent-client";

/**
 * What each core method group lets a key call. Deployment groups address the
 * deployment and only a deployment key holds them. A `Record` keeps this in
 * step with the contract: a new group fails the build until it is labelled.
 * `caution` names what a group reaches that its label does not make obvious.
 */
export const METHOD_GROUPS: Record<MethodGroup, { label: string; deployment?: true; caution?: string }> = {
  session: {
    label: "Sessions and runs, reading blobs",
    caution: "Reads and controls every session in the universe, private ones included.",
  },
  "blobs/put": { label: "Uploading blobs" },
  vfs: { label: "Workspaces" },
  profiles: { label: "Profiles" },
  models: { label: "Models" },
  mcp: { label: "MCP servers" },
  environments: { label: "Environments" },
  bots: { label: "Bots" },
  channels: { label: "Channel accounts and pairings" },
  "channels/inbound": {
    label: "Delivering channel messages",
    caution: "Delivers messages to bots as if they came from a channel. For channel connectors only.",
  },
  auth: { label: "Credentials and OAuth" },
  "auth/lease": {
    label: "Leasing credentials",
    caution: "Returns the plaintext access tokens behind credential grants. For services that call providers only.",
  },
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

/** "All groups" when a key holds everything its scope allows, else their labels. */
export function groupSummary(scope: "deployment" | "universe", groups: readonly MethodGroup[]): string {
  if (groupsFor(scope).every((group) => groups.includes(group))) return "All groups";
  return groups.map((group) => METHOD_GROUPS[group]?.label ?? group).join(", ");
}

const AGENT_CLIENT_PRESET = {
  id: "agent",
  label: "Agent client",
  description: "Starts sessions and runs, uploads attachments, lists models and uses workspaces.",
  groups: ["session", "blobs/put", "models", "vfs"] as MethodGroup[],
};

/** What a new universe key starts with. */
export const DEFAULT_UNIVERSE_KEY_GROUPS: readonly MethodGroup[] = AGENT_CLIENT_PRESET.groups;

/**
 * Starting points for a universe key. A key holds only the groups chosen;
 * only "All groups" includes credential leasing and channel delivery.
 */
export const UNIVERSE_KEY_PRESETS: { id: string; label: string; description: string; groups: MethodGroup[] }[] = [
  AGENT_CLIENT_PRESET,
  {
    id: "configuration",
    label: "Configuration",
    description: "Configures the universe without reading sessions; the Configurator's own key holds these.",
    groups: ["profiles", "mcp", "environments", "bots", "channels", "auth", "models"],
  },
  {
    id: "all",
    label: "All groups",
    description: "Every universe method, including credential leasing and channel delivery.",
    groups: groupsFor("universe"),
  },
];

/** The preset whose groups are exactly `groups`, if any. */
export function presetFor(groups: ReadonlySet<MethodGroup>) {
  return UNIVERSE_KEY_PRESETS.find((preset) =>
    preset.groups.length === groups.size && preset.groups.every((group) => groups.has(group)));
}

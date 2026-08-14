export const CHANNELS_ROLES = ["workflows", "activities", "telegram", "whatsapp"] as const;

export type ChannelsRole = (typeof CHANNELS_ROLES)[number];
export type ChannelsCommand = ChannelsRole | "all";

const DEFAULT_CONNECTORS: readonly ChannelsRole[] = ["telegram"];

export function resolveChannelsRoles(
  command: string | undefined,
  configuredConnectors: string | undefined,
): ChannelsRole[] {
  const resolvedCommand = command ?? "all";
  if (resolvedCommand !== "all") {
    if (!isChannelsRole(resolvedCommand)) {
      throw new TypeError(
        `unknown Channels command ${JSON.stringify(resolvedCommand)}; expected all, ${CHANNELS_ROLES.join(", ")}`,
      );
    }
    return [resolvedCommand];
  }

  return ["workflows", "activities", ...parseConnectors(configuredConnectors)];
}

function parseConnectors(value: string | undefined): ChannelsRole[] {
  if (value === undefined || value.trim().length === 0) {
    return [...DEFAULT_CONNECTORS];
  }
  const connectors: ChannelsRole[] = [];
  for (const entry of value.split(",")) {
    const connector = entry.trim();
    if (connector !== "telegram" && connector !== "whatsapp") {
      throw new TypeError(
        `invalid CHANNELS_CONNECTORS entry ${JSON.stringify(connector)}; expected telegram or whatsapp`,
      );
    }
    if (!connectors.includes(connector)) {
      connectors.push(connector);
    }
  }
  if (connectors.length === 0) {
    throw new TypeError("CHANNELS_CONNECTORS must contain at least one connector");
  }
  return connectors;
}

function isChannelsRole(value: string): value is ChannelsRole {
  return (CHANNELS_ROLES as readonly string[]).includes(value);
}

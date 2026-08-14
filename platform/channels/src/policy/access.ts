export type ChannelTurnAccess = "conversation" | "members";
export type ChannelControlAccess = "none" | "members" | "admins" | "owners";
export type ChannelMemberRole = "member" | "admin" | "owner" | null;

export interface ChannelAccessSettings {
  turn: ChannelTurnAccess;
  control: ChannelControlAccess;
}

export interface ChannelAuthorization {
  turnAllowed: boolean;
  controlAllowed: boolean;
  memberRole: ChannelMemberRole;
}

export function resolveAccessSettings(value: unknown): ChannelAccessSettings {
  if (value === null || value === undefined) {
    return { turn: "conversation", control: "admins" };
  }
  if (typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("binding access must be an object");
  }
  const input = value as Record<string, unknown>;
  const turn = input.turn ?? "conversation";
  const control = input.control ?? "admins";
  if (turn !== "conversation" && turn !== "members") {
    throw new TypeError("binding access.turn must be conversation or members");
  }
  if (
    control !== "none" &&
    control !== "members" &&
    control !== "admins" &&
    control !== "owners"
  ) {
    throw new TypeError("binding access.control must be none, members, admins, or owners");
  }
  return { turn, control };
}

export function authorizeChannelSender(
  settings: ChannelAccessSettings,
  memberRole: ChannelMemberRole,
): ChannelAuthorization {
  return {
    turnAllowed: settings.turn === "conversation" || memberRole !== null,
    controlAllowed:
      settings.control === "members"
        ? memberRole !== null
        : settings.control === "admins"
          ? memberRole === "owner" || memberRole === "admin"
          : settings.control === "owners"
            ? memberRole === "owner"
            : false,
    memberRole,
  };
}

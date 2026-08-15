import { describe, expect, it } from "vitest";
import { resolveChannelsRoles } from "../src/runtime/roles.js";

describe("Channels runtime roles", () => {
  it("starts core roles and Telegram by default", () => {
    expect(resolveChannelsRoles(undefined, undefined)).toEqual([
      "workflows",
      "activities",
      "telegram",
    ]);
  });

  it("starts all configured connectors with the core roles", () => {
    expect(resolveChannelsRoles("all", "telegram, whatsapp, telegram")).toEqual([
      "workflows",
      "activities",
      "telegram",
      "whatsapp",
    ]);
  });

  it.each(["workflows", "activities", "telegram", "whatsapp"])(
    "can start the %s role independently",
    (role) => {
      expect(resolveChannelsRoles(role, "telegram,whatsapp")).toEqual([role]);
    },
  );

  it("rejects unknown commands and connectors", () => {
    expect(() => resolveChannelsRoles("gateway", undefined)).toThrow("unknown Channels command");
    expect(() => resolveChannelsRoles("all", "signal")).toThrow(
      "invalid LIGHTSPEED_CHANNELS_CONNECTORS entry",
    );
  });
});

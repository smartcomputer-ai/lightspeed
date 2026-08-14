import { describe, expect, it } from "vitest";
import {
  authorizeChannelSender,
  resolveAccessSettings,
} from "../src/policy/access.js";
import { parseChannelControlCommand } from "../src/policy/control.js";

describe("Channels sender policy", () => {
  it("defaults turns to the admitted conversation and controls to admins", () => {
    const settings = resolveAccessSettings(null);
    expect(settings).toEqual({ turn: "conversation", control: "admins" });
    expect(authorizeChannelSender(settings, null)).toEqual({
      turnAllowed: true,
      controlAllowed: false,
      memberRole: null,
    });
    expect(authorizeChannelSender(settings, "admin")).toMatchObject({
      turnAllowed: true,
      controlAllowed: true,
    });
  });

  it("supports member-only turns and role-tiered controls", () => {
    const settings = resolveAccessSettings({ turn: "members", control: "owners" });
    expect(authorizeChannelSender(settings, null).turnAllowed).toBe(false);
    expect(authorizeChannelSender(settings, "admin").controlAllowed).toBe(false);
    expect(authorizeChannelSender(settings, "owner")).toMatchObject({
      turnAllowed: true,
      controlAllowed: true,
    });
  });

  it("rejects malformed access settings", () => {
    expect(() => resolveAccessSettings({ turn: "internet" })).toThrow("access.turn");
    expect(() => resolveAccessSettings({ control: "everyone" })).toThrow("access.control");
  });

  it("parses workflow-owned control commands including bot suffixes", () => {
    expect(parseChannelControlCommand("/activation@lightspeed_bot silent")).toEqual({
      kind: "activation",
      mode: "silent",
    });
    expect(parseChannelControlCommand("/activation invalid")).toEqual({
      kind: "activation_help",
    });
    expect(parseChannelControlCommand("/status")).toEqual({ kind: "status" });
    expect(parseChannelControlCommand("please /status")).toBeNull();
  });
});

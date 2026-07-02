import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import type { ProfileSource } from "@lightspeed/agent-client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  handleDenied,
  loadBridgeConfig,
  parseBindings,
  resolveBinding,
  resolveInboundAccess,
  type BindingRule,
} from "../src/config.js";

let dir: string;

beforeEach(async () => {
  dir = await mkdtemp(path.join(tmpdir(), "bridge-config-"));
});

afterEach(async () => {
  await rm(dir, { recursive: true, force: true });
});

describe("loadBridgeConfig", () => {
  it("rejects legacy top-level recipes", async () => {
    const configPath = path.join(dir, "bridge.json");
    await writeFile(configPath, JSON.stringify({ recipes: { personal: {} } }));

    await expect(loadBridgeConfig({ BRIDGE_CONFIG: configPath })).rejects.toThrow(
      /recipes are no longer supported/,
    );
  });

  it("defaults room retention watermarks to 300/200", async () => {
    const config = await loadBridgeConfig({});
    expect(config.runtime.roomRetentionHigh).toBe(300);
    expect(config.runtime.roomRetentionLow).toBe(200);
  });

  it("parses room retention watermarks from env, including 0 to disable", async () => {
    const config = await loadBridgeConfig({
      BRIDGE_ROOM_RETENTION_HIGH: "50",
      BRIDGE_ROOM_RETENTION_LOW: "10",
    });
    expect(config.runtime.roomRetentionHigh).toBe(50);
    expect(config.runtime.roomRetentionLow).toBe(10);

    // 0 disables retention; the LOW < HIGH check does not apply then.
    const disabled = await loadBridgeConfig({ BRIDGE_ROOM_RETENTION_HIGH: "0" });
    expect(disabled.runtime.roomRetentionHigh).toBe(0);
  });

  it("rejects a LOW watermark at or above HIGH", async () => {
    await expect(
      loadBridgeConfig({
        BRIDGE_ROOM_RETENTION_HIGH: "100",
        BRIDGE_ROOM_RETENTION_LOW: "100",
      }),
    ).rejects.toThrow(/BRIDGE_ROOM_RETENTION_LOW must be smaller/);
    await expect(
      loadBridgeConfig({
        BRIDGE_ROOM_RETENTION_HIGH: "100",
        BRIDGE_ROOM_RETENTION_LOW: "150",
      }),
    ).rejects.toThrow(/BRIDGE_ROOM_RETENTION_LOW must be smaller/);
  });
});

describe("parseBindings", () => {
  it("parses match, named profile, inline profile, and sessionKey", () => {
    const inlineProfile: ProfileSource = {
      kind: "inline",
      profile: { config: { tools: { filesystem: "readOnly" } } },
    };
    const bindings = parseBindings([
      {
        match: { channel: "telegram", handle: "@lukas" },
        profile: "personal",
        sessionKey: "lukas",
      },
      { match: { channel: "*" }, profile: inlineProfile },
    ]);

    expect(bindings).toEqual<BindingRule[]>([
      {
        match: { channel: "telegram", handle: "@lukas" },
        profile: { kind: "named", profileId: "personal" },
        sessionKey: "lukas",
      },
      { match: { channel: "*" }, profile: inlineProfile },
    ]);
  });

  it("parses handle arrays", () => {
    const bindings = parseBindings([
      {
        match: { channel: "telegram", handle: ["@lukas", "6071843755"] },
        profile: { kind: "named", profile_id: "personal" },
        sessionKey: "lukas",
      },
    ]);

    expect(bindings).toEqual<BindingRule[]>([
      {
        match: { channel: "telegram", handle: ["@lukas", "6071843755"] },
        profile: { kind: "named", profileId: "personal" },
        sessionKey: "lukas",
      },
    ]);
  });

  it("parses channel arrays", () => {
    const bindings = parseBindings([
      {
        id: "lukas-chat",
        match: { channel: ["telegram", "whatsapp"] },
        profile: "personal",
        sessionKey: "lukas",
        pairing: { code: "LOCALCODE" },
      },
    ]);

    expect(bindings).toEqual<BindingRule[]>([
      {
        id: "lukas-chat",
        match: { channel: ["telegram", "whatsapp"] },
        profile: { kind: "named", profileId: "personal" },
        sessionKey: "lukas",
        pairing: { code: "LOCALCODE" },
      },
    ]);
  });

  it("rejects invalid channel array entries", () => {
    expect(() => parseBindings([{ match: { channel: ["telegram", "email"] } }])).toThrow(
      /array entries must be telegram or whatsapp/,
    );
  });

  it("parses binding pairing codes from config and env", () => {
    expect(
      parseBindings([
        {
          id: "lukas-telegram",
          match: { channel: "telegram" },
          profile: "personal",
          sessionKey: "lukas",
          pairing: { code: "LOCALCODE" },
        },
      ])[0],
    ).toMatchObject({
      id: "lukas-telegram",
      pairing: { code: "LOCALCODE" },
    });

    expect(
      parseBindings(
        [
          {
            id: "anna-whatsapp",
            match: { channel: "whatsapp" },
            pairing: { codeEnv: "ANNA_PAIRING_CODE" },
          },
        ],
        { ANNA_PAIRING_CODE: "ENVCODE" },
      )[0],
    ).toMatchObject({
      id: "anna-whatsapp",
      pairing: { code: "ENVCODE", codeEnv: "ANNA_PAIRING_CODE" },
    });
  });

  it("requires a stable binding id for pairable bindings", () => {
    expect(() =>
      parseBindings([{ match: { channel: "telegram" }, pairing: { code: "CODE" } }]),
    ).toThrow(/id is required/);
  });

  it("rejects legacy binding recipes", () => {
    expect(() => parseBindings([{ match: { channel: "*" }, recipe: "personal" }])).toThrow(
      /recipe is no longer supported/,
    );
  });
});

describe("resolveBinding", () => {
  const bindings = parseBindings([
    {
      match: { channel: "telegram", handle: "@lukas" },
      profile: "personal",
      sessionKey: "lukas",
    },
    {
      match: { channel: "telegram", chatId: "-100", scope: "group" },
      profile: { kind: "inline", profile: { instructions: "support room" } },
      sessionKey: "eng",
    },
    { match: { channel: "*" }, profile: "support" },
  ]);

  it("matches the first rule by handle, ignoring leading @ and case", () => {
    expect(
      resolveBinding(
        { channel: "telegram", handles: ["123", "Lukas"], chatId: "dm", scope: "direct" },
        bindings,
      ),
    ).toEqual({
      profile: { kind: "named", profileId: "personal" },
      profileLabel: "personal",
      sessionKey: "lukas",
    });
  });

  it("matches any configured handle in a binding handle array", () => {
    const arrayBindings = parseBindings([
      {
        match: { channel: "telegram", handle: ["@lukas", "6071843755"] },
        profile: "personal",
        sessionKey: "lukas",
      },
    ]);
    expect(
      resolveBinding(
        { channel: "telegram", handles: ["6071843755"], chatId: "dm", scope: "direct" },
        arrayBindings,
      ),
    ).toEqual({
      profile: { kind: "named", profileId: "personal" },
      profileLabel: "personal",
      sessionKey: "lukas",
    });
  });

  it("matches any configured channel in a binding channel array", () => {
    const channelBindings = parseBindings([
      {
        match: { channel: ["telegram", "whatsapp"] },
        profile: "personal",
        sessionKey: "lukas",
      },
    ]);
    for (const channel of ["telegram", "whatsapp"] as const) {
      expect(
        resolveBinding(
          { channel, handles: ["123"], chatId: "dm", scope: "direct" },
          channelBindings,
        ),
      ).toEqual({
        profile: { kind: "named", profileId: "personal" },
        profileLabel: "personal",
        sessionKey: "lukas",
      });
    }
  });

  it("matches a group rule by chatId and scope", () => {
    expect(
      resolveBinding(
        { channel: "telegram", handles: ["999"], chatId: "-100", scope: "group" },
        bindings,
      ),
    ).toEqual({
      profile: { kind: "inline", profile: { instructions: "support room" } },
      profileLabel: "inline",
      sessionKey: "eng",
    });
  });

  it("falls through to the wildcard rule", () => {
    expect(
      resolveBinding(
        { channel: "whatsapp", handles: ["41790000000"], chatId: "x", scope: "direct" },
        bindings,
      ),
    ).toEqual({
      profile: { kind: "named", profileId: "support" },
      profileLabel: "support",
      sessionKey: null,
    });
  });

  it("returns the default profile when nothing matches", () => {
    expect(
      resolveBinding({ channel: "telegram", handles: ["1"], chatId: "x", scope: "direct" }, []),
    ).toEqual({ profile: null, profileLabel: null, sessionKey: null });
  });
});

describe("handleDenied", () => {
  it("allows anyone when the allowlist is empty", () => {
    expect(handleDenied([], ["123"])).toBe(false);
  });

  it("denies a sender absent from a non-empty allowlist", () => {
    expect(handleDenied(["@lukas"], ["123", "alice"])).toBe(true);
  });

  it("allows a sender present under any handle, ignoring @ and case", () => {
    expect(handleDenied(["@Lukas"], ["123", "lukas"])).toBe(false);
    expect(handleDenied(["41790000000"], ["41790000000@s.whatsapp.net", "41790000000"])).toBe(false);
  });
});

describe("resolveInboundAccess", () => {
  const bindings = parseBindings([
    {
      match: { channel: "telegram", handle: "@lukas" },
      profile: "personal",
      sessionKey: "lukas",
    },
  ]);

  it("resolves turn/control gates and the bound profile together", () => {
    const access = resolveInboundAccess(
      { channel: "telegram", handles: ["123", "lukas"], chatId: "dm", scope: "direct" },
      { allowFrom: ["@lukas"], controlAllowFrom: ["@lukas"] },
      bindings,
    );
    expect(access.turnAllowed).toBe(true);
    expect(access.controlAllowed).toBe(true);
    expect(access.profileLabel).toBe("personal");
    expect(access.profile).toEqual({ kind: "named", profileId: "personal" });
    expect(access.sessionKey).toBe("lukas");
  });

  it("denies a turn for an unlisted sender and defaults control to direct trust", () => {
    const access = resolveInboundAccess(
      { channel: "telegram", handles: ["999"], chatId: "dm", scope: "direct" },
      { allowFrom: ["@lukas"], controlAllowFrom: [] },
      bindings,
    );
    expect(access.turnAllowed).toBe(false);
    // Empty control allowlist trusts direct chats.
    expect(access.controlAllowed).toBe(true);
    expect(access.profileLabel).toBeNull();
  });

  it("does not trust group members for control with an empty control allowlist", () => {
    const access = resolveInboundAccess(
      { channel: "telegram", handles: ["999"], chatId: "-100", scope: "group" },
      { allowFrom: [], controlAllowFrom: [] },
      bindings,
    );
    expect(access.turnAllowed).toBe(true);
    expect(access.controlAllowed).toBe(false);
  });

  it("exposes consecutive pairable candidates before the fallback binding", () => {
    const access = resolveInboundAccess(
      { channel: "whatsapp", handles: ["41790000000"], chatId: "dm", scope: "direct" },
      { allowFrom: [], controlAllowFrom: [] },
      parseBindings([
        {
          id: "lukas-whatsapp",
          match: { channel: "whatsapp", scope: "direct" },
          profile: "lukas",
          pairing: { code: "LUKAS" },
        },
        {
          id: "anna-whatsapp",
          match: { channel: "whatsapp", scope: "direct" },
          profile: "anna",
          pairing: { code: "ANNA" },
        },
        { match: { channel: "*" } },
      ]),
    );

    expect(access.bindingCandidates.map((candidate) => candidate.bindingId)).toEqual([
      "lukas-whatsapp",
      "anna-whatsapp",
      null,
    ]);
    expect(access.profileLabel).toBe("lukas");
  });
});

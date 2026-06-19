import { describe, expect, it } from "vitest";
import {
  handleDenied,
  parseBindings,
  parseRecipes,
  resolveBinding,
  resolveInboundAccess,
  type BindingRule,
  type SessionRecipe,
} from "../src/config.js";

describe("parseRecipes", () => {
  it("parses config, mounts with defaults, and mcp links", () => {
    const recipes = parseRecipes({
      personal: {
        config: { tools: { filesystem: "readOnly" } },
        mounts: [
          { workspaceId: "ws-1" },
          { mountPath: "/snap", snapshotRef: "snap-1", access: "readOnly" },
        ],
        mcp: [{ serverId: "github", allowedTools: ["search"], approval: "never" }],
        environments: [{ envId: "devbox", providerId: "hetzner-devbox" }],
      },
    });
    const personal = recipes.personal as SessionRecipe;
    expect(personal.config).toEqual({ tools: { filesystem: "readOnly" } });
    expect(personal.mounts).toEqual([
      { mountPath: "/workspace", source: { workspaceId: "ws-1" }, access: "readWrite" },
      { mountPath: "/snap", source: { snapshotRef: "snap-1" }, access: "readOnly" },
    ]);
    expect(personal.mcp).toEqual([
      { serverId: "github", allowedTools: ["search"], approval: "never" },
    ]);
    expect(personal.environments).toEqual([
      { envId: "devbox", providerId: "hetzner-devbox", targetId: "local", activate: true },
    ]);
  });

  it("rejects a mount with neither workspaceId nor snapshotRef", () => {
    expect(() => parseRecipes({ r: { mounts: [{ mountPath: "/x" }] } })).toThrow(
      /workspaceId or snapshotRef/,
    );
  });

  it("rejects an mcp link with no serverId", () => {
    expect(() => parseRecipes({ r: { mcp: [{ allowedTools: [] }] } })).toThrow(/serverId/);
  });

  it("accepts envs as an alias and rejects multiple active environments", () => {
    expect(parseRecipes({ r: { envs: [{ envId: "devbox", providerId: "provider" }] } }).r)
      .toMatchObject({
        environments: [
          { envId: "devbox", providerId: "provider", targetId: "local", activate: true },
        ],
      });
    expect(() =>
      parseRecipes({
        r: {
          environments: [
            { envId: "devbox-a", providerId: "provider-a" },
            { envId: "devbox-b", providerId: "provider-b" },
          ],
        },
      }),
    ).toThrow(/at most one environment/);
  });
});

describe("parseBindings", () => {
  const recipes = parseRecipes({ personal: {}, support: {} });

  it("parses match, recipe, and sessionKey", () => {
    const bindings = parseBindings(
      [
        { match: { channel: "telegram", handle: "@lukas" }, recipe: "personal", sessionKey: "lukas" },
        { match: { channel: "*" }, recipe: "support" },
      ],
      recipes,
    );
    expect(bindings).toEqual<BindingRule[]>([
      { match: { channel: "telegram", handle: "@lukas" }, recipe: "personal", sessionKey: "lukas" },
      { match: { channel: "*" }, recipe: "support" },
    ]);
  });

  it("parses handle arrays", () => {
    const bindings = parseBindings(
      [
        {
          match: { channel: "telegram", handle: ["@lukas", "6071843755"] },
          recipe: "personal",
          sessionKey: "lukas",
        },
      ],
      recipes,
    );
    expect(bindings).toEqual<BindingRule[]>([
      {
        match: { channel: "telegram", handle: ["@lukas", "6071843755"] },
        recipe: "personal",
        sessionKey: "lukas",
      },
    ]);
  });

  it("rejects a binding referencing an undefined recipe", () => {
    expect(() => parseBindings([{ match: { channel: "*" }, recipe: "ghost" }], recipes)).toThrow(
      /not defined in recipes/,
    );
  });
});

describe("resolveBinding", () => {
  const bindings = parseBindings(
    [
      { match: { channel: "telegram", handle: "@lukas" }, recipe: "personal", sessionKey: "lukas" },
      { match: { channel: "telegram", chatId: "-100", scope: "group" }, recipe: "support", sessionKey: "eng" },
      { match: { channel: "*" }, recipe: "support" },
    ],
    parseRecipes({ personal: {}, support: {} }),
  );

  it("matches the first rule by handle, ignoring leading @ and case", () => {
    expect(
      resolveBinding(
        { channel: "telegram", handles: ["123", "Lukas"], chatId: "dm", scope: "direct" },
        bindings,
      ),
    ).toEqual({ recipe: "personal", sessionKey: "lukas" });
  });

  it("matches any configured handle in a binding handle array", () => {
    const arrayBindings = parseBindings(
      [
        {
          match: { channel: "telegram", handle: ["@lukas", "6071843755"] },
          recipe: "personal",
          sessionKey: "lukas",
        },
      ],
      parseRecipes({ personal: {} }),
    );
    expect(
      resolveBinding(
        { channel: "telegram", handles: ["6071843755"], chatId: "dm", scope: "direct" },
        arrayBindings,
      ),
    ).toEqual({ recipe: "personal", sessionKey: "lukas" });
  });

  it("matches a group rule by chatId and scope", () => {
    expect(
      resolveBinding(
        { channel: "telegram", handles: ["999"], chatId: "-100", scope: "group" },
        bindings,
      ),
    ).toEqual({ recipe: "support", sessionKey: "eng" });
  });

  it("falls through to the wildcard rule", () => {
    expect(
      resolveBinding(
        { channel: "whatsapp", handles: ["41790000000"], chatId: "x", scope: "direct" },
        bindings,
      ),
    ).toEqual({ recipe: "support", sessionKey: null });
  });

  it("returns the default recipe when nothing matches", () => {
    expect(
      resolveBinding({ channel: "telegram", handles: ["1"], chatId: "x", scope: "direct" }, []),
    ).toEqual({ recipe: null, sessionKey: null });
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
  const recipes = parseRecipes({ personal: { config: { tools: { filesystem: "readOnly" } } } });
  const bindings = parseBindings(
    [{ match: { channel: "telegram", handle: "@lukas" }, recipe: "personal", sessionKey: "lukas" }],
    recipes,
  );

  it("resolves turn/control gates and the bound recipe together", () => {
    const access = resolveInboundAccess(
      { channel: "telegram", handles: ["123", "lukas"], chatId: "dm", scope: "direct" },
      { allowFrom: ["@lukas"], controlAllowFrom: ["@lukas"] },
      bindings,
      recipes,
    );
    expect(access.turnAllowed).toBe(true);
    expect(access.controlAllowed).toBe(true);
    expect(access.recipeName).toBe("personal");
    expect(access.recipe?.config).toEqual({ tools: { filesystem: "readOnly" } });
    expect(access.sessionKey).toBe("lukas");
  });

  it("denies a turn for an unlisted sender and defaults control to direct trust", () => {
    const access = resolveInboundAccess(
      { channel: "telegram", handles: ["999"], chatId: "dm", scope: "direct" },
      { allowFrom: ["@lukas"], controlAllowFrom: [] },
      bindings,
      recipes,
    );
    expect(access.turnAllowed).toBe(false);
    // Empty control allowlist trusts direct chats.
    expect(access.controlAllowed).toBe(true);
    expect(access.recipeName).toBeNull();
  });

  it("does not trust group members for control with an empty control allowlist", () => {
    const access = resolveInboundAccess(
      { channel: "telegram", handles: ["999"], chatId: "-100", scope: "group" },
      { allowFrom: [], controlAllowFrom: [] },
      bindings,
      recipes,
    );
    expect(access.turnAllowed).toBe(true);
    expect(access.controlAllowed).toBe(false);
  });
});

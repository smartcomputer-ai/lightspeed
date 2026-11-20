import { describe, expect, it } from "vitest";
import {
  CORE_UNIVERSE_ACTIONS,
  DEPLOYMENT_ACTIONS,
  ORGANIZATION_ACTIONS,
  ORGANIZATION_ROLES,
  PLATFORM_UNIVERSE_ACTIONS,
  UNIVERSE_ROLES,
  UNIVERSE_ROLE_ACTIONS,
  canCloseSession,
  canDeleteSession,
  deploymentActions,
  highestRole,
  organizationActions,
  organizationRoleAllows,
  universeActions,
} from "@lightspeed-ai/platform-shared";

describe("role bundles", () => {
  it("place every universe action in some role, and Owner holds all of them", () => {
    for (const action of [...CORE_UNIVERSE_ACTIONS, ...PLATFORM_UNIVERSE_ACTIONS]) {
      expect(UNIVERSE_ROLES.some((role) => UNIVERSE_ROLE_ACTIONS[role].includes(action)), action).toBe(true);
    }
    expect([...universeActions(["owner"])].sort()).toEqual([...CORE_UNIVERSE_ACTIONS, ...PLATFORM_UNIVERSE_ACTIONS].sort());
  });

  it("give each universe role everything the one below holds", () => {
    for (let index = 1; index < UNIVERSE_ROLES.length; index++) {
      const below = universeActions([UNIVERSE_ROLES[index - 1]!]);
      const role = universeActions([UNIVERSE_ROLES[index]!]);
      for (const action of below) expect(role.has(action), `${UNIVERSE_ROLES[index]} ${action}`).toBe(true);
    }
  });

  it("union the bundles of several held roles and never subtract", () => {
    const held = universeActions(["viewer", "operator"]);
    expect(held).toEqual(universeActions(["operator"]));
    expect(universeActions([])).toEqual(new Set());
  });

  it("let a Member create nothing, a Creator create universes and an Admin do everything in the organization", () => {
    expect([...organizationActions(["member"])]).toEqual([]);
    expect([...organizationActions(["creator"])]).toEqual(["createUniverse"]);
    expect([...organizationActions(["admin"])].sort()).toEqual([...ORGANIZATION_ACTIONS].sort());
    expect(organizationRoleAllows("creator", "manageMembers")).toBe(false);
  });

  it("hold every deployment action for a Platform Admin and none otherwise", () => {
    expect([...deploymentActions(true)].sort()).toEqual([...DEPLOYMENT_ACTIONS].sort());
    expect(deploymentActions(false).size).toBe(0);
  });

  it("pick the highest held role for display", () => {
    expect(highestRole(UNIVERSE_ROLES, ["contributor", "viewer"])).toBe("contributor");
    expect(highestRole(ORGANIZATION_ROLES, [])).toBeUndefined();
  });
});

describe("session lifecycle rules", () => {
  const close = (role: (typeof UNIVERSE_ROLES)[number], creator: boolean, shared: boolean) => canCloseSession(universeActions([role]), creator, shared);
  const remove = (role: (typeof UNIVERSE_ROLES)[number], shared: boolean) => canDeleteSession(universeActions([role]), shared);

  it("let a creator close its own unshared session and leave shared ones to moderators", () => {
    expect(close("viewer", true, false)).toBe(false);
    expect(close("contributor", true, false)).toBe(true);
    expect(close("contributor", true, true)).toBe(false);
    expect(close("contributor", false, false)).toBe(false);
    expect(close("operator", false, true)).toBe(true);
    expect(close("operator", false, false)).toBe(false);
    expect(close("owner", false, false)).toBe(true);
  });

  it("require access to private work to delete an unshared session", () => {
    expect(remove("contributor", true)).toBe(false);
    expect(remove("operator", true)).toBe(true);
    expect(remove("operator", false)).toBe(false);
    expect(remove("owner", false)).toBe(true);
  });
});

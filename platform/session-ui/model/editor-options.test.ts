import { describe, expect, it } from "vitest";
import { attachedEnvironments, defaultEnvironmentAttachment, isEnvironmentAttached, resourceFeatureDisableReasons } from "./resource-features";

describe("environment attachment options", () => {
  const config = { features: { environments: { environments: [
    { environmentId: "primary", access: "jobs", default: true },
    { environmentId: "logs", access: "read" },
  ] } } };
  it("offers only attached environments and resolves the declared default", () => {
    expect(attachedEnvironments(config, [{ environmentId: "logs" }, { environmentId: "unlisted" }])).toEqual([{ environmentId: "logs" }]);
    expect(defaultEnvironmentAttachment(config)?.environmentId).toBe("primary");
    expect(isEnvironmentAttached(config, "unlisted")).toBe(false);
    expect(defaultEnvironmentAttachment({})).toBeUndefined();
  });
  it.each([
    ["environments", "environments", { environmentId: "primary" }],
    ["vfs", "workspaces", { workspaceId: "files" }],
    ["mcp", "servers", { serverId: "catalog" }],
  ])("requires removing %s attachments before disabling the feature", (feature, field, attachment) => {
    expect(resourceFeatureDisableReasons({ config: { features: { [feature]: { [field]: [attachment] } } } })).toHaveProperty(feature);
    expect(resourceFeatureDisableReasons({ config: { features: { [feature]: { [field]: [] } } } })).toEqual({});
  });
});

import { describe, expect, it } from "vitest";
import { GENERATED_TOOLS } from "../src/generated/tools.js";

describe("generated universe tools", () => {
  it("contains the configured 71-method surface and no operator methods", () => {
    expect(GENERATED_TOOLS).toHaveLength(71);
    expect(new Set(GENERATED_TOOLS.map((tool) => tool.name)).size).toBe(71);
    expect(GENERATED_TOOLS.some((tool) => tool.method.startsWith("operator/"))).toBe(false);
    expect(GENERATED_TOOLS.find((tool) => tool.method === "session/config/put")?.name).toBe(
      "lightspeed_session_config_put",
    );
    expect(GENERATED_TOOLS.map((tool) => tool.method)).not.toEqual(
      expect.arrayContaining([
        "initialize",
        "environments/providers/register",
        "environments/providers/heartbeat",
        "environments/providers/unregister",
        "environments/jobs/create",
        "environments/jobs/read",
        "environments/jobs/list",
        "environments/jobs/cancel",
        "outbox/read",
        "outbox/ack",
      ]),
    );
  });

  it("emits self-contained object input schemas", () => {
    for (const tool of GENERATED_TOOLS) {
      expect(tool.summary.trim(), `${tool.name} summary`).not.toBe("");
      expect(tool.description.trim(), `${tool.name} description`).not.toBe("");
      expect(tool.inputSchema.type, tool.name).toBe("object");
      const definitions = isRecord(tool.inputSchema.definitions)
        ? tool.inputSchema.definitions
        : {};
      for (const reference of collectReferences(tool.inputSchema)) {
        const prefix = "#/definitions/";
        expect(reference.startsWith(prefix), `${tool.name}: ${reference}`).toBe(true);
        expect(definitions[reference.slice(prefix.length)], `${tool.name}: ${reference}`).toBeDefined();
      }
    }
  });

  it("carries operational method documentation into MCP descriptors", () => {
    expect(GENERATED_TOOLS.find((tool) => tool.method === "auth/grants/read")).toMatchObject({
      summary: "Read authentication grant metadata",
      description: expect.stringContaining("token values are never returned"),
    });
  });
});

function collectReferences(value: unknown): string[] {
  if (Array.isArray(value)) {
    return value.flatMap(collectReferences);
  }
  if (!isRecord(value)) {
    return [];
  }
  return Object.entries(value).flatMap(([key, entry]) =>
    key === "$ref" && typeof entry === "string" ? [entry] : collectReferences(entry),
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

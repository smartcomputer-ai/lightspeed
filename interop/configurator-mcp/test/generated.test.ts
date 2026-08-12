import { describe, expect, it } from "vitest";
import { GENERATED_TOOLS } from "../src/generated/tools.js";

describe("generated universe tools", () => {
  it("contains the configured 70-method surface and no operator methods", () => {
    expect(GENERATED_TOOLS).toHaveLength(70);
    expect(new Set(GENERATED_TOOLS.map((tool) => tool.name)).size).toBe(70);
    expect(GENERATED_TOOLS.some((tool) => tool.method.startsWith("operator/"))).toBe(false);
    expect(GENERATED_TOOLS.find((tool) => tool.method === "session/config/put")?.name).toBe(
      "lightspeed_session_config_put",
    );
    expect(GENERATED_TOOLS.map((tool) => tool.method)).not.toEqual(
      expect.arrayContaining([
        "initialize",
        "session/managed/start",
        "environments/jobs/create",
        "environments/jobs/read",
        "environments/jobs/cancel",
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

  it("preserves literal boolean defaults while normalizing boolean schemas", () => {
    const tool = GENERATED_TOOLS.find((candidate) => candidate.method === "session/config/put");
    const definitions = isRecord(tool?.inputSchema.definitions)
      ? tool.inputSchema.definitions
      : {};
    const environments = definitions.EnvironmentsFeature;
    expect(environments).toMatchObject({
      additionalProperties: { not: {} },
      properties: {
        jobs: {
          default: false,
          type: "boolean",
        },
      },
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

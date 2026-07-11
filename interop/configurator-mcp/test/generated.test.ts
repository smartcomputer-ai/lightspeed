import { describe, expect, it } from "vitest";
import { GENERATED_TOOLS } from "../src/generated/tools.js";

describe("generated universe tools", () => {
  it("contains all 81 universe methods and no operator methods", () => {
    expect(GENERATED_TOOLS).toHaveLength(81);
    expect(new Set(GENERATED_TOOLS.map((tool) => tool.name)).size).toBe(81);
    expect(GENERATED_TOOLS.some((tool) => tool.method.startsWith("operator/"))).toBe(false);
    expect(GENERATED_TOOLS.find((tool) => tool.method === "session/config/put")?.name).toBe(
      "lightspeed_session_config_put",
    );
  });

  it("emits self-contained object input schemas", () => {
    for (const tool of GENERATED_TOOLS) {
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

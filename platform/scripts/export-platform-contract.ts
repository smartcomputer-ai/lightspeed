/** Native Platform contract: authored once in shared Zod route descriptors. */
import { writeFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { z } from "zod";
import { PLATFORM_OPERATIONS } from "../shared/src/platform-api.js";

const output = fileURLToPath(new URL("../shared/contract/", import.meta.url));
await mkdir(output, { recursive: true });
const definitions: Record<string, unknown> = {};
const operations = Object.entries(PLATFORM_OPERATIONS).map(([operationId, spec]) => {
  const name = operationId.split("/").map(word => word[0]!.toUpperCase() + word.slice(1).replace(/-([a-z])/g, (_, c: string) => c.toUpperCase())).join("");
  for (const [suffix, schema] of [["Params", spec.params], ["Result", spec.result]] as const) {
    const { $schema: _, ...json } = z.toJSONSchema(schema, { io: suffix === "Params" ? "input" : "output", target: "draft-7" });
    definitions[name + suffix] = json;
  }
  return { operationId, method: spec.method, path: spec.path, params: name + "Params", result: name + "Result" };
});
await writeFile(output + "platform.schema.json", JSON.stringify({ $schema: "http://json-schema.org/draft-07/schema#", title: "PlatformApi", type: "object", additionalProperties: false, definitions }, null, 2) + "\n");
await writeFile(output + "http.json", JSON.stringify({ operations }, null, 2) + "\n");

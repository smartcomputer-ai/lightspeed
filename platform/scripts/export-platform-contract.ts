/** Native Platform contract: authored once in shared Zod route descriptors. */
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { z } from "zod";
import { PLATFORM_OPERATIONS, platformConnectionDetailsSchema } from "../shared/src/platform-api.js";

const output = fileURLToPath(new URL("../shared/contract/", import.meta.url));
await mkdir(output, { recursive: true });
const definitions: Record<string, unknown> = {};
const { $schema: _discoveryDialect, ...discoveryDetails } = z.toJSONSchema(platformConnectionDetailsSchema, { io: "output", target: "draft-7" });
definitions.PlatformConnectionDetails = discoveryDetails;
const helpers = new Set<string>();
const runtime = JSON.parse(await readFile(new URL("../../crates/api/contract/http.json", import.meta.url), "utf8")) as { operations: Array<{ operationId: string; method: string; path: string }> };
const operationIds = new Set(runtime.operations.map(operation => operation.operationId));
const routeKey = (method: string, path: string) => `${method} ${path.replace(/\{[^}]+\}/g, "{}")}`;
const routes = new Set(runtime.operations.map(operation => routeKey(operation.method, operation.path)));
const operations = Object.entries(PLATFORM_OPERATIONS).map(([operationId, spec]) => {
  const route = routeKey(spec.method, spec.path);
  if (operationIds.has(operationId) || routes.has(route)) throw new Error(`Conflicting client operation or route: ${operationId}`);
  operationIds.add(operationId);
  routes.add(route);
  if (!/^[a-z][a-zA-Z0-9]*$/.test(spec.helper.name) || helpers.has(spec.helper.name)) {
    throw new Error(`Invalid or duplicate Platform helper for ${operationId}`);
  }
  helpers.add(spec.helper.name);
  const name = operationId.split("/").map(word => word[0]!.toUpperCase() + word.slice(1).replace(/-([a-z])/g, (_, c: string) => c.toUpperCase())).join("");
  for (const [suffix, schema] of [["Params", spec.params], ["Result", spec.result]] as const) {
    const { $schema: _, ...json } = z.toJSONSchema(schema, { io: suffix === "Params" ? "input" : "output", target: "draft-7" });
    definitions[name + suffix] = json;
  }
  return { operationId, helper: spec.helper, method: spec.method, path: spec.path, params: name + "Params", result: name + "Result" };
});
await writeFile(output + "platform.schema.json", JSON.stringify({ $schema: "http://json-schema.org/draft-07/schema#", title: "PlatformApi", type: "object", additionalProperties: false, definitions }, null, 2) + "\n");
await writeFile(output + "http.json", JSON.stringify({ operations }, null, 2) + "\n");

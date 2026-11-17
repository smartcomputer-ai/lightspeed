import { readFile } from "node:fs/promises";
import { expect, it, vi } from "vitest";
import { ALL_HELPERS, HELPERS, MANUAL_HELPERS, validateHelpers } from "../scripts/helpers.mjs";
import { CLIENT_HELPERS, LightspeedClient, OPERATIONS } from "../src/index.js";

const schema = JSON.parse(await readFile(new URL("../schema/api.schema.json", import.meta.url), "utf8"));
const contract = JSON.parse(await readFile(new URL("../schema/http.json", import.meta.url), "utf8"));

it("names every runtime operation exactly once and validates catalog drift", () => {
  expect(Object.values(CLIENT_HELPERS).sort()).toEqual([...OPERATIONS].sort());
  expect(new Set(ALL_HELPERS.map(([id]) => id)).size).toBe(OPERATIONS.length);
  validateHelpers(contract.operations);
  expect(() => validateHelpers(contract.operations.slice(1))).toThrow("unknown operation");
  expect(() => validateHelpers([...contract.operations, { operationId: "new/operation" }])).toThrow("Missing named helpers");
  const client = new LightspeedClient("https://example.test");
  for (const [, name] of MANUAL_HELPERS) expect(client[name], name).toBeTypeOf("function");
});

for (const [operation, helper, ...targets] of HELPERS) {
  it(`${helper} forwards every wire field through the overridable common dispatcher`, async () => {
    const entry = contract.operations.find(item => item.operationId === operation);
    const definition = schema.definitions[entry.params.schema.$ref.split("/").at(-1)];
    const params = Object.fromEntries(Object.keys(definition.properties ?? {}).map(key => [key, targets.includes(key) ? `${key}-target` : { field: key }]));
    const options = Object.fromEntries(Object.entries(params).filter(([key]) => !targets.includes(key)));
    const client = new LightspeedClient("https://example.test");
    const response = { sentinel: true };
    const call = vi.spyOn(client, "call").mockResolvedValue(response);
    const signal = new AbortController().signal;
    const headers = { "x-custom": "value" };
    expect(await client[helper](...targets.map(key => params[key]), { ...options, signal, headers })).toBe(response);
    expect(call).toHaveBeenCalledWith(operation, params, { signal, headers });
  });
}

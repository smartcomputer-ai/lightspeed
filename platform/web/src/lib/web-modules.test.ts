import { expect, it } from "vitest";
import { composeWebModules, type UniverseWebModule } from "./web-modules";

const sample: UniverseWebModule = {
  id: "sample", label: "Sample", icon: () => null, action: "read",
  routes: [{ path: "", Page: () => null }],
};

it("accepts an empty distribution and preserves the order of selected modules", () => {
  expect(composeWebModules([])).toEqual([]);
  const other = { ...sample, id: "other" };
  expect(composeWebModules([{ id: other.id, module: other }, { id: sample.id, module: sample }])).toEqual([other, sample]);
});

it("rejects conflicting modules and paths during assembly", () => {
  const entry = { id: sample.id, module: sample };
  expect(() => composeWebModules([entry, entry])).toThrow(/conflicting/);
  expect(() => composeWebModules([{ ...entry, id: "other" }])).toThrow(/exported sample/);
  expect(() => composeWebModules([{ id: "workspaces", module: { ...sample, id: "workspaces" } }])).toThrow(/conflicting/);
  expect(() => composeWebModules([{ ...entry, module: { ...sample, routes: [] } }])).toThrow(/landing/);
  expect(() => composeWebModules([{ ...entry, module: { ...sample, routes: [...sample.routes, ...sample.routes] } }])).toThrow(/duplicate route/);
});

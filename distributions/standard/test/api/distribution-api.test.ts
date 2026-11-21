import { expect, it, vi } from "vitest";
import { standardApiModules } from "../../api.js";

vi.mock("../../catalog.js", async () => {
  const { standardModules } = await vi.importActual<typeof import("../../catalog.js")>(
    "../../catalog.js",
  );
  return { standardModules: standardModules.filter(module => module.id !== "bots") };
});
vi.mock("../../../../modules/bots/api/index.ts", () => {
  throw new Error("The omitted Bots router was loaded");
});

it("assembles the other API contributions without loading Bots", () => {
  expect(standardApiModules.find(module => module.id === "bots")).toBeUndefined();
  expect(standardApiModules.find(module => module.id === "sessions")?.routes).toBeTypeOf("function");
  expect(standardApiModules.find(module => module.id === "artifacts")?.routes).toBeUndefined();
});

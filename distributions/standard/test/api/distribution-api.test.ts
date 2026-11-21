import { expect, it, vi } from "vitest";
import { standardApiModules } from "../../api.js";

vi.mock("../../catalog.js", async () => {
  const { standardModules } = await vi.importActual<typeof import("../../catalog.js")>(
    "../../catalog.js",
  );
  const artifacts = standardModules.find(module => module.id === "artifacts")!;
  return { standardModules: [
    ...standardModules.filter(module => module.id !== "bots"),
    {
      ...artifacts, id: "mixed", api: { ...artifacts.api, browserRoutes: "@lightspeed-ai/sessions-server/api" },
    },
    { id: "shell-only", name: "Shell only", description: "", defaultEnabled: true },
  ] };
});
vi.mock("../../../../modules/bots/api/index.ts", () => {
  throw new Error("The omitted Bots router was loaded");
});

it("assembles the other API contributions without loading Bots", () => {
  expect(standardApiModules.find(module => module.id === "bots")).toBeUndefined();
  const sessions = standardApiModules.find(module => module.id === "sessions")!;
  const artifacts = standardApiModules.find(module => module.id === "artifacts")!;
  expect(sessions.browserRoutes).toBeTypeOf("function");
  expect(sessions.publicOperations).toBeUndefined();
  expect(artifacts.publicOperations).toBeTypeOf("function");
  expect(artifacts.browserRoutes).toBeUndefined();
  expect(standardApiModules.find(module => module.id === "mixed")).toMatchObject({
    browserRoutes: sessions.browserRoutes, publicOperations: artifacts.publicOperations,
  });
  expect(standardApiModules.find(module => module.id === "shell-only")).toMatchObject({
    browserRoutes: undefined, publicOperations: undefined,
  });
});

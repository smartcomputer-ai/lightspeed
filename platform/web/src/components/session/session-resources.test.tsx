import { expect, it } from "vitest";
import type { Environment } from "@/api";
import { sessionResources } from "@/lib/sessions/session-resources";
import { activeEnvironmentItems, sessionResourceItems } from "./session-resources";

it("lists Code mode as a resource only when code mode is enabled", () => {
  const labels = (features: unknown) =>
    sessionResourceItems({ resources: sessionResources({ features }, null), slug: "demo" }).map((item) => item.label);

  expect(labels({ codeMode: {} })).toContain("Code mode: Code mode");
  expect(labels({})).not.toContain("Code mode: Code mode");
});

it("shows resources on the page, each with its details, and an absence only in the strip", () => {
  const items = sessionResourceItems({ resources: sessionResources({ features: { codeMode: {}, environments: {} } }, null), slug: "demo" });
  const codeMode = items.find((item) => item.label === "Code mode: Code mode")!;
  expect(codeMode.places).toEqual(["header", "strip"]);
  expect(codeMode.details).toBeDefined();
  expect(items.find((item) => item.label === "Environment: No environment")?.places).toEqual(["strip"]);
});

it("shows the active environment everywhere, colored by its status", () => {
  const environments = [{ environmentId: "env-1", displayName: "CI runner", status: "ready" }] as Environment[];
  const [item] = activeEnvironmentItems("env-1", environments);
  expect(item).toMatchObject({ label: "Environment: CI runner", tone: "success" });
  expect(item!.places).toBeUndefined();
  expect(activeEnvironmentItems(null, environments)).toEqual([]);
});

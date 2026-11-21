import { expect, it } from "vitest";
import { withModuleRoutes, type ModuleOperation, type PlatformModule } from "@lightspeed-ai/platform-shared";

const module: PlatformModule = { id: "reports", name: "Reports", description: "", defaultEnabled: true, permissions: { inspect: "read" } };
const read: ModuleOperation = { id: "get", path: "/:recordId", method: "GET", permission: "inspect" };
function assemble(...operations: ModuleOperation[]) { return withModuleRoutes([{ ...module, operations }], {}); }

it("accepts resource routes, distinct methods and specific child resources", () => {
  expect(assemble(read, { ...read, id: "update", method: "PUT" }, { ...read, id: "content", path: "/:recordId/content" }, { ...read, id: "recent", path: "/recent" })).toHaveLength(1);
});

it("requires an existing local permission and a unique operation ID", () => {
  expect(() => assemble({ ...read, permission: "unregistered" })).toThrow("undeclared permission");
  expect(() => assemble(read, { ...read, path: "/" })).toThrow("duplicate operation ID");
  expect(() => assemble({ ...read, permission: "toString" })).toThrow("undeclared permission");
});

it("rejects two operations with the same method and path shape", () => {
  expect(() => assemble(read, { ...read, id: "other", path: "/:otherId" })).toThrow("Ambiguous module operation path");
});

it("rejects unsupported HTTP methods in a JavaScript module descriptor", () => {
  // @ts-expect-error JavaScript contributions are not type checked by the host.
  expect(() => assemble({ ...read, method: "OPTIONS" })).toThrow("Invalid module operation method");
});

it.each(["/:id", "/*", "/:uuid?", "/:recordId/:recordId", "foo", "/../secret"])("rejects an unsafe or ambiguous parameter in %s", path => {
  expect(() => assemble({ ...read, path })).toThrow("Invalid module operation path");
});

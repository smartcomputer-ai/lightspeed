// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Member } from "@/api";
import { PrivateContentAccess } from "./private-content-access";
import { PrivilegedReadMarker } from "./privileged-read";
const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", () => ({ api: mocks.api }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
const person: Member = {
  id: "principal:person:viewer",
  subject: { kind: "principal", id: "person" },
  principalKind: "user",
  userId: "account",
  name: "Reader",
  email: "",
  role: "viewer",
  createdAt: "",
};
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.api.mockReset().mockImplementation(async (_method, _url, body) => body);
  client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  container = document.createElement("div");
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  vi.unstubAllGlobals();
});
async function render(member: Member) {
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <PrivateContentAccess
          universeId="universe"
          member={member}
          onDone={async () => {}}
        />
      </QueryClientProvider>,
    ),
  );
}
async function click() {
  await act(async () => {
    container.querySelector("button")!.click();
    await new Promise((resolve) => setTimeout(resolve, 20));
  });
}
it("grants and revokes independently of the member's role", async () => {
  await render(person);
  await click();
  expect(mocks.api).toHaveBeenLastCalledWith(
    "PUT",
    expect.stringContaining("/private-content-access"),
    { enabled: true },
  );
  expect(container.textContent).toContain("Remove private-content access");
  await click();
  expect(mocks.api).toHaveBeenLastCalledWith("PUT", expect.any(String), {
    enabled: false,
  });
  expect(container.textContent).toContain("Allow private-content reads");
});
it.each([
  { ...person, principalKind: "service" as const },
  { ...person, subject: { kind: "group" as const, id: "team" } },
])("does not offer the capability to groups or services", async (member) => {
  await render(member);
  expect(container.querySelector("button")).toBeNull();
});
it("preserves the current state when the capability change fails", async () => {
  mocks.api.mockRejectedValue(new Error("Capability change refused"));
  await render(person);
  await click();
  expect(container.textContent).toContain("Allow private-content reads");
  expect(container.querySelector('[role="alert"]')?.textContent).toBe(
    "Capability change refused",
  );
});
it("marks only privileged reads and explains their audit trail", async () => {
  await act(async () =>
    root.render(<PrivilegedReadMarker privileged={false} />),
  );
  expect(container.textContent).toBe("");
  await act(async () => root.render(<PrivilegedReadMarker privileged />));
  expect(
    container.querySelector('[role="note"]')?.getAttribute("title"),
  ).toContain("recorded in the access audit");
});

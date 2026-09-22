// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  AccessPolicyView,
  ResourceRef,
} from "@lightspeed-ai/agent-client";
import { ApiError } from "@/api";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { AccessButton } from "./access-dialog";
import { CreationAccessFields, creationAccessInput } from "./creation";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({
  ...(await original<typeof import("@/api")>()),
  api: mocks.api,
}));
// Exercise access decisions without depending on popover geometry in jsdom.
vi.mock("./shared", async (original) => {
  const actual = await original<typeof import("./shared")>();
  return {
    ...actual,
    AccessSelect: ({
      label,
      value,
      options,
      onChange,
      disabled,
    }: Parameters<typeof actual.AccessSelect>[0]) => (
      <select
        aria-label={label}
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
    ),
  };
});
const rootResource: ResourceRef = { kind: "collection", id: "research" };
const child: ResourceRef = { kind: "session", id: "child" };
let policy: AccessPolicyView;
let actor: string;
let writable: boolean;
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  actor = "owner";
  writable = true;
  policy = {
    resource: child,
    root: rootResource,
    owner: "owner",
    visibility: "restricted",
    execution: { kind: "service", runAs: "service" },
    revision: 7,
    grants: [
      {
        subject: { kind: "principal", id: "writer" },
        permission: "write",
        grantedBy: "owner",
        grantedAtMs: 0,
      },
    ],
    updatedAtMs: 0,
    updatedBy: { kind: "principal", id: "owner" },
  };
  mocks.api
    .mockReset()
    .mockImplementation(
      async (
        method: string,
        path: string,
        body?: { resources?: ResourceRef[] },
      ) => {
        if (path.endsWith("/policy/read")) return { policy };
        if (path.endsWith("/access"))
          return {
            actions: ["read"],
            resources: (body?.resources ?? []).map((resource) => ({
              resource,
              actions: writable ? ["read", "share_resource"] : ["read"],
            })),
          };
        if (path.includes("/subjects"))
          return {
            principalId: actor,
            subjects: ["owner", "reader", "writer"].map((id) => ({
              subject: { kind: "principal", id },
              displayName: id,
            })),
          };
        if (method === "PUT")
          throw new ApiError(409, { error: "revision conflict" });
        throw new Error(path);
      },
    );
  client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
async function settle() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10);
  });
}
async function show() {
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <PermissionIdentityProvider userId={actor}>
          <MemoryRouter>
            <AccessButton universeId="universe" slug="test" resource={child} />
          </MemoryRouter>
        </PermissionIdentityProvider>
      </QueryClientProvider>,
    ),
  );
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[aria-label="Access and sharing"]')!
      .click(),
  );
  await settle();
  await settle();
}
function button(label: string) {
  return [...document.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent === label,
  );
}
it("shows inherited access to readers without offering sharing mutations", async () => {
  actor = "reader";
  writable = false;
  await show();
  expect(document.body.textContent).toContain(
    "Shared through collection research",
  );
  expect(
    document.querySelector<HTMLAnchorElement>(
      'a[href="/u/test/collections/research"]',
    ),
  ).not.toBeNull();
  expect(document.body.textContent).toContain("Running as universe service");
  expect(document.querySelector('[id="share-search"]')).toBeNull();
  expect(button("Save")).toBeUndefined();
});
it("writers can add readers but cannot change existing writers or transfer ownership", async () => {
  actor = "writer";
  await show();
  expect(document.querySelector('[aria-label="Remove writer"]')).toBeNull();
  expect(
    document.querySelector('[aria-label="Permission for writer"]'),
  ).toBeNull();
  expect(document.body.textContent).not.toContain("Transfer ownership");
  await act(async () => button("readerAdd")!.click());
  await act(async () => button("Save")!.click());
  await settle();
  const put = mocks.api.mock.calls.find(([method]) => method === "PUT");
  expect(put?.[2]).toEqual({
    resource: rootResource,
    visibility: "restricted",
    expectedRevision: 7,
    grants: [
      { subject: { kind: "principal", id: "writer" }, permission: "write" },
      { subject: { kind: "principal", id: "reader" }, permission: "read" },
    ],
  });
  expect(document.body.textContent).toContain(
    "Access changed while you were editing",
  );
  expect(button("Save")?.disabled).toBe(true);
});
it("personal roots offer read-only sharing and no ownership transfer", async () => {
  policy.execution = { kind: "personal", runAs: "owner" };
  await show();
  expect(
    document.querySelector('[aria-label="Permission for writer"]'),
  ).toBeNull();
  expect(document.body.textContent).toContain(
    "Personal work is shared as read-only here.",
  );
  expect(document.body.textContent).not.toContain("Transfer ownership");
});
it("collection creation inputs omit independent execution and grants", () => {
  expect(
    creationAccessInput({
      collectionId: "team",
      kind: "personal",
      visibility: "restricted",
    }),
  ).toEqual({ access: { root: { kind: "collection", id: "team" } } });
  expect(
    creationAccessInput({
      collectionId: "",
      kind: "personal",
      visibility: "restricted",
    }),
  ).toEqual({
    access: { visibility: "restricted" },
    execution: { kind: "personal" },
  });
});

async function showCreation(collectionId = "") {
  vi.useRealTimers();
  mocks.api.mockImplementation(
    async (
      _method: string,
      path: string,
      body?: { resources?: ResourceRef[] },
    ) => {
      if (path.endsWith("/execution"))
        return { policy: { personalExecutionEnabled: false } };
      if (path.endsWith("/collections"))
        return {
          collections: [
            {
              collectionId: "team",
              displayName: "Team research",
              access: {
                visibility: "restricted",
                execution: { kind: "personal" },
              },
            },
          ],
        };
      if (path.endsWith("/access"))
        return {
          actions: ["read"],
          resources: (body?.resources ?? []).map((resource) => ({
            resource,
            actions: ["read", "control_session"],
          })),
        };
      throw new Error(path);
    },
  );
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <PermissionIdentityProvider userId={actor}>
          <MemoryRouter>
            <CreationAccessFields
              universeId="universe"
              value={{ collectionId, kind: "service", visibility: "universe" }}
              onChange={() => {}}
            />
          </MemoryRouter>
        </PermissionIdentityProvider>
      </QueryClientProvider>,
    ),
  );
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 50));
  });
}
it("shows collection inheritance without independent visibility or execution controls", async () => {
  await showCreation("team");
  expect(document.body.textContent).toContain(
    "Access and execution come from Team research",
  );
  expect(document.body.textContent).toContain(
    "Restricted access; runs as the collection owner",
  );
  expect(document.querySelector('[aria-label="Running as"]')).toBeNull();
  expect(document.querySelector('[aria-label="Who can read"]')).toBeNull();
});
it("offers only universe service while personal execution is disabled", async () => {
  await showCreation();
  const options = [
    ...document.querySelectorAll('[aria-label="Running as"] option'),
  ].map((option) => option.textContent);
  expect(options).toContain("Universe service");
  expect(options).not.toContain("Me");
});

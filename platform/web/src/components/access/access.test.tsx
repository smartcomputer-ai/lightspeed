// @vitest-environment jsdom
import { act, useState } from "react";
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
import { CreationAccessFields, creationAccessInput, defaultCreationAccess } from "./creation";

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
const rootResource: ResourceRef = { kind: "bot", id: "research" };
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
it.each(["bot", "session"] as const)("shows inherited %s access to readers without offering sharing mutations", async (kind) => {
  policy.root = { kind, id: "research" };
  actor = "reader";
  writable = false;
  await show();
  expect(document.body.textContent).toContain(
    `Shared through ${kind} research`,
  );
  expect(
    document.querySelector<HTMLAnchorElement>(
      `a[href="/u/test/${kind === "bot" ? "bots" : "sessions"}/research"]`,
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
it("edits a standalone session's own policy", async () => {
  policy.root = child;
  await show();
  expect(document.body.textContent).not.toContain("Shared through");
  await act(async () => button("readerAdd")!.click());
  await act(async () => button("Save")!.click());
  await settle();
  const put = mocks.api.mock.calls.find(([method]) => method === "PUT");
  expect(put?.[2].resource).toEqual(child);
});

it.each([
  ["service", "universe"],
  ["personal", "restricted"],
] as const)("creates standalone %s work with its own audience", (kind, visibility) => {
  expect(creationAccessInput({ kind, visibility })).toEqual({
    access: { visibility },
    execution: { kind },
  });
});

async function showCreation(personalExecutionEnabled = false) {
  mocks.api.mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/execution"))
      return { policy: { personalExecutionEnabled } };
    throw new Error(path);
  });
  function CreationForm() {
    const [value, onChange] = useState(defaultCreationAccess);
    return (
      <CreationAccessFields universeId="universe" value={value} onChange={onChange} />
    );
  }
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <PermissionIdentityProvider userId={actor}>
          <MemoryRouter>
            <CreationForm />
          </MemoryRouter>
        </PermissionIdentityProvider>
      </QueryClientProvider>,
    ),
  );
  await settle();
}
it("offers direct access settings without collection discovery", async () => {
  await showCreation();
  expect(document.querySelector('[aria-label="Collection"]')).toBeNull();
  expect(document.querySelector('[aria-label="Running as"]')).not.toBeNull();
  expect(document.querySelector('[aria-label="Who can read"]')).not.toBeNull();
  expect(mocks.api.mock.calls.some(([, path]) => path.includes("/collections"))).toBe(false);
});
it("offers only universe service while personal execution is disabled", async () => {
  await showCreation();
  const options = [
    ...document.querySelectorAll('[aria-label="Running as"] option'),
  ].map((option) => option.textContent);
  expect(options).toContain("Universe service");
  expect(options).not.toContain("Me");
});
it("defaults personal execution to restricted access", async () => {
  await showCreation(true);
  const execution = document.querySelector<HTMLSelectElement>('[aria-label="Running as"]')!;
  expect([...execution.options].map((option) => option.textContent)).toContain("Me");
  await act(async () => {
    execution.value = "personal";
    execution.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(document.querySelector<HTMLSelectElement>('[aria-label="Who can read"]')!.value).toBe("restricted");
});

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
import { CreationAccessSummary, creationAccessInput, defaultCreationAccess } from "./creation";

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
let resource: ResourceRef;
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  actor = "owner";
  writable = true;
  resource = child;
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
            subjects: [
              ...["owner", "reader", "writer"].map((id) => ({
                subject: { kind: "principal", id },
                displayName: id,
              })),
              {
                subject: { kind: "principal", id: "agent" },
                displayName: "Default agent identity",
              },
            ],
          };
        if (path.endsWith("/execution"))
          return {
            policy: {
              executionPrincipalId: "agent",
              personalExecutionEnabled: false,
            },
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
            <AccessButton universeId="universe" slug="test" resource={resource} />
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
  expect(document.body.textContent).toContain("Running as Default agent identity");
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

function operational(kind: "workspace" | "environment" | "mcp_server") {
  resource = { kind, id: "production" };
  policy = {
    resource,
    root: resource,
    owner: "owner",
    visibility: "restricted",
    revision: 3,
    grants: [
      {
        subject: { kind: "principal", id: "writer" },
        permission: "use",
        grantedBy: "owner",
        grantedAtMs: 0,
      },
    ],
    updatedAtMs: 0,
    updatedBy: { kind: "principal", id: "owner" },
  };
}
it.each([
  ["workspace", "read and change its files"],
  ["environment", "files, commands, jobs and the credentials bound to it"],
  ["mcp_server", "call its allowed tools"],
] as const)("shares a %s with use as its only permission", async (kind, covers) => {
  operational(kind);
  await show();
  expect(document.body.textContent).toContain("Who can use");
  expect(document.body.textContent).toContain(covers);
  expect(document.body.textContent).toContain(
    "Configuring it stays with its owner, Operators and Admins.",
  );
  expect(document.body.textContent).not.toContain("Running as");
  // No read/control choice: every grant reads "Can use".
  expect(document.querySelector('[aria-label="Permission for writer"]')).toBeNull();
  expect(document.body.textContent).toContain("Can use");
  await act(async () => button("readerAdd")!.click());
  await act(async () => button("Save")!.click());
  await settle();
  const put = mocks.api.mock.calls.find(([method]) => method === "PUT");
  expect(put?.[2]).toEqual({
    resource,
    visibility: "restricted",
    expectedRevision: 3,
    grants: [
      { subject: { kind: "principal", id: "writer" }, permission: "use" },
      { subject: { kind: "principal", id: "reader" }, permission: "use" },
    ],
  });
});
it("explains what granting the default agent identity means", async () => {
  operational("environment");
  const sentence =
    "Every session and bot running as Default agent identity can use this.";
  await show();
  expect(document.body.textContent).not.toContain(sentence);
  await act(async () => button("Default agent identityAdd")!.click());
  expect(document.body.textContent).toContain(sentence);
  expect(document.body.textContent).toContain("Agent identity");
  await act(async () =>
    document
      .querySelector<HTMLButtonElement>(
        '[aria-label="Remove Default agent identity"]',
      )!
      .click(),
  );
  expect(document.body.textContent).not.toContain(sentence);
  const visibility = document.querySelector<HTMLSelectElement>(
    '[aria-label="Who can use"]',
  )!;
  await act(async () => {
    visibility.value = "universe";
    visibility.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(document.body.textContent).toContain(sentence);
});
it("never offers the default agent identity as a new owner", async () => {
  operational("workspace");
  await show();
  const owners = [
    ...document.querySelectorAll('[aria-label="New owner"] option'),
  ].map((option) => option.textContent);
  expect(owners).toContain("reader");
  expect(owners).not.toContain("Default agent identity");
});
it("never offers the default agent identity as a session grantee", async () => {
  await show();
  expect(button("readerAdd")).toBeTruthy();
  expect(button("Default agent identityAdd")).toBeFalsy();
});

async function chooseVisibility(label: string, value: string) {
  const select = document.querySelector<HTMLSelectElement>(`[aria-label="${label}"]`)!;
  await act(async () => {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
function putBody() {
  return mocks.api.mock.calls.find(([method]) => method === "PUT")?.[2];
}
it("lists nobody on a universe-visible resource but keeps its grants", async () => {
  operational("workspace");
  await show();
  await chooseVisibility("Who can use", "universe");
  expect(document.body.textContent).toContain("Everyone in the universe can use this.");
  expect(document.querySelector("#share-search")).toBeNull();
  expect(document.querySelector('[aria-label="Remove writer"]')).toBeNull();
  await act(async () => button("Save")!.click());
  await settle();
  expect(putBody()).toMatchObject({
    visibility: "universe",
    grants: [{ subject: { kind: "principal", id: "writer" }, permission: "use" }],
  });
});
it("lets the owner of a universe-visible session add people to control it", async () => {
  policy.root = child;
  policy.visibility = "universe";
  await show();
  expect(document.body.textContent).toContain(
    "Everyone in the universe can already read this. Add people who should also be able to control it.",
  );
  await act(async () => button("readerAdd")!.click());
  await act(async () => button("Save")!.click());
  await settle();
  expect(putBody()?.grants).toContainEqual({
    subject: { kind: "principal", id: "reader" },
    permission: "write",
  });
});
it("offers a writer nothing to add on a universe-visible session", async () => {
  policy.root = child;
  policy.visibility = "universe";
  actor = "writer";
  await show();
  expect(document.body.textContent).toContain("Everyone in the universe can already read this.");
  expect(document.body.textContent).not.toContain("Add people who should also");
  expect(document.querySelector("#share-search")).toBeNull();
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
      <CreationAccessSummary audience="read" universeId="universe" value={value} onChange={onChange} />
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
async function changeAccess() {
  const change = document.querySelector<HTMLButtonElement>(
    '[aria-label="Change access and identity"]',
  )!;
  expect(change.textContent).toBe("Change");
  await act(async () => change.click());
}
it("summarizes a new root's default access and identity in one closed line", async () => {
  await showCreation();
  expect(container.textContent).toBe(
    "Universe members can read it · Runs as Default agent identity·Change",
  );
  expect(document.querySelector('[aria-label="Running as"]')).toBeNull();
  expect(document.querySelector('[aria-label="Who can read"]')).toBeNull();
  await changeAccess();
  expect(document.querySelector('[aria-label="Who can read"]')).not.toBeNull();
});
it("shows the default agent identity as text while personal execution is disabled", async () => {
  await showCreation();
  await changeAccess();
  expect(document.querySelector('[aria-label="Running as"]')).toBeNull();
  expect(container.textContent).toContain("Running asDefault agent identity");
});
it("defaults personal execution to restricted access", async () => {
  await showCreation(true);
  await changeAccess();
  const execution = document.querySelector<HTMLSelectElement>('[aria-label="Running as"]')!;
  expect([...execution.options].map((option) => option.textContent)).toEqual([
    "Default agent identity",
    "Me",
  ]);
  await act(async () => {
    execution.value = "personal";
    execution.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(document.querySelector<HTMLSelectElement>('[aria-label="Who can read"]')!.value).toBe("restricted");
  expect(container.textContent).toContain(
    "Only you and people you share with can read it · Runs as you·Hide",
  );
});
it("summarizes who can use a new workspace, environment or MCP server", async () => {
  function UseForm() {
    const [value, onChange] = useState<"universe" | "restricted">("universe");
    return <CreationAccessSummary audience="use" value={value} onChange={onChange} />;
  }
  await act(async () => root.render(<UseForm />));
  expect(container.textContent).toBe("Universe members can use it·Change");
  await act(async () =>
    document.querySelector<HTMLButtonElement>('[aria-label="Change who can use it"]')!.click(),
  );
  const visibility = document.querySelector<HTMLSelectElement>('[aria-label="Who can use"]')!;
  await act(async () => {
    visibility.value = "restricted";
    visibility.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(container.textContent).toContain(
    "Only you and people you share with can use it·Hide",
  );
});

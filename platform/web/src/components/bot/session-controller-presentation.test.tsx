// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ControllerPresentationProps } from "@session-ui/controller-presentation";
import { settleQueries } from "@/test/settle-queries";
import { BotSessionControllerPresentation } from "./session-controller-presentation";

const mocks = vi.hoisted(() => ({ api: vi.fn(), botsEnabled: true, mayRead: true }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useFeature: () => mocks.botsEnabled }));
vi.mock("@/lib/permissions", () => ({ useActionPermissions: () => ({ can: () => mocks.mayRead }) }));

type ManagedSession = NonNullable<ControllerPresentationProps["session"]>;
const managed: ManagedSession = {
  id: "session 1",
  metadata: { bot: "support" },
  integration: { version: 1, lifecycleController: { workflowKind: "BotControllerWorkflow", workflowId: "u/bot-support" } },
};

let container: HTMLDivElement;
let root: Root;
let client: QueryClient;

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.botsEnabled = mocks.mayRead = true;
  mocks.api.mockReset().mockResolvedValue({ bots: [{ botId: "support", displayName: "Support triage" }] });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div");
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

async function show(session: ManagedSession) {
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <BotSessionControllerPresentation handle="acme" universeId="universe" session={session}>
        {({ attribution, label, open }) => open
          ? <a href={open.href}>{attribution?.label}</a>
          : <span>{attribution?.label ?? label}</span>}
      </BotSessionControllerPresentation>
    </QueryClientProvider>,
  ));
  await settleQueries(client);
}

it("resolves the bot name and a destination for a readable managed session", async () => {
  await show(managed);
  expect(mocks.api).toHaveBeenCalledWith("GET", "/api/ui/universes/universe/bots");
  expect(container.textContent).toBe("Support triage");
  expect(container.querySelector("a")?.getAttribute("href")).toBe("/u/acme/bots/support/chat/session%201");
});

it.each(["disabled", "unreadable"])("keeps the recorded bot identity when its pages are %s", async (state) => {
  mocks.botsEnabled = state !== "disabled";
  mocks.mayRead = state !== "unreadable";
  await show(managed);
  expect(container.textContent).toBe("support");
  expect(container.querySelector("a")).toBeNull();
  expect(mocks.api).not.toHaveBeenCalled();
});

it("keeps a conversation attributable after its bot is deleted", async () => {
  mocks.api.mockResolvedValue({ bots: [] });
  await show(managed);
  expect(container.textContent).toBe("support");
  expect(container.querySelector("a")).toBeNull();
});

it("leaves another workflow's label intact without querying Bots", async () => {
  await show({ ...managed, integration: {
    version: 1, lifecycleController: { workflowKind: "CustomerWorkflow", workflowId: "u/controller" },
  } });
  expect(container.textContent).toBe("CustomerWorkflow");
  expect(mocks.api).not.toHaveBeenCalled();
});

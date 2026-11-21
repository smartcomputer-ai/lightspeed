// @vitest-environment jsdom
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SessionDetail } from "@lightspeed-ai/session-components";
import { BotChat } from "./chat";

vi.mock("@/lib/permissions", () => ({ useActionPermissions: () => ({ can: () => true }) }));
vi.mock("@lightspeed-ai/session-components", async original => ({
  ...await original<typeof import("@lightspeed-ai/session-components")>(),
  SessionDetail: ({ sessionId, sessionHref, embedded }: ComponentProps<typeof SessionDetail>) => (
    <a data-embedded={embedded} href={sessionHref(sessionId)}>{sessionId}</a>
  ),
}));

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  root = createRoot(container);
});
afterEach(async () => { await act(async () => root.unmount()); vi.unstubAllGlobals(); });

it.each([
  [undefined, "main", "/u/acme/bots/support"],
  ["thread/1", "thread/1", "/u/acme/bots/support/chat/thread%2F1"],
] as const)("opens %s with the shared session view and bot-local navigation", async (sessionId, id, path) => {
  await act(async () => root.render(
    <MemoryRouter>
      <BotChat universeId="universe" handle="acme" sessionId={sessionId}
        bot={{ botId: "support", profileId: "profile", eventSeq: 0, revision: 1, updatedAtMs: 0, createdAtMs: 0 }}
        state={{ controller: { mainSessionId: "main", setupStatus: "ready", controllerStatus: "idle", enabled: true, closed: false } }}
      />
    </MemoryRouter>,
  ));
  const view = container.querySelector("a");
  expect(view?.textContent).toBe(id);
  expect(view?.dataset.embedded).toBe("true");
  expect(view?.getAttribute("href")).toBe(path);
});

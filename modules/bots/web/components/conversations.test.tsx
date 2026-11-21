// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { BotStateView } from "@lightspeed-ai/sdk";
import { ConversationSwitcher } from "./conversations";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

/// Main, one thread, and a sub-agent the thread delegated to.
const state = {
  controller: {
    mainSessionId: "main", controllerStatus: "idle", setupStatus: "ready", enabled: true, closed: false, activeDeliveries: [],
    sessions: [
      { sessionId: "main", label: "main", kind: "main", busy: false, generation: 1 },
      { sessionId: "thread", label: "PR-2", kind: "perKey", busy: false, generation: 1, lastActiveAtMs: 2 },
    ],
  },
  sessions: [{
    id: "sub", displayName: "Reviewer", lifecycleStatus: "closed", activity: "idle", createdAtMs: 1, updatedAtMs: 1,
    lifecycleControlled: true, retention: { rootSessionId: "main" }, access: { visibility: "universe" },
    origin: {
      kind: "subagent", parentSessionId: "thread", parentRunId: "run", rootSessionId: "main", depth: 1, invocationId: "inv",
      agent: { profileId: "reviewer", revision: 1 }, limits: { maxDepth: 1, maxConcurrent: 1, runTimeoutMs: 1 },
    },
  }],
} as unknown as BotStateView;

const href = (id: string) => `/u/demo/bots/triage/chat/${id}`;

it("names the open conversation with its kind and a caret, and lists Main and the threads to switch to", async () => {
  await act(async () => root.render(
    <MemoryRouter><ConversationSwitcher state={state} currentId="thread" href={href} bot="Triage" /></MemoryRouter>,
  ));
  const trigger = container.querySelector("button")!;
  expect(trigger.textContent).toBe("PR-2");
  expect(trigger.getAttribute("aria-label")).toBe("Thread: PR-2. Switch conversation");
  expect(trigger.querySelector("[data-icon-role=SessionSubThreadIcon]")).not.toBeNull();
  expect(trigger.querySelector("[data-icon-role=DropdownCaretIcon]")).not.toBeNull();

  await act(async () => trigger.click());
  const rows = [...document.body.querySelectorAll<HTMLAnchorElement>('ul[aria-label="Conversations"] a')];
  // Sub-agents are reached from their thread, not listed here.
  expect(rows.map((row) => row.getAttribute("href"))).toEqual([href("main"), href("thread")]);
  expect(rows.map((row) => row.getAttribute("aria-current"))).toEqual([null, "page"]);
  // The popover points back at the switcher.
  expect(document.body.querySelector('[data-slot="popover-notch"]')).not.toBeNull();

  await act(async () => rows[0]!.click());
  expect(document.body.querySelector('ul[aria-label="Conversations"]')).toBeNull();
});

it("names an open sub-agent, though the popover lists only Main and the threads", async () => {
  await act(async () => root.render(
    <MemoryRouter><ConversationSwitcher state={state} currentId="sub" href={href} bot="Triage" /></MemoryRouter>,
  ));
  const trigger = container.querySelector("button")!;
  expect(trigger.textContent).toBe("Reviewer");
  expect(trigger.querySelector("[data-icon-role=SubAgentIcon]")).not.toBeNull();
});

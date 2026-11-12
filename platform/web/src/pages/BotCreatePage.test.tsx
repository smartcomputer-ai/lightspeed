// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { BotCreateDialog } from "./BotCreatePage";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({ api: vi.fn(), role: "operator" }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", async (original) => ({
  ...await original<typeof import("@/lib/universes")>(),
  useActiveUniverse: () => ({ universe: { id: "universe", role: mocks.role }, handle: "factory", isLoading: false }),
  useFeature: () => true,
}));
vi.mock("@/lib/sessions/editor-options", () => ({ useSessionConfigEditorOptions: () => ({}) }));
vi.mock("@/components/session/session-config-editor", () => ({ SessionConfigEditor: () => <div data-testid="config-editor" /> }));
vi.mock("@/components/provider-readiness-banner", () => ({ ProviderReadinessBanner: () => null }));
// JSDOM has no popup layout; each select becomes a native control named by its trigger id.
vi.mock("@/components/ui/select", async () => {
  const React = await import("react");
  function SelectTrigger(_props: { id?: string; children?: ReactNode }) {
    return null;
  }
  function Select({ value, onValueChange, children }: { value: string; onValueChange: (value: string) => void; children: ReactNode }) {
    const options: { value: string; label: ReactNode }[] = [];
    let id: string | undefined;
    const visit = (nodes: ReactNode) => React.Children.forEach(nodes, (child) => {
      if (!React.isValidElement(child)) return;
      const props = child.props as { value?: string; children?: ReactNode; id?: string };
      if (child.type === SelectTrigger) id = props.id;
      else if (props.value !== undefined) options.push({ value: props.value, label: props.children });
      else if (props.children) visit(props.children);
    });
    visit(children);
    return <select id={id} data-testid="select" value={value} onChange={(event) => onValueChange(event.target.value)}>
      <option value="" />
      {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
    </select>;
  }
  const Part = () => null;
  return { Select, SelectTrigger, SelectContent: ({ children }: { children?: ReactNode }) => <>{children}</>, SelectItem: Part, SelectValue: Part };
});

let bots: Array<{ botId: string }>;
let profiles: Array<{ profileId: string; displayName?: string; revision: number }>;
let failCreate: boolean;
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("PointerEvent", MouseEvent);
  mocks.role = "operator";
  bots = [{ botId: "taken" }];
  profiles = [{ profileId: "shared-support", displayName: "Shared support", revision: 1 }, { profileId: "claimed", revision: 1 }];
  failCreate = false;
  mocks.api.mockReset().mockImplementation(async (method: string, path: string, body?: { bot?: { botId: string } }) => {
    if (method === "GET" && path === "/api/v1/universes/universe/bots") return { bots };
    if (method === "GET" && path === "/api/v1/universes/universe/profiles") return profiles;
    if (method === "GET" && path.startsWith("/api/v1/universes/universe/profiles/")) return { profileId: path.split("/").pop(), revision: 1 };
    if (method === "PUT" && path.startsWith("/api/v1/universes/universe/profiles/")) return {};
    if (method === "DELETE" && path.startsWith("/api/v1/universes/universe/profiles/")) return {};
    if (method === "POST" && path === "/api/v1/universes/universe/bots") {
      if (failCreate) throw new Error("bot quota reached");
      return { bot: { botId: body!.bot!.botId } };
    }
    throw new Error(`Unexpected request: ${method} ${path}`);
  });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  vi.unstubAllGlobals();
});

function Location() {
  const location = useLocation();
  return <p data-testid="location">{location.pathname}{location.state?.introduce ? " (introduce)" : ""}</p>;
}
async function show() {
  await act(async () => root.render(
    <QueryClientProvider client={client}><PermissionIdentityProvider userId="user" platformAdmin={false}><MemoryRouter initialEntries={["/u/factory/bots"]}>
      <Routes>
        <Route path="/u/:handle/bots" element={<BotCreateDialog universeId="universe" handle="factory" open onOpenChange={() => undefined} />} />
        <Route path="*" element={<Location />} />
      </Routes>
    </MemoryRouter></PermissionIdentityProvider></QueryClientProvider>,
  ));
  if (mocks.role !== "viewer" && mocks.role !== "contributor") {
    await waitForUi(() => expect(dialog()?.textContent).toContain("What is this bot's job?"));
  }
}
const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]');
const input = (id: string) => document.querySelector<HTMLInputElement | HTMLTextAreaElement>(`#${id}`)!;
const buttons = () => [...dialog()!.querySelectorAll<HTMLButtonElement>("button")];
const template = (name: string) => buttons().find((node) => node.getAttribute("aria-pressed") !== null && node.textContent?.includes(name))!;
const step = (label: string) => [...dialog()!.querySelectorAll<HTMLButtonElement>('[aria-label="Bot creation progress"] button')].find((node) => node.textContent?.endsWith(label))!;
const createButton = () => buttons().find((node) => node.textContent?.trim() === "Create bot");
const location = () => container.querySelector('[data-testid="location"]')?.textContent;
async function click(node: HTMLElement | undefined) {
  expect(node).toBeTruthy();
  await act(async () => node!.click());
}
async function type(id: string, value: string) {
  const node = input(id);
  const prototype = node instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  await act(async () => {
    Object.getOwnPropertyDescriptor(prototype, "value")!.set!.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const calls = (method: string) => mocks.api.mock.calls.filter(([verb]) => verb === method);

it.each(["viewer", "contributor"])("offers no bot creation to a %s", async (role) => {
  mocks.role = role;
  await show();
  expect(dialog()).toBeNull();
});

it("fills a template's suggested name and replaces it with the next template's, but keeps a typed name", async () => {
  await show();
  await click(template("Pull-request reviewer"));
  expect(template("Pull-request reviewer").getAttribute("aria-pressed")).toBe("true");
  expect(template("Pull-request reviewer").textContent).toContain("GitHub events");
  expect([input("new-bot-name").value, input("new-bot-id").value]).toEqual(["Reviewer", "reviewer"]);
  expect(input("new-bot-brief").value).toContain("You review pull requests");

  await click(template("Daily digest"));
  expect([input("new-bot-name").value, input("new-bot-id").value]).toEqual(["Digest", "digest"]);
  await click(template("Blank"));
  expect([input("new-bot-name").value, input("new-bot-id").value, input("new-bot-brief").value]).toEqual(["", "", ""]);

  await type("new-bot-name", "Morning Paper");
  expect(input("new-bot-id").value).toBe("morning-paper");
  await click(template("Daily digest"));
  expect([input("new-bot-name").value, input("new-bot-id").value]).toEqual(["Morning Paper", "morning-paper"]);
});

it("keeps an id the person typed when the name changes", async () => {
  await show();
  await type("new-bot-id", "ops");
  await type("new-bot-name", "Operations Desk");
  expect(input("new-bot-id").value).toBe("ops");
});

it("creates the bot's own profile, then the bot with the template's triggers, and opens it", async () => {
  await show();
  await click(template("Daily digest"));
  await type("new-bot-description", " Writes the morning digest. ");
  await click(step("Guardrails"));
  expect(createButton()!.disabled).toBe(false);
  await click(createButton());
  await waitForUi(() => expect(location()).toBe("/u/factory/bots/digest (introduce)"));

  expect(calls("PUT")).toEqual([[
    "PUT", "/api/v1/universes/universe/profiles/digest",
    { profileId: "digest", displayName: "Digest", description: "Setup of bot digest", config: { features: { web: { fetch: {}, search: {} } } } },
  ]]);
  const [, path, body] = calls("POST")[0]!;
  expect(path).toBe("/api/v1/universes/universe/bots");
  expect(body.bot).toEqual({
    botId: "digest", displayName: "Digest", description: "Writes the morning digest.", profileId: "digest",
    brief: expect.stringContaining("Every morning you prepare a digest"), runsPerDay: 10, selfConfig: true, emit: false,
  });
  expect(body.triggers).toEqual([expect.objectContaining({ triggerId: "daily", kind: "schedule" })]);
});

it("removes the profile it just created when the bot cannot be created", async () => {
  failCreate = true;
  await show();
  await type("new-bot-name", "Triage");
  await click(step("Guardrails"));
  await click(createButton());
  await waitForUi(() => expect(dialog()!.textContent).toContain("bot quota reached"));
  expect(calls("DELETE")).toEqual([["DELETE", "/api/v1/universes/universe/profiles/triage"]]);
  expect(location()).toBeUndefined();
});

it("holds creation while the id is missing, taken by a bot, or taken by a profile", async () => {
  await show();
  await click(step("Guardrails"));
  expect(createButton()!.disabled).toBe(true);
  expect(dialog()!.textContent).toContain("1 item needs attention");

  await click(step("Job"));
  await type("new-bot-name", "Taken");
  expect(input("new-bot-id").getAttribute("aria-invalid")).toBe("true");
  expect(dialog()!.textContent).toContain("Taken by another bot.");
  await type("new-bot-name", "Claimed");
  await click(step("Guardrails"));
  expect(createButton()!.disabled).toBe(true);

  // A profile with the bot's id is fine once the bot applies a shared profile instead.
  await click(step("Session profile"));
  await click(buttons().find((node) => node.textContent?.startsWith("A shared profile")));
  const picker = dialog()!.querySelector<HTMLSelectElement>('[data-testid="select"]')!;
  await act(async () => { picker.value = "shared-support"; picker.dispatchEvent(new Event("change", { bubbles: true })); });
  await click(step("Guardrails"));
  expect(createButton()!.disabled).toBe(false);
  await click(createButton());
  await waitForUi(() => expect(location()).toBe("/u/factory/bots/claimed (introduce)"));
  expect(calls("PUT")).toHaveLength(0);
  expect(calls("POST")[0]![2].bot.profileId).toBe("shared-support");
});

it("requires a shared profile to be picked once that mode is chosen", async () => {
  await show();
  await type("new-bot-name", "Helper");
  await click(step("Session profile"));
  expect(dialog()!.querySelector('[data-testid="config-editor"]')).not.toBeNull();
  await click(buttons().find((node) => node.textContent?.startsWith("A shared profile")));
  expect(dialog()!.querySelector('[data-testid="config-editor"]')).toBeNull();
  await click(step("Guardrails"));
  expect(createButton()!.disabled).toBe(true);
});

it("rejects a daily run limit below one", async () => {
  await show();
  await type("new-bot-name", "Helper");
  await click(step("Guardrails"));
  await type("new-bot-runs", "0");
  expect(createButton()!.disabled).toBe(true);
  await type("new-bot-runs", "");
  expect(createButton()!.disabled).toBe(false);
  await click(createButton());
  await waitForUi(() => expect(calls("POST")).toHaveLength(1));
  expect(calls("POST")[0]![2].bot).not.toHaveProperty("runsPerDay");
});

it("opens the inbox when the bot may message other bots", async () => {
  await show();
  await type("new-bot-name", "Relay");
  await click(step("Other bots"));
  const inbox = () => dialog()!.querySelector<HTMLSelectElement>("#new-bot-inbox")!;
  expect(inbox().value).toBe("off");
  await click(dialog()!.querySelector<HTMLElement>("#new-bot-emit")!);
  expect(inbox().value).toBe("any");
  await click(step("Guardrails"));
  await click(createButton());
  await waitForUi(() => expect(calls("POST")).toHaveLength(1));
  const body = calls("POST")[0]![2];
  expect(body.bot.emit).toBe(true);
  expect(body.triggers).toEqual([expect.objectContaining({ triggerId: "inbox", kind: "bot" })]);
});

it("adds and removes a trigger drafted from the picker", async () => {
  await show();
  await type("new-bot-name", "Pinger");
  await click(step("Triggers"));
  await click(buttons().find((node) => node.textContent?.startsWith("Schedule")));
  await click(buttons().find((node) => node.textContent?.startsWith("Schedule")));
  expect(dialog()!.querySelectorAll('[aria-label="Remove trigger"]')).toHaveLength(2);
  await click(dialog()!.querySelectorAll<HTMLElement>('[aria-label="Remove trigger"]')[0]);
  expect(dialog()!.querySelectorAll('[aria-label="Remove trigger"]')).toHaveLength(1);
});

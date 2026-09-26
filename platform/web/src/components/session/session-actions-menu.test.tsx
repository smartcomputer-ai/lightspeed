// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SessionActionsMenu } from "./session-actions-menu";

// The menu's contents are what is under test, not the popup: render every
// primitive in place, separators as <hr>.
vi.mock("@/components/ui/dropdown-menu", () => {
  const pass = ({ children }: { children?: ReactNode }) => <div>{children}</div>;
  return {
    DropdownMenu: pass,
    DropdownMenuTrigger: () => null,
    DropdownMenuContent: pass,
    DropdownMenuGroup: pass,
    DropdownMenuLabel: pass,
    DropdownMenuItem: pass,
    DropdownMenuCheckboxItem: pass,
    DropdownMenuSeparator: () => <hr />,
  };
});

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

/** The menu as its groups, each the text of what it holds. */
async function groups(props: Partial<Parameters<typeof SessionActionsMenu>[0]>): Promise<string[]> {
  await act(async () => root.render(
    <MemoryRouter>
      <SessionActionsMenu variant="more" sessionId="session-1" metadata={{ team: "core" }} {...props} />
    </MemoryRouter>,
  ));
  return container.innerHTML
    .split("<hr>")
    .map((html) => {
      const part = document.createElement("div");
      part.innerHTML = html;
      return part.textContent?.trim() ?? "";
    });
}

it("puts what can be done first, then view preferences, then the id and metadata", async () => {
  const [actions, preferences, identity, metadata, ...rest] = await groups({
    onSettings: () => undefined,
    onShare: () => undefined,
    lifecycle: <span>Close session…</span>,
  });
  expect(actions).toMatch(/^Session settings\s*Share with universe…\s*Close session…$/);
  expect(preferences).toMatch(/^Collapse completed runs\s*Show run statistics$/);
  expect(identity).toMatch(/^Session ID\s*session-1/);
  expect(metadata).toContain("team");
  expect(rest).toEqual([]);
});

it("starts with view preferences for someone who can do nothing", async () => {
  const [first, identity, metadata] = await groups({});
  expect(first).toMatch(/^Collapse completed runs/);
  expect(identity).toContain("session-1");
  expect(metadata).toContain("team");
});

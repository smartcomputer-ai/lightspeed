// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { BotActionsMenu } from "./bot-actions-menu";

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
    DropdownMenuItem: ({ children, onClick }: { children?: ReactNode; onClick?: () => void }) => (
      <button type="button" onClick={onClick}>{children}</button>
    ),
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

async function groups(props: Partial<Parameters<typeof BotActionsMenu>[0]>): Promise<string[]> {
  await act(async () => root.render(<BotActionsMenu botId="helper" onSettings={() => undefined} {...props} />));
  return container.innerHTML.split("<hr>").map((html) => {
    const part = document.createElement("div");
    part.innerHTML = html;
    return part.textContent?.trim() ?? "";
  });
}

it("offers settings and pausing, then the bot's id", async () => {
  const onToggle = vi.fn();
  const [actions, identity, ...rest] = await groups({ pause: { enabled: true, pending: false, onToggle } });
  expect(actions).toMatch(/^Bot settings\s*Pause bot$/);
  expect(identity).toMatch(/^Bot ID\s*helper/);
  expect(container.querySelector('[aria-label="Copy bot id"]')).not.toBeNull();
  expect(rest).toEqual([]);
  const pause = [...container.querySelectorAll("button")].find((button) => button.textContent === "Pause bot")!;
  await act(async () => pause.click());
  expect(onToggle).toHaveBeenCalledOnce();
});

it("offers resuming a paused bot, and only settings to someone who cannot manage it", async () => {
  const [paused] = await groups({ pause: { enabled: false, pending: false, onToggle: () => undefined } });
  expect(paused).toContain("Resume bot");
  const [actions] = await groups({});
  expect(actions).toBe("Bot settings");
});

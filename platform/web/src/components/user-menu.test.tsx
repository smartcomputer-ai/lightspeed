// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { UserMenu } from "./user-menu";

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
    DropdownMenuRadioGroup: pass,
    DropdownMenuRadioItem: pass,
    DropdownMenuSeparator: () => <hr />,
    DropdownMenuSub: pass,
    DropdownMenuSubContent: pass,
    DropdownMenuSubTrigger: pass,
  };
});
vi.mock("@/components/ui/sidebar", () => {
  const pass = ({ children }: { children?: ReactNode }) => <div>{children}</div>;
  return { SidebarMenu: pass, SidebarMenuItem: pass, SidebarMenuButton: pass };
});
vi.mock("@/auth", () => ({ authClient: { signOut: vi.fn() } }));

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

it("ends with the Lightspeed version", async () => {
  const user = { id: "u1", name: "Ada Demo", email: "ada@example.com" } as Parameters<typeof UserMenu>[0]["user"];
  await act(async () => root.render(
    <MemoryRouter>
      <UserMenu user={user} canAdmin={false} />
    </MemoryRouter>,
  ));
  const groups = container.innerHTML.split("<hr>");
  const last = document.createElement("div");
  last.innerHTML = groups.at(-1)!;
  expect(__LIGHTSPEED_VERSION__).toMatch(/^\d+\.\d+\.\d+/);
  expect(last.textContent?.trim()).toBe(`Lightspeed v${__LIGHTSPEED_VERSION__}`);
});

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

it("shows the Lightspeed version below the user identity, separated from Settings", async () => {
  const user = { id: "u1", name: "Ada Demo", email: "ada@example.com" } as Parameters<typeof UserMenu>[0]["user"];
  await act(async () => root.render(
    <MemoryRouter>
      <UserMenu user={user} canAdmin={false} />
    </MemoryRouter>,
  ));
  const groups = container.innerHTML.split("<hr>");
  const header = document.createElement("div");
  header.innerHTML = groups[0]!;
  expect(__LIGHTSPEED_VERSION__).toMatch(/^\d+\.\d+\.\d+/);
  expect(Array.from(header.querySelectorAll("span"), (span) => span.textContent?.trim())).toEqual([
    user.name,
    user.email,
    `Lightspeed v${__LIGHTSPEED_VERSION__}`,
  ]);
  expect(container.querySelector("hr")?.nextElementSibling?.textContent?.trim()).toBe("Settings");
  expect(groups.at(-1)).not.toContain("Lightspeed v");
});

// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DevicePage } from "./DevicePage";
const review = vi.hoisted(() => ({ data: { user_code: "ABCDEFGH", status: "pending", client_id: "lightspeed-cli" }, error: null, isFetching: false }));
vi.mock("@tanstack/react-query", () => ({ useQuery: () => review }));
let root: Root; let container: HTMLDivElement; let fetchMock: ReturnType<typeof vi.fn>;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  fetchMock = vi.fn(async () => Response.json({ success: true })); vi.stubGlobal("fetch", fetchMock);
  review.data = { user_code: "ABCDEFGH", status: "pending", client_id: "lightspeed-cli" };
  container = document.createElement("div"); document.body.append(container); root = createRoot(container);
});
afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.unstubAllGlobals(); });
async function render() { await act(async () => root.render(<MemoryRouter initialEntries={["/device?user_code=ABCDEFGH"]}><DevicePage user={{ id: "user", name: "User", email: "user@example.test" }} /></MemoryRouter>)); }
function button(text: string) { return [...container.querySelectorAll("button")].find(button => button.textContent?.includes(text))!; }
it("shows identity and code, and never approves until the user confirms and clicks", async () => {
  await render();
  expect(container.textContent).toContain("user@example.test"); expect(container.textContent).toContain("ABCDEFGH");
  expect(container.textContent).toContain("lightspeed-cli");
  expect(fetchMock).not.toHaveBeenCalled();
  expect(button("Approve CLI").disabled).toBe(true);
  await act(async () => (container.querySelector('[role="checkbox"]') as HTMLElement).click());
  expect(button("Approve CLI").disabled).toBe(false);
  await act(async () => button("Approve CLI").click());
  expect(fetchMock).toHaveBeenCalledWith("/api/auth/device/approve", expect.objectContaining({ method: "POST", body: JSON.stringify({ userCode: "ABCDEFGH" }) }));
  expect(container.textContent).toContain("Return to your terminal");
});
it("permits explicit denial without confirmation", async () => {
  await render(); await act(async () => button("Deny").click());
  expect(fetchMock).toHaveBeenCalledWith("/api/auth/device/deny", expect.anything());
  expect(container.textContent).toContain("No CLI session was issued");
});
it("does not offer approval for a code claimed by another user", async () => {
  review.data.client_id = ""; await render();
  expect(button("Approve CLI")).toBeUndefined();
  expect(container.textContent).toContain("another account");
  expect(fetchMock).not.toHaveBeenCalled();
});

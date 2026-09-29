// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ModelOption } from "@/api";
import { SessionComposer } from "./composer";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...(await original<typeof import("@/api")>()), api: mocks.api }));
vi.mock("@/components/ui/popover", () => import("@/components/ui/popover.test-double"));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let root: ReturnType<typeof createRoot>;
let container: HTMLDivElement;
let uploads: Array<(blobRef: string) => void>;
beforeEach(() => {
  uploads = [];
  mocks.api.mockReset();
  mocks.api.mockImplementation(() => new Promise((resolve) => {
    uploads.push((blobRef) => resolve({ blobs: [{ blobRef, bytes: 3 }] }));
  }));
});
afterEach(async () => {
  await act(async () => root?.unmount());
  container?.remove();
  localStorage.clear();
});

const config = {
  model: { providerId: "openai", apiKind: "openai:responses", model: "gpt-5.5" },
  generation: { reasoningEffort: "high", maxOutputTokens: 4000 },
};
const models: ModelOption[] = ["gpt-5.5", "gpt-5.5-mini"].map((model) => ({
  providerId: "openai", apiKind: "openai:responses", model, displayName: model,
  capabilities: { reasoningEfforts: ["low", "medium", "high"] }, source: "provider", fetchedAtMs: 0,
}));

async function setup({ runActive = true, canSteer = true, text = "  Change direction  ", onSaveDefault = vi.fn(async () => {}) } = {}) {
  localStorage.setItem("composer-test", text);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const onSend = vi.fn();
  await act(async () => root.render(<SessionComposer draftKey="composer-test" runActive={runActive}
    canSteer={canSteer} error={null} onSend={onSend} onStop={vi.fn()}
    attachments={{ universeId: "u1", apiKind: "anthropic:messages" }}
    model={{ config, models, canSaveDefault: true, onSaveDefault }} />));
  return { onSend, onSaveDefault };
}
const button = (label: string) =>
  [...document.querySelectorAll("button")].find((node) => node.getAttribute("aria-label") === label || node.textContent === label)!;
const textarea = () => container.querySelector("textarea")!;
async function press(key: string, init: KeyboardEventInit = {}) {
  await act(async () => textarea().dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, ...init })));
}
async function attach(...files: File[]) {
  const input = container.querySelector<HTMLInputElement>('input[type="file"]')!;
  Object.defineProperty(input, "files", { value: files, configurable: true });
  await act(async () => input.dispatchEvent(new Event("change", { bubbles: true })));
  // The base64 read is asynchronous; let it reach the upload call.
  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
}
const png = (name = "screen.png", size = 3) => new File([new Uint8Array(size)], name, { type: "image/png" });

it.each([[true, true, "Queue message", "queue"], [false, false, "Send message", null]] as const)(
  "keeps the primary send action for runActive=%s", async (active, steerable, label, mode) => {
    const { onSend } = await setup({ runActive: active, canSteer: steerable });
    await act(async () => button(label).click());
    expect(onSend).toHaveBeenCalledWith({ text: "Change direction", attachments: [] }, mode);
    expect(textarea().value).toBe("");
    expect(localStorage.getItem("composer-test")).toBeNull();
  },
);

it("preserves keyboard steering and does not send while composing text", async () => {
  const { onSend } = await setup();
  await press("Enter", { isComposing: true });
  expect(onSend).not.toHaveBeenCalled();
  await press("Enter", { ctrlKey: true });
  expect(onSend).toHaveBeenCalledWith({ text: "Change direction", attachments: [] }, "steer");
});

it("keeps the draft and explains when nothing can be steered", async () => {
  const { onSend } = await setup({ canSteer: false });
  await press("Enter", { metaKey: true });
  expect(onSend).not.toHaveBeenCalled();
  expect(textarea().value).toBe("  Change direction  ");
  expect(container.textContent).toContain("There is no run to steer right now");
});

it("uploads attachments on pick and sends them with the message", async () => {
  const { onSend } = await setup({ runActive: false });
  await attach(png());
  expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/universes/u1/attachments", { bytesBase64: btoa("\0\0\0") }, expect.any(AbortSignal));
  expect(container.textContent).toContain("Uploading…");
  await act(async () => uploads[0]!(`sha256:${"a".repeat(64)}`));
  expect(container.textContent).toContain("PNG · 3 B");
  await act(async () => button("Send message").click());
  expect(onSend).toHaveBeenCalledWith({
    text: "Change direction",
    attachments: [{ blobRef: `sha256:${"a".repeat(64)}`, mime: "image/png", kind: "image", name: "screen.png", size: 3 }],
  }, null);
  expect(container.querySelector('[aria-label="Attachments"]')).toBeNull();
});

it("waits for uploads when Enter is pressed early, then sends", async () => {
  const { onSend } = await setup({ runActive: false, text: "" });
  await attach(png());
  await press("Enter");
  expect(onSend).not.toHaveBeenCalled();
  await act(async () => uploads[0]!(`sha256:${"b".repeat(64)}`));
  expect(onSend).toHaveBeenCalledWith(expect.objectContaining({ text: "", attachments: [expect.objectContaining({ name: "screen.png" })] }), null);
});

it("refuses unsupported and oversized files at pick time", async () => {
  await setup({ runActive: false });
  await attach(new File(["x"], "notes.docx", { type: "application/msword" }), png("huge.png", 6 * 1024 * 1024));
  expect(mocks.api).not.toHaveBeenCalled();
  expect(container.textContent).toContain("notes.docx is not a type the model can read.");
  expect(container.textContent).toContain("1 more file was not attached.");
  await act(async () => button("Dismiss").click());
  await attach(png("huge.png", 6 * 1024 * 1024));
  // Anthropic's per-image limit is lower than the runtime's.
  expect(container.textContent).toContain("huge.png is 6.0 MB. PNG files can be up to 5.0 MB.");
});

it("blocks sending while an upload has failed until it is removed", async () => {
  mocks.api.mockRejectedValueOnce(new Error("Gateway unavailable"));
  const { onSend } = await setup({ runActive: false });
  await attach(png());
  expect(container.textContent).toContain("Upload failed");
  await act(async () => button("Send message").click());
  expect(onSend).not.toHaveBeenCalled();
  expect(container.textContent).toContain("Remove or retry the attachments that failed to upload.");
  await act(async () => button("Remove screen.png").click());
  await act(async () => button("Send message").click());
  expect(onSend).toHaveBeenCalledWith({ text: "Change direction", attachments: [] }, null);
});

it("restores ready attachments with the draft", async () => {
  localStorage.setItem("composer-test:attachments", JSON.stringify([
    { id: "a", name: "offer.pdf", mime: "application/pdf", kind: "document", size: 2048, blobRef: `sha256:${"c".repeat(64)}` },
  ]));
  await setup({ runActive: false });
  expect(container.textContent).toContain("offer.pdf");
  expect(container.textContent).toContain("PDF · 2 KB");
});

it("sends model choices as run options and saves them as the session default", async () => {
  const { onSend, onSaveDefault } = await setup({ runActive: false });
  const pill = [...container.querySelectorAll("button")].find((node) => node.getAttribute("aria-label")?.startsWith("Model:"))!;
  expect(pill.textContent).toContain("gpt-5.5");
  expect(pill.textContent).toContain("High");
  await act(async () => button("gpt-5.5-mini").click());
  await act(async () => button("Low").click());
  expect(document.body.textContent).toContain("Applies to each message you send from here.");
  await act(async () => button("Save as session default").click());
  expect(onSaveDefault).toHaveBeenCalledWith({
    model: { providerId: "openai", apiKind: "openai:responses", model: "gpt-5.5-mini" },
    generation: { reasoningEffort: "low", maxOutputTokens: 4000 },
  });
  // Saving clears the override; choose again and send with it.
  await act(async () => button("gpt-5.5-mini").click());
  await act(async () => button("Send message").click());
  expect(onSend).toHaveBeenCalledWith({
    text: "Change direction",
    attachments: [],
    options: { model: { providerId: "openai", apiKind: "openai:responses", model: "gpt-5.5-mini" } },
  }, null);
});

it("does not send run options with steering", async () => {
  localStorage.setItem("composer-test:run", JSON.stringify({ reasoningEffort: "low" }));
  const { onSend } = await setup();
  await press("Enter", { ctrlKey: true });
  expect(onSend).toHaveBeenCalledWith({ text: "Change direction", attachments: [] }, "steer");
});

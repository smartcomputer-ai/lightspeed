// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SessionComposer } from "./composer";

const mocks = vi.hoisted(() => ({ capture: vi.fn(), transcribe: vi.fn(), cancel: vi.fn() }));
vi.mock("@/lib/audio-capture", () => ({ startAudioCapture: mocks.capture, isDemoDictation: false }));
vi.mock("@/lib/dictation", () => ({ transcribeRecording: mocks.transcribe, cancelRecording: mocks.cancel }));
let root: Root;
let container: HTMLDivElement;
let finish: (text: string) => void;
let stop: ReturnType<typeof vi.fn>;
let onSend: ReturnType<typeof vi.fn>;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.clearAllMocks();
  stop = vi.fn();
  mocks.capture.mockResolvedValue({ stop, name: "dictation.webm", result: Promise.resolve(new Blob(["audio"], { type: "audio/webm" })) });
  mocks.transcribe.mockImplementation(() => new Promise<string>((resolve) => { finish = resolve; }));
  onSend = vi.fn();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  localStorage.clear();
  vi.unstubAllGlobals();
});
async function show(disabledReason?: string, disabled = false) {
  await act(async () => root.render(<SessionComposer draftKey="voice-test" runActive={false} canSteer={false}
    dictation={{ universeId: "universe", disabledReason }} disabled={disabled} error={null} onSend={onSend} onStop={vi.fn()} />));
}
async function click(label: string) {
  const button = [...container.querySelectorAll("button")].find((node) => node.getAttribute("aria-label") === label || node.textContent === label)!;
  await act(async () => button.click());
}
async function type(text: string) {
  const input = container.querySelector("textarea")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(input, text);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function record() { await click("Dictate message"); await click("Stop recording"); }
it("appends to the latest edited draft and waits for an explicit send", async () => {
  await show();
  await type("Before");
  await record();
  await type("Edited while transcribing.");
  await act(async () => finish("Spoken text."));
  expect(container.querySelector("textarea")!.value).toBe("Edited while transcribing. Spoken text.");
  expect(localStorage.getItem("voice-test")).toBe("Edited while transcribing. Spoken text.");
  expect(container.textContent).toContain("Review and edit before sending");
  expect(onSend).not.toHaveBeenCalled();
  expect(stop).toHaveBeenCalled();
  await click("Send message");
  expect(onSend).toHaveBeenCalledWith("Edited while transcribing. Spoken text.", null);
});
it.each(["Cancel dictation", "Send message"])("ignores late completion after %s", async (action) => {
  await show();
  await type("Keep this");
  await record();
  await click(action);
  expect(mocks.transcribe.mock.calls[0]![2].aborted).toBe(true);
  await act(async () => finish("Late text"));
  expect(container.querySelector("textarea")!.value).toBe(action === "Send message" ? "" : "Keep this");
  expect(onSend).toHaveBeenCalledTimes(action === "Send message" ? 1 : 0);
});
it("cancels on navigation without modifying the saved draft", async () => {
  await show();
  await type("Saved draft");
  await record();
  await act(async () => root.render(null));
  await act(async () => finish("Late text"));
  expect(mocks.transcribe.mock.calls[0]![2].aborted).toBe(true);
  expect(localStorage.getItem("voice-test")).toBe("Saved draft");
});
it.each([["Set a speech-to-text default", false], [undefined, true]] as const)("disables recording for missing defaults or session permissions", async (reason, disabled) => {
  await show(reason, disabled);
  const button = container.querySelector<HTMLButtonElement>('[aria-label="Dictate message"]')!;
  expect(button.disabled).toBe(true);
  await click("Dictate message");
  expect(mocks.capture).not.toHaveBeenCalled();
});
it("retains the draft and recording for a transcription retry", async () => {
  mocks.transcribe.mockRejectedValueOnce(new Error("Service unavailable"));
  await show();
  await type("Draft");
  await record();
  expect(container.textContent).toContain("Service unavailable");
  expect(container.querySelector("textarea")!.value).toBe("Draft");
  await click("Retry transcription");
  expect(mocks.capture).toHaveBeenCalledTimes(1);
  expect(mocks.transcribe.mock.calls[1]![1]).toBe(mocks.transcribe.mock.calls[0]![1]);
  await act(async () => finish("Retry worked"));
  expect(container.querySelector("textarea")!.value).toBe("Draft Retry worked");
});
it("explains permission denial without losing text", async () => {
  mocks.capture.mockRejectedValueOnce(new DOMException("Denied", "NotAllowedError"));
  await show();
  await type("Draft");
  await click("Dictate message");
  expect(container.textContent).toContain("Microphone access was denied");
  expect(container.querySelector("textarea")!.value).toBe("Draft");
  expect(mocks.transcribe).not.toHaveBeenCalled();
});

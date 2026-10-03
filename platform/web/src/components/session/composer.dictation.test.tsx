// @vitest-environment jsdom
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SessionComposer } from "./composer";

const mocks = vi.hoisted(() => ({ capture: vi.fn(), transcribe: vi.fn(), cancel: vi.fn(), api: vi.fn() }));
vi.mock("@/api", async original => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/audio-capture", () => ({ startAudioCapture: mocks.capture, isDemoDictation: false }));
vi.mock("@/lib/dictation", () => ({ transcribeRecording: mocks.transcribe, cancelRecording: mocks.cancel }));
vi.mock("@/components/ui/popover", () => import("@/components/ui/popover.test-double"));
let root: Root;
let container: HTMLDivElement;
let finish: (text: string) => void;
let failTranscription: (error: Error) => void;
let finishUpload: (result: unknown) => void;
let stop: ReturnType<typeof vi.fn>;
let onSend: ReturnType<typeof vi.fn>;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.clearAllMocks();
  stop = vi.fn();
  mocks.capture.mockResolvedValue({ stop, name: "dictation.webm", result: Promise.resolve(new Blob(["audio"], { type: "audio/webm" })) });
  mocks.transcribe.mockImplementation(() => new Promise<string>((resolve, reject) => { finish = resolve; failTranscription = reject; }));
  mocks.api.mockImplementation(() => new Promise(resolve => { finishUpload = resolve; }));
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
async function show(disabledReason?: string, disabled = false, props: Partial<ComponentProps<typeof SessionComposer>> = {}) {
  await act(async () => root.render(<SessionComposer draftKey="voice-test" runActive={false} canSteer={false}
    dictation={{ universeId: "universe", disabledReason }} disabled={disabled} error={null} onSend={onSend} onStop={vi.fn()} {...props} />));
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
async function press(key: string, init: KeyboardEventInit = {}) {
  await act(async () => container.querySelector("textarea")!.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, ...init })));
}
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
  expect(onSend).toHaveBeenCalledWith({ text: "Edited while transcribing. Spoken text.", attachments: [] }, null);
});
it("inserts at the caret the field last had, when the text is unchanged", async () => {
  await show();
  await type("Hello world");
  const input = container.querySelector("textarea")!;
  await act(async () => {
    input.focus();
    input.setSelectionRange(5, 5);
    input.blur();
  });
  await record();
  await act(async () => finish("big"));
  expect(input.value).toBe("Hello big world");
  expect(input.selectionStart).toBe(9);
});
it.each(["Send message", "Enter"])("stops recording on %s and sends once the transcript is ready", async action => {
  await show();
  await click("Dictate message");
  expect(container.querySelector('[aria-label^="Recording"]')).not.toBeNull();
  const input = container.querySelector("textarea")!;
  if (action === "Enter") await press("Enter");
  else await click(action);
  expect(stop).toHaveBeenCalled();
  expect(onSend).not.toHaveBeenCalled();
  await act(async () => finish("Spoken."));
  expect(onSend).toHaveBeenCalledExactlyOnceWith({ text: "Spoken.", attachments: [] }, null);
  expect(input.value).toBe("");
  expect(localStorage.getItem("voice-test")).toBeNull();
});
it("discards the recording on Escape", async () => {
  await show();
  await click("Dictate message");
  const input = container.querySelector("textarea")!;
  await act(async () => input.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
  expect(stop).toHaveBeenCalled();
  expect(container.querySelector('[aria-label="Dictate message"]')).not.toBeNull();
  expect(mocks.transcribe).not.toHaveBeenCalled();
});
it.each(["Cancel dictation", "Escape", "pagehide"])("cancels a pending send and ignores late completion after %s", async action => {
  await show();
  await type("Keep this");
  await record();
  await click("Send message");
  if (action === "Escape") await press("Escape");
  else if (action === "pagehide") await act(async () => window.dispatchEvent(new Event("pagehide")));
  else await click(action);
  expect(mocks.transcribe.mock.calls[0]![2].aborted).toBe(true);
  await act(async () => finish("Late text"));
  expect(container.querySelector("textarea")!.value).toBe("Keep this");
  expect(onSend).not.toHaveBeenCalled();
  await record();
  await act(async () => finish("New recording"));
  expect(container.querySelector("textarea")!.value).toBe("Keep this New recording");
  expect(onSend).not.toHaveBeenCalled();
});
it("cancels on navigation without modifying the saved draft", async () => {
  await show();
  await type("Saved draft");
  await record();
  await click("Send message");
  await act(async () => root.render(null));
  await act(async () => finish("Late text"));
  expect(mocks.transcribe.mock.calls[0]![2].aborted).toBe(true);
  expect(localStorage.getItem("voice-test")).toBe("Saved draft");
  expect(onSend).not.toHaveBeenCalled();
});
it("explains a missing speech default instead of recording", async () => {
  await show("Set a speech-to-text default in Models to enable dictation.");
  const button = container.querySelector<HTMLButtonElement>('[aria-label="Dictate message"]')!;
  expect(button.getAttribute("aria-disabled")).toBe("true");
  await click("Dictate message");
  expect(mocks.capture).not.toHaveBeenCalled();
  expect(container.textContent).toContain("Set a speech-to-text default in Models");
});
it("offers no dictation when the composer is disabled", async () => {
  await show(undefined, true);
  expect(container.querySelector('[aria-label="Dictate message"]')).toBeNull();
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

it.each(["", "Existing draft"])("waits for transcription with draft %j and sends the latest text exactly once", async draft => {
  await show();
  await type(draft);
  await record();
  expect(container.querySelector<HTMLButtonElement>('[aria-label="Send message"]')!.disabled).toBe(false);
  await click("Send message");
  await click("Send message");
  expect(onSend).not.toHaveBeenCalled();
  expect(mocks.transcribe.mock.calls[0]![2].aborted).toBe(false);
  expect(container.textContent).toContain("Your message will send when ready");
  await type("Latest edit");
  await act(async () => finish("Spoken text"));
  expect(onSend).toHaveBeenCalledExactlyOnceWith({ text: "Latest edit Spoken text", attachments: [] }, null);
  expect(container.querySelector("textarea")!.value).toBe("");
});

it.each([false, true])("preserves queue and keyboard steering after transcription (steer: %s)", async steer => {
  await show(undefined, false, { runActive: true, canSteer: true });
  await record();
  await press("Enter", { ctrlKey: steer });
  expect(onSend).not.toHaveBeenCalled();
  await act(async () => finish("Spoken text"));
  expect(onSend).toHaveBeenCalledExactlyOnceWith({ text: "Spoken text", attachments: [] }, steer ? "steer" : "queue");
});

it("keeps the draft after a transcription error and requires a new send after retry", async () => {
  await show();
  await type("Keep this");
  await record();
  await click("Send message");
  await act(async () => failTranscription(new Error("Service unavailable")));
  expect(container.textContent).toContain("Service unavailable");
  expect(onSend).not.toHaveBeenCalled();
  expect(container.querySelector("textarea")!.value).toBe("Keep this");
  await click("Retry transcription");
  await act(async () => finish("Recovered text"));
  expect(container.querySelector("textarea")!.value).toBe("Keep this Recovered text");
  expect(onSend).not.toHaveBeenCalled();
  await click("Send message");
  expect(onSend).toHaveBeenCalledExactlyOnceWith({ text: "Keep this Recovered text", attachments: [] }, null);
});

it("clears a pending send when the composer becomes disabled", async () => {
  await show();
  await type("Keep this");
  await record();
  await click("Send message");
  await show(undefined, true);
  await act(async () => finish("Late transcript"));
  expect(onSend).not.toHaveBeenCalled();
  expect(container.querySelector("textarea")!.value).toBe("Keep this");
  await show();
  await record();
  await act(async () => finish("New transcript"));
  expect(onSend).not.toHaveBeenCalled();
});

it.each(["transcript", "upload"])("waits for both the transcript and attachments when %s finishes first", async first => {
  await show(undefined, false, { attachments: { universeId: "universe", apiKind: "anthropic:messages" } });
  const input = container.querySelector<HTMLInputElement>('input[type="file"]')!;
  Object.defineProperty(input, "files", { value: [new File(["pdf"], "notes.pdf", { type: "application/pdf" })] });
  await act(async () => input.dispatchEvent(new Event("change", { bubbles: true })));
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 20)); });
  expect(mocks.api).toHaveBeenCalled();
  await record();
  await click("Send message");
  const completeTranscript = async () => { await act(async () => finish("Spoken text")); };
  const completeUpload = async () => { await act(async () => finishUpload({ blobs: [{ blobRef: `sha256:${"a".repeat(64)}`, bytes: 3 }] })); };
  if (first === "transcript") await completeTranscript();
  else await completeUpload();
  expect(onSend).not.toHaveBeenCalled();
  if (first === "transcript") await completeUpload();
  else await completeTranscript();
  expect(onSend).toHaveBeenCalledExactlyOnceWith({ text: "Spoken text", attachments: [expect.objectContaining({ name: "notes.pdf" })] }, null);
});

it("keeps the completed transcript for review when an attachment upload fails", async () => {
  mocks.api.mockRejectedValueOnce(new Error("Upload unavailable"));
  await show(undefined, false, { attachments: { universeId: "universe", apiKind: "anthropic:messages" } });
  const input = container.querySelector<HTMLInputElement>('input[type="file"]')!;
  Object.defineProperty(input, "files", { value: [new File(["pdf"], "notes.pdf", { type: "application/pdf" })] });
  await act(async () => input.dispatchEvent(new Event("change", { bubbles: true })));
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 20)); });
  expect(container.textContent).toContain("Upload failed");
  await record();
  await click("Send message");
  await act(async () => finish("Spoken text"));
  expect(onSend).not.toHaveBeenCalled();
  expect(container.querySelector("textarea")!.value).toBe("Spoken text");
  expect(container.textContent).toContain("Remove or retry the attachments");
  await click("Remove notes.pdf");
  expect(onSend).not.toHaveBeenCalled();
  await click("Send message");
  expect(onSend).toHaveBeenCalledExactlyOnceWith({ text: "Spoken text", attachments: [] }, null);
});

// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useDictationAvailability } from "./use-dictation";
const mocks = vi.hoisted(() => ({ defaults: {} as any, discovery: {} as any, browser: vi.fn() }));
vi.mock("./model-defaults", () => ({ useModelDefaults: () => mocks.defaults, useModelDiscovery: () => mocks.discovery }));
vi.mock("./audio-capture", () => ({ audioCaptureUnavailableReason: mocks.browser }));
let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
let result: ReturnType<typeof useDictationAvailability>;
function Probe() { result = useDictationAvailability("universe"); return null; }
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.browser.mockReturnValue(undefined);
  mocks.defaults = { data: { agentRun: { providerId: "openai", apiKind: "openai:responses", model: "agent" }, speechToText: null } };
  mocks.discovery = { data: { providers: [{ providerId: "speech", apiKinds: ["openai:audio-transcriptions"], credential: "notRequired" }] } };
  container = document.createElement("div");
  root = createRoot(container);
});
afterEach(async () => { await act(async () => root.unmount()); vi.unstubAllGlobals(); });
const speech = { providerId: "speech", apiKind: "openai:audio-transcriptions", model: "unlisted-speech" };
async function check() { await act(async () => root.render(<Probe />)); return result.disabledReason; }
it("requires the speech default even when an agent default is set", async () => { expect(await check()).toContain("Set a speech-to-text default"); });
it("allows a configured custom anonymous speech endpoint with an unlisted model", async () => {
  mocks.defaults.data.speechToText = speech;
  expect(await check()).toBeUndefined();
});
it("blocks the exact speech provider when credentials are missing", async () => {
  mocks.defaults.data.speechToText = speech;
  mocks.discovery.data.providers[0].credential = "missing";
  expect(await check()).toContain("needs a credential");
});
it("blocks when defaults cannot be read", async () => {
  mocks.defaults.error = new Error("Offline");
  expect(await check()).toContain("unavailable");
});
it("keeps manual selection available during discovery failure but checks browser support", async () => {
  mocks.defaults.data.speechToText = speech;
  mocks.discovery.error = new Error("Offline");
  expect(await check()).toBeUndefined();
  mocks.browser.mockReturnValue("Requires HTTPS");
  expect(await check()).toBe("Requires HTTPS");
});

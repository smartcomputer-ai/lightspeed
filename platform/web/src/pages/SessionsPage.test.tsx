// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError, type BotView, type SessionView } from "@/api";
import { BotChat } from "@/components/bot/chat";
import { sessionDraftKey } from "@/lib/sessions/draft";
import { applyEvents, emptyTranscript, reconcileRuns } from "@/lib/sessions/transcript";
import { SessionDetail } from "./SessionsPage";

const mocks = vi.hoisted(() => ({
  permissions: new Set<string>(),
  permissionLoading: false,
  api: vi.fn(),
  tail: vi.fn(),
  reconcileRuns: vi.fn(),
  scrollable: { start: true, end: true },
  scrollToEnd: vi.fn(),
  compactions: [] as { status: string; error: Error | null }[],
  session: undefined as SessionView | undefined,
  sessionRefetchInterval: undefined as unknown,
}));

vi.mock("@/api", async (original) => ({
  ...await original<typeof import("@/api")>(),
  api: mocks.api,
}));
vi.mock("@/lib/sessions/tail", () => ({ useSessionTail: mocks.tail }));
vi.mock("@/lib/permissions", () => ({
  useActionPermissions: () => ({ can: (action: string) => mocks.permissions.has(action), isLoading: mocks.permissionLoading }),
  useSessionDeletePermission: () => () => mocks.permissions.has("deleteSession"),
  useSessionClosePermission: () => () => mocks.permissions.has("closeSession"),
  usePermissionIdentity: () => "user",
  useUniverseRole: () => "contributor",
}));
// Keep queries and unrelated settings out of these composer/transcript tests.
vi.mock("@tanstack/react-query", () => ({
  useQueryClient: () => ({}),
  useQuery: ({ queryKey, refetchInterval }: { queryKey: string[]; refetchInterval?: unknown }) => {
    if (queryKey[0] === "session" && refetchInterval) mocks.sessionRefetchInterval = refetchInterval;
    return { data: queryKey[0] === "session" ? mocks.session : undefined, refetch: vi.fn() };
  },
  useInfiniteQuery: () => ({ data: undefined, refetch: vi.fn() }),
  useMutation: () => ({ isPending: false }),
  useMutationState: () => mocks.compactions,
}));
vi.mock("@/components/session/session-settings-sheet", () => ({ SessionSettingsDialog: () => null }));
vi.mock("@/components/ui/message-scroller", () => {
  const Container = ({ children }: { children?: ReactNode }) => <div>{children}</div>;
  return {
    MessageScrollerProvider: ({ children, autoScroll }: { children?: ReactNode; autoScroll?: boolean }) => (
      <div data-auto-scroll={autoScroll}>{children}</div>
    ),
    MessageScroller: Container,
    MessageScrollerContent: ({ children }: { children?: ReactNode }) => <div data-testid="transcript">{children}</div>,
    MessageScrollerViewport: Container,
    MessageScrollerItem: Container,
    MessageScrollerButton: () => null,
    useMessageScroller: () => ({ scrollToEnd: mocks.scrollToEnd }),
    useMessageScrollerScrollable: () => mocks.scrollable,
  };
});

const bot: BotView = {
  botId: "bot", profileId: "profile", revision: 1, eventSeq: 0,
  createdAtMs: 0, updatedAtMs: 0,
};
type Pathway = "session" | "bot-main" | "bot-other";
let container: HTMLDivElement;
let root: Root;
let transcript = emptyTranscript();

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  window.localStorage.clear();
  mocks.permissions = new Set(["read", "controlSession", "stopSession", "deleteSession", "invokeBot", "manageBot"]);
  mocks.permissionLoading = false;
  mocks.compactions = [];
  mocks.session = undefined;
  transcript = emptyTranscript();
  mocks.reconcileRuns.mockReset();
  mocks.tail.mockReturnValue({
    transcript, phase: "live", error: null, reconcileRuns: mocks.reconcileRuns,
    hasOlder: false, loadingOlder: false, historyError: null, historyRevision: 0, loadOlder: vi.fn(),
  });
  mocks.api.mockReset();
  mocks.scrollable.end = true;
  mocks.scrollToEnd.mockReset().mockImplementation(() => {
    // The primitive's scrollToEnd restores following-bottom when autoScroll is on.
    mocks.scrollable.end = false;
    return true;
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

describe("queued message steering", () => {
  const base = "/api/v1/universes/universe/sessions/session";
  const text = "Complete message. ".repeat(500) + "Keep the final sentence.";
  const attachment = { blobRef: `sha256:${"a".repeat(64)}`, mime: "application/pdf", kind: "document", name: "notes.pdf" };
  const queued = { id: "queued-run", status: "queued", source: { type: "input", items: [
    { type: "media", ...attachment }, { type: "text", text },
  ] } };

  function prepare() {
    transcript.activeRun = { runId: "active-run", label: "thinking", cancelling: false };
    transcript.queuedRuns = [{ runId: "queued-run" }];
    transcript.runPhases.set("queued-run", "queued");
    mocks.reconcileRuns.mockImplementation((runs: Parameters<typeof reconcileRuns>[1]) => {
      Object.assign(transcript, reconcileRuns(transcript, runs));
    });
    mocks.api.mockImplementation(async (method, path) => {
      if (method === "GET") return queued;
      if (path.endsWith("/cancel")) return { run: { id: "queued-run", status: "cancelled" } };
      if (path.endsWith("/steer")) return { steeringId: "steer", run: { id: "active-run", status: "running" } };
      return { run: { id: "requeued-run", status: "queued" } };
    });
  }
  function steerButton() {
    return container.querySelector<HTMLButtonElement>('[aria-label="Steer with queued message"]');
  }

  it.each<Pathway>(["session", "bot-main", "bot-other"])("converts a backend-loaded message with its full input in %s", async (pathway) => {
    await show(pathway);
    prepare();
    await show(pathway);
    await act(async () => steerButton()!.click());
    expect(mocks.api.mock.calls).toEqual([
      ["GET", `${base}/runs/queued-run`],
      ["POST", `${base}/runs/queued-run/cancel`, {}],
      ["POST", `${base}/runs/active-run/steer`, expect.objectContaining({ text, attachments: [attachment], submissionId: expect.any(String) })],
    ]);
    expect(container.querySelector('[aria-label="Queued messages"]')).toBeNull();
    expect(container.textContent).toContain(text);
    expect(container.textContent).not.toContain("Saved message");
  });

  it("waits for acceptance before offering conversion of an optimistic message", async () => {
    transcript.activeRun = { runId: "active-run", label: "thinking", cancelling: false };
    let accept!: (result: unknown) => void;
    mocks.api.mockReturnValue(new Promise((resolve) => { accept = resolve; }));
    await show("session");
    await type("queued locally");
    await enter();
    expect(steerButton()!.disabled).toBe(true);
    await act(async () => accept({ run: { id: "queued-run", status: "queued" } }));
    expect(steerButton()!.disabled).toBe(false);
    prepare();
    await act(async () => steerButton()!.click());
    expect(container.querySelector('[aria-label="Queued messages"]')).toBeNull();
  });

  it("does not cancel a stale queue entry that has already started", async () => {
    prepare();
    mocks.api.mockResolvedValue({ ...queued, status: "running" });
    await show("session");
    await act(async () => steerButton()!.click());
    expect(mocks.api).toHaveBeenCalledOnce();
    expect(container.textContent).toContain("no longer queued");
  });

  it("prevents duplicate clicks and cancellation while loading the message", async () => {
    prepare();
    let resolve!: (result: unknown) => void;
    mocks.api.mockReturnValueOnce(new Promise((done) => { resolve = done; }));
    await show("session");
    await act(async () => { steerButton()!.click(); steerButton()!.click(); });
    expect(mocks.api).toHaveBeenCalledOnce();
    expect(steerButton()!.disabled).toBe(true);
    expect(container.querySelector('[aria-label="Cancel queued message"]')).toBeNull();
    await act(async () => resolve(queued));
    expect(mocks.api).toHaveBeenCalledTimes(3);
  });

  it("keeps a recoverable copy when steering fails after cancellation", async () => {
    await show("session");
    prepare();
    const implementation = mocks.api.getMockImplementation()!;
    mocks.api.mockImplementation((method, path, body) => path.endsWith("/steer")
      ? Promise.reject(new Error("The active run ended")) : implementation(method, path, body));
    await show("session");
    await act(async () => steerButton()!.click());
    expect(container.querySelector('[aria-label="Queued messages"]')).toBeNull();
    expect(container.textContent).toContain("steering could not be confirmed");
    expect(container.textContent).toContain(text);
    expect(container.textContent).toContain("notes.pdf");
    expect(mocks.api).toHaveBeenCalledTimes(3);
    await navigateAway();
    await show("session", "universe", "other");
    expect(container.textContent).not.toContain("Saved message");
    await show("session");
    expect(container.textContent).toContain("Saved message");
    expect(container.textContent).toContain("notes.pdf");
    const requeue = [...container.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === "Queue again")!;
    await act(async () => requeue.click());
    expect(mocks.api).toHaveBeenLastCalledWith("POST", `${base}/messages`, {
      text, attachments: [attachment], submissionId: expect.any(String),
    });
    expect(container.textContent).not.toContain("Saved message");
  });

  it("sends one follow-up when the original steering target finishes during conversion", async () => {
    prepare();
    const implementation = mocks.api.getMockImplementation()!;
    let rejectSteer!: (error: Error) => void;
    mocks.api.mockImplementation((method, path, body) => path.endsWith("/steer")
      ? new Promise((_resolve, reject) => { rejectSteer = reject; }) : implementation(method, path, body));
    await show("session");
    await act(async () => { steerButton()!.click(); steerButton()!.click(); });
    transcript.activeRun = { runId: "next-active-run", label: "thinking", cancelling: false };
    await show("session");
    await act(async () => rejectSteer(new ApiError(409, {
      error: "run is already terminal: active-run", kind: "steering_not_submitted",
    })));
    expect(mocks.api.mock.calls.filter(([, path]) => path.endsWith("/steer"))).toHaveLength(1);
    expect(mocks.api).toHaveBeenLastCalledWith("POST", `${base}/messages`, {
      text, attachments: [attachment], submissionId: "queued-steer:queued-run",
    });
    expect(mocks.api).toHaveBeenCalledTimes(4);
    expect(container.textContent).toContain("Sent your message as a follow-up");
    expect(container.textContent).not.toContain("run is already terminal");
    expect(container.textContent).not.toContain("Saved message");
    expect(container.querySelector('[aria-label="Queued messages"]')?.textContent).toContain(text);
  });

  it.each([
    new Error("Connection lost"),
    new ApiError(409, { error: "run ended before steering reached it: active-run" }),
    new ApiError(502, { error: "Runtime unavailable" }),
  ])("does not automatically resend an uncertain steering outcome: %s", async (error) => {
    prepare();
    mocks.api.mockResolvedValueOnce(queued)
      .mockResolvedValueOnce({ run: { id: "queued-run", status: "cancelled" } })
      .mockRejectedValueOnce(error);
    await show("session");
    await act(async () => steerButton()!.click());
    expect(mocks.api).toHaveBeenCalledTimes(3);
    expect(container.textContent).toContain("Saved message");
  });

  it("retries an unconfirmed follow-up with the same submission id after reload", async () => {
    prepare();
    mocks.api.mockResolvedValueOnce(queued)
      .mockResolvedValueOnce({ run: { id: "queued-run", status: "cancelled" } })
      .mockRejectedValueOnce(new ApiError(409, { error: "Target finished", kind: "steering_not_submitted" }))
      .mockRejectedValueOnce(new Error("Response lost"));
    await show("session");
    await act(async () => steerButton()!.click());
    const firstSubmission = mocks.api.mock.calls.at(-1);
    expect(firstSubmission).toEqual(["POST", `${base}/messages`, {
      text, attachments: [attachment], submissionId: "queued-steer:queued-run",
    }]);
    expect(container.textContent).toContain("Saved message");
    await navigateAway();
    await show("session");
    mocks.api.mockResolvedValueOnce({ run: { id: "requeued-run", status: "completed" } });
    const requeue = [...container.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === "Queue again")!;
    await act(async () => { requeue.click(); requeue.click(); });
    expect(mocks.api).toHaveBeenCalledTimes(5);
    expect(mocks.api.mock.calls.at(-1)).toEqual(firstSubmission);
    expect(container.textContent).not.toContain("Saved message");
    expect(container.querySelector('[aria-label="Queued messages"]')).toBeNull();
    expect(container.textContent).not.toContain(text);
  });

  it("restores the original session without showing a follow-up in the newly opened session", async () => {
    prepare();
    const implementation = mocks.api.getMockImplementation()!;
    let rejectSteer!: (error: Error) => void;
    mocks.api.mockImplementation((method, path, body) => path.endsWith("/steer")
      ? new Promise((_resolve, reject) => { rejectSteer = reject; }) : implementation(method, path, body));
    await show("session");
    await act(async () => steerButton()!.click());
    await show("session", "universe", "other");
    await act(async () => rejectSteer(new ApiError(409, {
      error: "run is already terminal: active-run", kind: "steering_not_submitted",
    })));
    expect(mocks.api).toHaveBeenLastCalledWith("POST", `${base}/messages`, {
      text, attachments: [attachment], submissionId: "queued-steer:queued-run",
    });
    expect(container.textContent).not.toContain(text);
    expect(container.textContent).not.toContain("Sent your message as a follow-up");
    expect(container.textContent).not.toContain("Saved message");
  });

  it.each([
    { status: "cancelling", startedAtMs: 10 },
    { status: "cancelled", startedAtMs: 10 },
  ])("does not steer after the queued run started during cancellation: %j", async (run) => {
    prepare();
    mocks.api.mockResolvedValueOnce(queued).mockResolvedValueOnce({ run: { id: "queued-run", ...run } });
    await show("session");
    await act(async () => steerButton()!.click());
    expect(mocks.api).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("started before it could be converted");
    expect(container.textContent).toContain("Saved message");
  });

  it("does not steer when cancellation cannot be confirmed", async () => {
    prepare();
    mocks.api.mockResolvedValueOnce(queued).mockRejectedValueOnce(new Error("Connection lost"));
    await show("session");
    await act(async () => steerButton()!.click());
    expect(mocks.api).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("Steering was not sent");
    expect(container.textContent).toContain("Saved message");
  });

  it("keeps steering pinned to the original active run", async () => {
    prepare();
    let cancelled!: (result: unknown) => void;
    mocks.api.mockResolvedValueOnce(queued).mockReturnValueOnce(new Promise((resolve) => { cancelled = resolve; }));
    await show("session");
    await act(async () => steerButton()!.click());
    transcript.activeRun = { runId: "next-active-run", label: "thinking", cancelling: false };
    await show("session");
    await act(async () => cancelled({ run: { id: "queued-run", status: "cancelled" } }));
    expect(mocks.api).toHaveBeenLastCalledWith("POST", `${base}/runs/active-run/steer`, expect.objectContaining({ text, attachments: [attachment] }));
  });

  it("does not cancel after navigating away during the input read", async () => {
    prepare();
    let loaded!: (result: unknown) => void;
    mocks.api.mockReturnValueOnce(new Promise((resolve) => { loaded = resolve; }));
    await show("session");
    await act(async () => steerButton()!.click());
    await show("session", "universe", "other");
    await act(async () => loaded(queued));
    expect(mocks.api).toHaveBeenCalledOnce();
  });

  it("keeps the message queued if the active run ends while reading input", async () => {
    prepare();
    let loaded!: (result: unknown) => void;
    mocks.api.mockReturnValueOnce(new Promise((resolve) => { loaded = resolve; }));
    await show("session");
    await act(async () => steerButton()!.click());
    transcript.activeRun = null;
    await show("session");
    await act(async () => loaded(queued));
    expect(mocks.api).toHaveBeenCalledOnce();
    expect(container.textContent).toContain("message is still queued");
  });

  it("leaves the queue untouched when full input cannot be loaded", async () => {
    prepare();
    mocks.api.mockRejectedValue(new Error("Input unavailable"));
    await show("session");
    await act(async () => steerButton()!.click());
    expect(mocks.api).toHaveBeenCalledOnce();
    expect(container.querySelector('[aria-label="Queued messages"]')).not.toBeNull();
    expect(container.textContent).toContain("Input unavailable");
  });

  it.each([null, { runId: "active-run", label: "cancelling", cancelling: true }])("disables conversion without a steerable run: %j", async (active) => {
    prepare();
    transcript.activeRun = active;
    await show("session");
    expect(steerButton()!.disabled).toBe(true);
  });

  it.each([["read"], ["read", "stopSession"], ["read", "controlSession"]])("requires both control and stop permission: %j", async (...permissions) => {
    prepare();
    mocks.permissions = new Set(permissions);
    await show("session");
    expect(steerButton()).toBeNull();
  });

  it("respects the managed session direct input gate", async () => {
    prepare();
    mocks.session = { id: "session", lifecycleControlled: true, lifecycleStatus: "open", activity: "working" } as SessionView;
    await show("session");
    expect(steerButton()).toBeNull();
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function show(pathway: Pathway, universeId = "universe", sessionId = "session") {
  await act(async () => root.render(
    <MemoryRouter>
      {pathway === "session" ? (
        <SessionDetail universeId={universeId} handle={universeId} sessionId={sessionId} />
      ) : (
        <BotChat
          universeId={universeId}
          handle={universeId}
          bot={bot}
          state={{ controller: {
            mainSessionId: pathway === "bot-main" ? sessionId : "main",
            setupStatus: "ready", controllerStatus: "idle", enabled: true, closed: false,
          } }}
          sessionId={pathway === "bot-main" ? undefined : sessionId}
        />
      )}
    </MemoryRouter>,
  ));
}

function input() {
  return container.querySelector<HTMLTextAreaElement>('textarea[aria-label="Message"]')!;
}

async function type(text: string) {
  await act(async () => {
    // Bypass React's value tracker to simulate a browser edit.
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(input(), text);
    input().dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function enter(options: KeyboardEventInit = {}) {
  await act(async () => {
    input().dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, ...options }));
  });
}

function pauseFollowing() {
  mocks.scrollable.end = true;
  mocks.scrollToEnd.mockClear();
}

async function navigateAway() {
  await act(async () => root.render(null));
}

describe.each<Pathway>(["session", "bot-main", "bot-other"])("%s composer", (pathway) => {
  it("persists exact drafts across navigation and isolates sessions and universes", async () => {
    await show(pathway);
    await type("  unfinished\nmessage 🦀  ");
    expect(window.localStorage.getItem(sessionDraftKey("universe", "session"))).toBe("  unfinished\nmessage 🦀  ");

    await show(pathway, "universe", "other");
    expect(input().value).toBe("");
    await type("other session draft");
    await show(pathway, "another-universe", "session");
    expect(input().value).toBe("");
    await type("other universe draft");

    await navigateAway();
    await show(pathway);
    expect(input().value).toBe("  unfinished\nmessage 🦀  ");
    await show(pathway, "universe", "other");
    expect(input().value).toBe("other session draft");
    await show(pathway, "another-universe", "session");
    expect(input().value).toBe("other universe draft");
  });

  it("shows compaction request failures inside the transcript", async () => {
    mocks.compactions = [{ status: "error", error: new Error("No compactable context") }];
    await show(pathway);
    const error = [...container.querySelectorAll('[role="alert"]')].find((entry) => entry.textContent?.includes("Compaction failed"))!;
    expect(error.textContent).toContain("No compactable context");
    expect(error.closest('[data-testid="transcript"]')).not.toBeNull();
  });

  it("shows compaction progress while allowing messages and steering to be submitted", async () => {
    mocks.compactions = [{ status: "pending", error: null }];
    transcript.activeRun = { runId: "active-run", label: "running", cancelling: false };
    Object.assign(transcript, applyEvents(transcript, [{
      cursor: { seq: 1 }, observedAtMs: 1, joins: { runId: "active-run" }, sessionId: "session",
      kind: { type: "contextCompactionRequested", baseRevision: 0, revision: 1, trigger: "manual" },
    }]));
    mocks.api.mockResolvedValue({ run: { id: "queued-run", status: "queued" } });
    await show(pathway);
    const progress = container.querySelector('[data-slot="marker"][role="status"]')!;
    expect(progress.textContent).toBe("compacting context");
    expect(progress.closest('[data-testid="transcript"]')).not.toBeNull();
    expect(progress.querySelector(".animate-spin")).not.toBeNull();
    expect(container.textContent).not.toContain("Requesting compaction");
    expect(input().disabled).toBe(false);
    await type("after compaction");
    await enter();
    expect(mocks.api).toHaveBeenCalledWith("POST", expect.stringContaining("/messages"), expect.objectContaining({ text: "after compaction" }));
    await type("adjust the current run");
    await enter({ ctrlKey: true });
    expect(mocks.api).toHaveBeenCalledWith("POST", expect.stringContaining("/runs/active-run/steer"), expect.objectContaining({ text: "adjust the current run" }));
  });

  it("follows a held steering with the same submission id", async () => {
    transcript.activeRun = { runId: "active-run", label: "running", cancelling: false };
    mocks.api.mockRejectedValueOnce(new ApiError(409, { error: "held", kind: "operationPending" }))
      .mockResolvedValueOnce({ steeringId: "steering_1", run: { id: "active-run", status: "running" } });
    await show(pathway);
    await type("adjust the current run");
    await enter({ ctrlKey: true });
    const steers = mocks.api.mock.calls.filter(([, url]) => String(url).endsWith("/runs/active-run/steer"));
    expect(steers).toHaveLength(2);
    expect(steers[1]![2]).toEqual(steers[0]![2]);
  });

  it.each(["message", "queue", "steer"] as const)("clears a submitted %s and resumes following only after acceptance", async (mode) => {
    if (mode !== "message") {
      transcript.activeRun = { runId: "active-run", label: "running", cancelling: false };
    }
    let accept!: (value: unknown) => void;
    mocks.api.mockReturnValue(new Promise((resolve) => { accept = resolve; }));
    await show(pathway);
    expect(container.querySelector('[data-auto-scroll="true"]')).not.toBeNull();
    pauseFollowing();
    await type("  send this  ");
    await enter(mode === "steer" ? { ctrlKey: true } : {});
    expect(mocks.api).toHaveBeenCalledWith(
      "POST",
      `/api/v1/universes/universe/sessions/session/${mode === "steer" ? "runs/active-run/steer" : "messages"}`,
      { text: "send this", submissionId: expect.any(String) },
    );
    expect(mocks.scrollToEnd).not.toHaveBeenCalled();
    const messageSelector = mode === "queue" ? '[aria-label="Queued messages"] li' : '[data-slot="bubble"]';
    const message = container.querySelector(messageSelector)!;
    expect(message.textContent).toContain("send this");
    expect(message.className).not.toContain("opacity-60");
    expect(message.className).not.toContain("transition-opacity");
    const messageClass = message.className;
    await act(async () => accept({ run: { id: "accepted-run", status: mode === "queue" ? "queued" : "running" } }));
    expect(container.querySelector(messageSelector)!.className).toBe(messageClass);
    expect(mocks.scrollToEnd).toHaveBeenCalledWith({ behavior: "auto" });
    expect(input().value).toBe("");
    expect(window.localStorage.getItem(sessionDraftKey("universe", "session"))).toBeNull();

    // Streaming content continues to follow after a successful submission.
    mocks.scrollToEnd.mockClear();
    transcript.entries = [{ kind: "message", key: "reply", role: "assistant", text: "Newest reply" }];
    await show(pathway);
    expect(mocks.scrollToEnd).toHaveBeenCalledWith({ behavior: "auto" });

    // Manual reading remains respected until the next successful submission.
    pauseFollowing();
    transcript.entries = [...transcript.entries, { kind: "message", key: "later", role: "assistant", text: "Later reply" }];
    await show(pathway);
    expect(mocks.scrollToEnd).not.toHaveBeenCalled();
    await navigateAway();
    await show(pathway);
    expect(input().value).toBe("");
  });

  it.each(["message", "queue", "steer"] as const)("shows a rejected %s and removes its echo without resuming following", async (mode) => {
    if (mode !== "message") {
      transcript.activeRun = { runId: "active-run", label: "running", cancelling: false };
    }
    mocks.api.mockRejectedValue(new Error("Message rejected"));
    await show(pathway);
    pauseFollowing();
    await type("message");
    await enter(mode === "steer" ? { ctrlKey: true } : {});
    expect(mocks.scrollToEnd).not.toHaveBeenCalled();
    expect(container.textContent).toContain("Message rejected");
    expect(container.querySelector('[data-slot="bubble"]')).toBeNull();
    expect(container.querySelector('[aria-label="Queued messages"]')).toBeNull();
  });

  it("lets the scroller preserve a history prepend without requesting the end", async () => {
    await show(pathway);
    mocks.scrollToEnd.mockClear();
    mocks.tail.mock.results.at(-1)!.value.historyRevision = 1;
    transcript.entries = [{ kind: "message", key: "older", role: "user", text: "Older input" }];
    await show(pathway);
    expect(mocks.scrollToEnd).not.toHaveBeenCalled();
  });

  it("retains an unavailable steering draft and does not resume following", async () => {
    transcript.queuedRuns = [{ runId: "queued-run" }];
    await show(pathway);
    pauseFollowing();
    await type("steer later");
    await enter({ ctrlKey: true });
    expect(mocks.api).not.toHaveBeenCalled();
    expect(mocks.scrollToEnd).not.toHaveBeenCalled();
    await navigateAway();
    await show(pathway);
    expect(input().value).toBe("steer later");
  });
});

it("shares the draft between a normal session and bot views of that same session", async () => {
  await show("session");
  await type("shared draft");
  await navigateAway();
  await show("bot-main");
  expect(input().value).toBe("shared draft");
  await navigateAway();
  await show("bot-other");
  expect(input().value).toBe("shared draft");
});

it("keeps editing and submitting usable when local storage access is blocked", async () => {
  vi.spyOn(window, "localStorage", "get").mockImplementation(() => { throw new Error("Storage blocked"); });
  mocks.api.mockResolvedValue({ run: { id: "run", status: "running" } });
  await show("session");
  await type("in-memory draft");
  expect(input().value).toBe("in-memory draft");
  await enter();
  expect(input().value).toBe("");
  expect(mocks.api).toHaveBeenCalledOnce();
});

it.each(["post-first", "post-without-submission", "tail-first", "tail-with-input"] as const)(
  "preserves the message appearance, element, and expansion through confirmation (%s)", async (order) => {
    vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockReturnValue(700);
    let accept!: (value: unknown) => void;
    mocks.api.mockReturnValue(new Promise((resolve) => { accept = resolve; }));
    await show("session");
    const text = "A long message\n".repeat(40).trim();
    await type(text);
    await enter();
    const submissionId = mocks.api.mock.calls[0]![2].submissionId as string;
    const bubble = container.querySelector('[data-slot="bubble"]')!;
    expect(bubble.className).not.toContain("opacity-60");
    expect(bubble.className).not.toContain("transition-opacity");
    const messageClass = bubble.className;
    const expand = bubble.querySelector<HTMLButtonElement>("[data-message-expansion]")!;
    await act(async () => expand.click());
    expect(expand.getAttribute("aria-expanded")).toBe("true");

    const confirmInput = () => {
      transcript.entries = [{
        kind: "message", key: "durable-input", role: "user", text,
        runId: "run_1", origin: "user:operator",
      }];
    };
    const acknowledge = async () => {
      await act(async () => accept({ run: { id: "run_1", status: "running" } }));
      expect(container.querySelector('[data-slot="bubble"]')).toBe(bubble);
      expect(bubble.className).toBe(messageClass);
    };
    const postFirst = order === "post-first" || order === "post-without-submission";
    if (postFirst) await acknowledge();
    if (order !== "post-without-submission") transcript.runBySubmission.set(submissionId, "run_1");
    transcript.runPhases.set("run_1", "running");
    transcript.runRevision += 1;
    transcript.activeRun = { runId: "run_1", label: "thinking", cancelling: false };
    if (order === "tail-with-input") confirmInput();
    await show("session");
    expect(container.querySelectorAll('[data-slot="bubble"]')).toHaveLength(1);
    expect(container.querySelector('[data-slot="bubble"]')).toBe(bubble);
    expect(expand.getAttribute("aria-expanded")).toBe("true");
    if (order !== "tail-with-input") {
      // Acceptance leaves the appearance unchanged, including before durable input.
      expect(bubble.className).toBe(messageClass);
      confirmInput();
      await show("session");
    }
    if (!postFirst) await acknowledge();
    expect(container.querySelector('[data-slot="bubble"]')).toBe(bubble);
    expect(bubble.className).toBe(messageClass);
    expect(expand.getAttribute("aria-expanded")).toBe("true");

    transcript.activeRun = null;
    transcript.runPhases.set("run_1", "terminal");
    transcript.runRevision += 1;
    transcript.entries = [...transcript.entries,
      { kind: "message", key: "reply", role: "assistant", text: "Received", runId: "run_1" },
      { kind: "run-summary", key: "done", runId: "run_1", status: "completed", usageComplete: true },
    ];
    await show("session");
    expect(container.querySelector('[data-slot="bubble"]')).toBe(bubble);
    expect(expand.getAttribute("aria-expanded")).toBe("true");
    expect(container.textContent).toContain("Received");
  },
);

it.each([false, true])("keeps a backend-loaded run mounted through history prepends (acknowledgement loaded: %s)", async (acknowledgementLoaded) => {
  const partial = [
    { kind: "reasoning" as const, key: "thinking", text: "Inspect the workspace", runId: "run_2" },
    { kind: "message" as const, key: "reply", role: "assistant" as const, text: "Done", runId: "run_2" },
    { kind: "run-summary" as const, key: "summary", runId: "run_2", status: "completed" as const, usageComplete: true },
  ];
  transcript.entries = partial;
  if (acknowledgementLoaded) transcript.runBySubmission.set("historical-submission", "run_2");
  await show("session");
  const strip = container.querySelector<HTMLButtonElement>('button[aria-label="Show this run\'s activity"]')!;
  await act(async () => strip.click());
  expect(strip.getAttribute("aria-expanded")).toBe("true");

  transcript.entries = [
    { kind: "message", key: "older-input", role: "user", text: "Older request", runId: "run_1" },
    { kind: "run-summary", key: "older-summary", runId: "run_1", status: "completed", usageComplete: true },
    { kind: "message", key: "input", role: "user", text: "Current request", runId: "run_2" },
    ...partial,
  ];
  transcript.runBySubmission.set("historical-submission", "run_2");
  mocks.tail.mock.results.at(-1)!.value.historyRevision += 1;
  await show("session");
  expect(container.querySelector('button[aria-label="Hide this run\'s activity"]')).toBe(strip);
  expect(strip.getAttribute("aria-expanded")).toBe("true");
  expect(container.textContent).toContain("Inspect the workspace");
  expect(container.textContent).toContain("Current request");
});


describe("dead session execution", () => {
  it("says the workflow stopped instead of showing the stored activity", async () => {
    mocks.session = { id: "session", lifecycleControlled: false, lifecycleStatus: "open", activity: "working", executionFailure: "timedOut" } as SessionView;
    await show("session");
    const alert = container.querySelector('[role="alert"]');
    expect(alert?.textContent).toContain("This session's workflow timed out.");
    expect(container.textContent).toContain("Stopped");
  });

  it("offers a restart to whoever may configure the session", async () => {
    mocks.session = { id: "session", lifecycleControlled: false, lifecycleStatus: "open", activity: "idle", executionFailure: "failed" } as SessionView;
    mocks.permissions = new Set(["read", "configureSession"]);
    await show("session");
    const restart = [...container.querySelectorAll('[role="alert"] button')].find((button) => button.textContent === "Restart");
    expect(restart).toBeDefined();

    mocks.permissions = new Set(["read"]);
    await show("session");
    expect(container.querySelector('[role="alert"] button')).toBeNull();
  });

  it("stays quiet for a healthy session", async () => {
    mocks.session = { id: "session", lifecycleControlled: false, lifecycleStatus: "open", activity: "idle" } as SessionView;
    await show("session");
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(container.textContent).not.toContain("Stopped");
  });

  it("keeps re-reading a working session, because a dead workflow emits nothing", async () => {
    await show("session");
    const interval = mocks.sessionRefetchInterval as (query: { state: { data?: Partial<SessionView> } }) => number | false;
    expect(interval({ state: { data: { lifecycleStatus: "open", activity: "working" } } })).toBeGreaterThan(0);
    expect(interval({ state: { data: { lifecycleStatus: "open", activity: "idle" } } })).toBe(false);
    expect(interval({ state: { data: { lifecycleStatus: "open", activity: "working", executionFailure: "failed" } } })).toBe(false);
  });
});

describe("session action permissions", () => {
  it("invites whoever can write to an empty session, and tells a reader it is empty", async () => {
    await show("session");
    expect(container.textContent).toContain("What should we work on?");
    mocks.permissions = new Set(["read"]);
    await show("session");
    expect(container.textContent).toContain("Nothing here yet");
    expect(container.textContent).not.toContain("What should we work on?");
  });

  it("keeps a reader's transcript visible without input or queue controls", async () => {
    mocks.permissions = new Set(["read"]);
    transcript.entries = [{ kind: "message", key: "reply", role: "assistant", text: "Visible result" }];
    transcript.activeRun = { runId: "active-run", label: "running", cancelling: false };
    transcript.queuedRuns = [{ runId: "queued" }];
    await show("session");
    expect(container.textContent).toContain("Visible result");
    expect(input().disabled).toBe(true);
    expect(container.textContent).toContain("read-only access");
    for (const label of ["Stop run", "Cancel queued message", "Session settings", "Force close session", "Delete session"]) {
      expect(container.querySelector(`[aria-label="${label}"]`)).toBeNull();
    }
    await enter();
    expect(mocks.api).not.toHaveBeenCalled();
  });

  it("lets an operator stop another user's run without enabling steering or settings", async () => {
    mocks.permissions = new Set(["read", "stopSession"]);
    transcript.activeRun = { runId: "active-run", label: "running", cancelling: false };
    mocks.api.mockResolvedValue({ run: { id: "active-run", status: "cancelled" } });
    await show("session");
    expect(input().disabled).toBe(true);
    expect(container.querySelector('[aria-label="Session settings"]')).toBeNull();
    const stop = container.querySelector<HTMLButtonElement>('[aria-label="Stop run"]');
    expect(stop).not.toBeNull();
    await act(async () => stop!.click());
    expect(mocks.api).toHaveBeenCalledWith("POST", "/api/v1/universes/universe/sessions/session/runs/active-run/cancel", {});
  });

  it("fails closed while the member's role is loading", async () => {
    mocks.permissions.clear();
    mocks.permissionLoading = true;
    await show("session");
    expect(input().disabled).toBe(true);
    expect(container.querySelector('[aria-label="Session settings"]')).toBeNull();
    expect(mocks.api).not.toHaveBeenCalled();
  });

  it("removes controls when the member's role no longer permits them", async () => {
    transcript.activeRun = { runId: "active-run", label: "running", cancelling: false };
    await show("session");
    expect(input().disabled).toBe(false);
    mocks.permissions = new Set(["read"]);
    await show("session");
    expect(input().disabled).toBe(true);
    expect(container.querySelector('[aria-label="Stop run"]')).toBeNull();
  });
});

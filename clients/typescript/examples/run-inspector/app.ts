import {
  LightspeedClient,
  LightspeedRpcError,
  runTimeline,
  type RunSummaryView,
  type RunTimelineItem,
  type RunTimelineTool,
  type RunView,
  type SessionSummaryView,
} from "@lightspeed-ai/sdk";

// The same client a server would use. It talks to our own /rpc, which holds the key.
const client = new LightspeedClient({ endpoint: new URL("/rpc", location.origin) });

const el = (id: string) => document.getElementById(id) as HTMLElement;
const sessionsEl = el("sessions");
const runsEl = el("runs");
const runEl = el("run");
const errorEl = el("error");

/** Builds an element. Text only ever goes through textContent, so run output cannot inject markup. */
function h(tag: string, props: { class?: string; text?: string } = {}, ...children: Node[]): HTMLElement {
  const node = document.createElement(tag);
  if (props.class) node.className = props.class;
  if (props.text !== undefined) node.textContent = props.text;
  node.append(...children);
  return node;
}

function button(label: string, onClick: () => Promise<void> | void, className?: string): HTMLButtonElement {
  const node = h("button", { text: label, ...(className ? { class: className } : {}) }) as HTMLButtonElement;
  node.onclick = () => void guarded(async () => onClick());
  return node;
}

/** Shows what went wrong by its error kind. Giving up on a wait is not a failure. */
async function guarded(work: () => Promise<void>): Promise<void> {
  try {
    errorEl.textContent = "";
    await work();
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") return;
    const kind = error instanceof LightspeedRpcError ? `${error.kind}: ` : "";
    errorEl.textContent = kind + (error instanceof Error ? error.message : String(error));
  }
}

const when = (ms: number | null | undefined) => (ms ? new Date(ms).toLocaleString() : "");

let selectedSession: string | null = null;
let selectedRun: string | null = null;
/** Stops following the run on screen when another is selected. */
let following: AbortController | null = null;

async function loadSessions(cursor: string | null = null): Promise<void> {
  const result = await client.listSessions({ limit: 50, ...(cursor ? { cursor } : {}) });
  if (!cursor) sessionsEl.replaceChildren();
  sessionsEl.querySelector("[data-more]")?.remove();
  for (const session of result.sessions ?? []) sessionsEl.append(sessionRow(session));
  const next = result.nextCursor;
  if (next) {
    const more = button("Load more", () => loadSessions(next));
    more.dataset.more = "";
    sessionsEl.append(more);
  }
}

function sessionRow(session: SessionSummaryView): HTMLElement {
  const row = button("", () => selectSession(session.id), "row");
  row.dataset.id = session.id;
  row.append(
    h("span", { text: session.displayName ?? session.id }),
    h("small", { text: `${session.lifecycleStatus} · ${when(session.updatedAtMs)}` }),
  );
  return row;
}

function markCurrent(container: HTMLElement, id: string | null): void {
  for (const row of container.querySelectorAll<HTMLElement>("button.row")) {
    row.setAttribute("aria-current", String(row.dataset.id === id));
  }
}

async function selectSession(sessionId: string): Promise<void> {
  following?.abort();
  selectedSession = sessionId;
  selectedRun = null;
  markCurrent(sessionsEl, sessionId);
  runEl.replaceChildren();
  runsEl.replaceChildren();

  const runs = (await client.listRuns(sessionId, { limit: 50 })).runs ?? [];
  if (runs.length === 0) runsEl.append(h("small", { text: "No runs yet." }));
  for (const run of runs) runsEl.append(runRow(sessionId, run));
  if (runs[0]) await selectRun(sessionId, runs[0].id);
}

function runRow(sessionId: string, run: RunSummaryView): HTMLElement {
  const row = button("", () => selectRun(sessionId, run.id), "row");
  row.dataset.id = run.id;
  row.append(
    h("span", { text: run.source.preview ?? run.id }),
    h("small", { text: `${run.status} · ${when(run.acceptedAtMs)}` }),
  );
  return row;
}

async function selectRun(sessionId: string, runId: string): Promise<void> {
  following?.abort();
  selectedRun = runId;
  markCurrent(runsEl, runId);
  const { run } = await client.readRun(sessionId, runId);
  renderRun(sessionId, run);
  if (!["completed", "failed", "cancelled"].includes(run.status)) follow(sessionId, runId);
}

/** Events only say something changed. The run's own view is what gets shown. */
function follow(sessionId: string, runId: string): void {
  const controller = new AbortController();
  following = controller;
  const refresh = async () => {
    const { run } = await client.readRun(sessionId, runId, { signal: controller.signal });
    if (selectedRun === runId) renderRun(sessionId, run);
  };
  void guarded(async () => {
    const { run } = await client.awaitRun(sessionId, runId, {
      signal: controller.signal,
      onEvent: refresh,
    });
    if (selectedRun === runId) renderRun(sessionId, run);
  });
}

function renderRun(sessionId: string, run: RunView): void {
  const parts: Node[] = [h("h2", { text: `${run.id} · ${run.status}` })];
  if (run.failure) {
    parts.push(h("p", { class: "failed", text: `${run.failure.kind}: ${run.failure.message}` }));
  }
  if (["queued", "running", "parked"].includes(run.status)) {
    parts.push(
      h("div", { class: "actions" }, button("Cancel run", async () => {
        await client.cancelRun(sessionId, run.id);
      })),
    );
  }
  for (const approval of run.pendingApprovals ?? []) {
    const decide = (decision: "approve" | "reject") => async () => {
      await client.decideApprovals(sessionId, run.id, {
        decisions: [{ approvalId: approval.approvalId, decision }],
      });
    };
    parts.push(
      h("div", { class: "item" },
        h("strong", { text: `Approval needed: ${approval.subject.serverLabel} / ${approval.subject.toolName}` }),
        h("pre", { text: approval.subject.argumentsPreview }),
        h("div", { class: "actions" }, button("Approve", decide("approve")), button("Reject", decide("reject"))),
      ),
    );
  }
  if (run.earlierEventsOmitted) {
    parts.push(h("p", { text: "This run is very long, so its middle turns are not shown." }));
  }
  for (const item of runTimeline(run)) parts.push(timelineItem(item));
  runEl.replaceChildren(...parts);
}

function timelineItem(item: RunTimelineItem): Node {
  switch (item.type) {
    case "message":
      return h("div", { class: "item" },
        h("strong", { text: item.role }),
        h("pre", { text: item.entry.text ?? item.entry.preview ?? `(${item.entry.content.mime})` }),
      );
    case "reasoning":
      return h("div", { class: "item" },
        h("strong", { text: "reasoning" }),
        h("pre", { text: item.entry.text ?? "(not visible)" }),
      );
    case "tool":
      return toolItem(item);
    default:
      return h("div", { class: "item" }, h("strong", { text: item.entry.kind.type }));
  }
}

function toolItem(tool: RunTimelineTool): Node {
  const { call } = tool;
  const failed = call?.isError || call?.status === "failed";
  const box = h("div", { class: "item" },
    h("strong", {
      text: `${tool.name} · ${call?.status ?? "unknown"}${call?.durationMs != null ? ` · ${call.durationMs} ms` : ""}`,
      ...(failed ? { class: "failed" } : {}),
    }),
    h("pre", { text: call?.arguments ?? "" }),
  );
  if (call?.output) box.append(h("pre", { text: call.output }));
  // A tool's output in the run is a bounded preview; the whole of it is a blob.
  const outputRef = call?.outputRef;
  if (call?.outputPreviewTruncated && outputRef) {
    const load = button("Show full output", async () => {
      load.replaceWith(h("pre", { text: await client.readBlobText(outputRef) }));
    });
    box.append(load);
  }
  return box;
}

void guarded(() => loadSessions());

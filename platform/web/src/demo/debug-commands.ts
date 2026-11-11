import type { BotEventView, RunView } from "@lightspeed-ai/sdk";
import type { Environment, SessionOrigin } from "@/api";
import { closeSession, newSession, startRun } from "./engine";
import { agentSpawn, mcpCall, readFile, runCommand, webFetch, writeFile } from "./fixtures/builders";
import type { BotRecord, DemoTurn, ResponderContext, SessionRecord, UniverseState } from "./store";

/// Debug commands for the demo backend. A chat message (or steering) that
/// starts with `debug:` runs a command instead of the scripted reply, to
/// reach states the scripts do not: an approval, a stopped workflow, an
/// environment failing, a bot paused. `debug: help` and unknown commands
/// answer with the list. The demo entry installs the handler in debug mode
/// only (VITE_ENABLE_DEBUG_MODE), so other builds leave this module out.

type DebugContext = ResponderContext & { run: RunView };

interface Command {
  /// The words after `debug:`; a trailing `<n>` takes a count.
  usage: string;
  help: string;
  matches: (words: string[]) => boolean;
  turn: (words: string[], context: DebugContext) => DemoTurn;
}

const PREFIX = /^\s*debug:\s*/i;
const MAX_COUNT = 9;

const count = (words: string[], index: number, fallback = 3) => {
  const value = Number.parseInt(words[index] ?? "", 10);
  return Number.isFinite(value) && value > 0 ? Math.min(value, MAX_COUNT) : fallback;
};
const is = (...expected: string[]) => (words: string[]) => expected.every((word, index) => words[index] === word);
const reply = (text: string): DemoTurn => ({ text });

// ---------------------------------------------------------------------------
// Session states
// ---------------------------------------------------------------------------

const STOPS = {
  failed: "failed",
  "timed out": "timedOut",
  terminated: "terminated",
  missing: "missing",
} as const;

/// The MCP server a session's config names first, else any in the universe.
function sessionMcpServer(universe: UniverseState, session: SessionRecord) {
  const config = JSON.stringify(session.view.config ?? {});
  const servers = [...universe.mcpServers.values()];
  return servers.find((server) => config.includes(`"${server.serverId}"`)) ?? servers[0];
}

function approvalTurn({ universe, session }: DebugContext): DemoTurn {
  const server = sessionMcpServer(universe, session);
  const serverId = server?.serverId ?? "github";
  const serverLabel = server?.displayName ?? server?.defaultServerLabel ?? serverId;
  const args = { title: "Debug: approval requested by the demo", labels: ["debug"] };
  return {
    thinking: "This step changes something outside the session, so it needs a person's approval first.",
    text: `I need approval to call \`${serverLabel}.create_issue\` before I go on.`,
    approval: {
      serverId,
      serverLabel,
      toolName: "create_issue",
      arguments: args,
      then: (approved) => approved
        ? { tools: [mcpCall(`${serverId}.create_issue`, args, { number: 512, url: "https://example.test/issues/512" })], text: "Approved. I created the issue (#512)." }
        : reply("Rejected, so I left it alone."),
    },
  };
}

function subagentsTurn(words: string[], { store, universe, session, run }: DebugContext): DemoTurn {
  const n = count(words, 1);
  const profileId = [...universe.profiles.keys()][0] ?? "reviewer";
  const tasks = Array.from({ length: n }, (_, i) => `Debug sub-agent ${i + 1}: check part ${i + 1} of the work`);
  return {
    tools: tasks.map((task, i) => agentSpawn(profileId, task, `debug-promise-${i + 1}`)),
    text: `Delegated ${n} sub-agent${n === 1 ? "" : "s"}; they work on their own and finish one after another.`,
    landed: () => {
      const parentOrigin = session.view.origin;
      tasks.forEach((task, i) => {
        const origin: SessionOrigin = {
          kind: "subagent",
          parentSessionId: session.view.id,
          parentRunId: run.id,
          rootSessionId: session.view.retention.rootSessionId,
          depth: (parentOrigin?.depth ?? 0) + 1,
          invocationId: store.nextId("invocation"),
          agent: { profileId, revision: 1 },
          limits: { maxDepth: 3, maxConcurrent: MAX_COUNT, runTimeoutMs: 20 * 60_000 },
        };
        const child = newSession(store, universe, {
          displayName: `Debug sub-agent ${i + 1}`,
          config: structuredClone(session.view.config ?? undefined) ?? undefined,
          origin,
          activeEnvironmentId: session.view.activeEnvironmentId ?? null,
        });
        startRun(store, universe, child, {
          text: task,
          turn: { thinking: "Working through the delegated part.", thinkMs: 3_000 + i * 2_000, text: `Part ${i + 1} checked: nothing to fix.` },
        });
      });
    },
  };
}

function queueTurn(words: string[], { store, universe, session }: DebugContext): DemoTurn {
  const n = count(words, 1);
  return {
    thinkMs: 4_000,
    text: `Queued ${n} follow-up${n === 1 ? "" : "s"}; each runs once the one before it finishes.`,
    // Queued while this run is still going, so they wait in line.
    landed: () => {
      for (let i = 1; i <= n; i += 1) {
        startRun(store, universe, session, { text: `Debug follow-up ${i}`, turn: { thinkMs: 2_500, text: `Follow-up ${i} done.` } });
      }
    },
  };
}

// ---------------------------------------------------------------------------
// Run content
// ---------------------------------------------------------------------------

const TOOLS_TURN = (): DemoTurn => ({
  thinking: "Trying one tool of each kind, so every row of a run can be looked at.",
  tools: [
    readFile("README.md", "# Demo\n\nA file read by the debug command."),
    runCommand(["npm", "test"], "Tests: 42 passed, 42 total", { detail: "debug" }),
    writeFile("notes/debug.md", "# Debug\n", "a scratch note"),
    webFetch("https://example.test/status", "status page", "All systems normal."),
    mcpCall("github.list_issues", { state: "open" }, [{ number: 511, title: "Debug issue" }]),
    runCommand(["false"], "exit status 1", { detail: "fails on purpose", isError: true }),
  ],
  text: "Ran one tool of each kind; the last one failed on purpose.",
});

const LONG_REPLY = [
  "## A long reply",
  "",
  "This reply exercises the Markdown the transcript draws: **bold**, *emphasis*, `inline code`, and [a link](https://example.test).",
  "",
  "### A list",
  "",
  ...Array.from({ length: 6 }, (_, i) => `${i + 1}. Point ${i + 1}, with enough words to wrap on a narrow window and show how lines break.`),
  "",
  "### A table",
  "",
  "| Step | Status | Time |",
  "| --- | --- | --- |",
  "| Build | passed | 1m 12s |",
  "| Test | passed | 3m 40s |",
  "| Deploy | skipped | – |",
  "",
  "### Code",
  "",
  "```ts",
  "export function retry<T>(op: () => Promise<T>, attempts = 3): Promise<T> {",
  "  return op().catch((error) => (attempts > 1 ? retry(op, attempts - 1) : Promise.reject(error)));",
  "}",
  "```",
  "",
  "> A quote, to close.",
  "",
  ...Array.from({ length: 4 }, (_, i) => `Paragraph ${i + 1}. ${"The quick brown fox jumps over the lazy dog. ".repeat(6)}`),
].join("\n");

// ---------------------------------------------------------------------------
// Environment and MCP
// ---------------------------------------------------------------------------

const ENV_STATUSES = ["ready", "provisioning", "paused", "suspended", "stopped", "away", "failed", "unknown"] as const;

function environmentTurn(words: string[], { universe, session }: DebugContext): DemoTurn {
  const status = words[1] as Environment["status"];
  const environment = session.view.activeEnvironmentId ? universe.environments.get(session.view.activeEnvironmentId) : undefined;
  if (!environment) return reply("This session has no active environment to change.");
  return {
    text: `The environment ${environment.displayName ?? environment.environmentId} is now ${status}.`,
    landed: (at) => {
      environment.status = status;
      environment.state = status === "provisioning" || status === "failed" ? status : "open";
      environment.updatedAtMs = at;
    },
  };
}

function mcpTurn(words: string[], { universe, session }: DebugContext): DemoTurn {
  const server = sessionMcpServer(universe, session);
  if (!server) return reply("This universe has no MCP server to change.");
  const status = words[1] === "auth" ? "needsAuthConfig" : "active";
  return {
    text: status === "active"
      ? `The MCP server ${server.serverId} is active again.`
      : `The MCP server ${server.serverId} now needs authentication.`,
    landed: () => { server.status = status; },
  };
}

// ---------------------------------------------------------------------------
// Bots
// ---------------------------------------------------------------------------

/// The bot whose sessions include this one.
function botOf(universe: UniverseState, session: SessionRecord): BotRecord | undefined {
  return [...universe.bots.values()].find((record) =>
    record.state.mainSessionId === session.view.id
    || (record.state.sessions ?? []).some((entry) => entry.sessionId === session.view.id));
}

/// A new event for the bot, shaped like its latest one.
function botEvent(record: BotRecord, at: number, outcome: BotEventView["outcome"], detail: string | null): BotEventView {
  const last = record.events.at(-1);
  const seq = (last?.seq ?? 0) + 1;
  return {
    ...(last ?? {}),
    eventId: `debug-event-${record.bot.botId}-${seq}`,
    seq,
    kind: "debug",
    receivedAtMs: at,
    occurredAtMs: at,
    outcome,
    outcomeDetail: detail,
  } as BotEventView;
}

const BOT_STATES = ["idle", "working", "waiting", "paused", "attention", "budget", "closed"] as const;

function botTurn(words: string[], context: DebugContext): DemoTurn {
  const record = botOf(context.universe, context.session);
  if (!record) return reply("This is not one of a bot's sessions; open the bot's chat and try again there.");
  const { bot, state } = record;
  const what = words[1];
  if (what === "waiting") return approvalTurn(context);
  if (what === "events") {
    const n = count(words, 2);
    return {
      text: `${n} event${n === 1 ? "" : "s"} now wait${n === 1 ? "s" : ""} for the bot.`,
      landed: (at) => {
        for (let i = 0; i < n; i += 1) record.events.push(botEvent(record, at, null, null));
        state.pendingDeliveries = (state.pendingDeliveries ?? 0) + n;
      },
    };
  }
  if (what === "failed") {
    return {
      text: "The bot's latest event failed.",
      landed: (at) => { record.events.push(botEvent(record, at, "runFailed", "Debug: the run failed")); },
    };
  }
  return {
    text: `The bot is now ${what}.`,
    landed: (at) => {
      // From a clean, idle state, then the one asked for.
      bot.enabled = true;
      bot.closedAtMs = null;
      state.enabled = true;
      state.closed = false;
      state.controllerStatus = "idle";
      state.lastError = null;
      state.activeDeliveries = [];
      state.pendingDeliveries = 0;
      for (const event of record.events) if (event.outcome == null) event.outcome = "handled";
      switch (what) {
        case "working":
          state.controllerStatus = "deliveringEvent";
          record.events.push(botEvent(record, at, null, null));
          state.activeDeliveries = [{ deliveryId: `debug-delivery-${at}`, seqs: [record.events.at(-1)!.seq], sessionId: state.mainSessionId, startedAtMs: at }];
          break;
        case "paused":
          bot.enabled = false;
          state.enabled = false;
          break;
        case "attention":
          state.controllerStatus = "degraded";
          state.lastError = "Debug: the controller lost its connection to its channel.";
          break;
        case "budget":
          state.controllerStatus = "budgetExhausted";
          break;
        case "closed":
          bot.closedAtMs = at;
          state.closed = true;
          state.controllerStatus = "closed";
          break;
      }
    },
  };
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

const COMMANDS: Command[] = [
  {
    usage: "long think",
    help: "thinks for 15 seconds, then answers",
    matches: is("long", "think"),
    turn: () => ({
      thinking:
        "Taking the long way round on purpose: this is the demo's long-think debug command, holding the run in its thinking phase for 15 seconds so the working state can be looked at.",
      thinkMs: 15_000,
      text: "Done thinking. That took 15 seconds.",
    }),
  },
  {
    usage: "approval",
    help: "asks to approve an MCP tool call; the session waits for approval",
    matches: is("approval"),
    turn: (_, context) => approvalTurn(context),
  },
  {
    usage: "stop <failed|timed out|terminated|missing>",
    help: "stops the session's workflow that way (Restart clears it)",
    matches: (words) => words[0] === "stop" && words.slice(1).join(" ") in STOPS,
    turn: (words, { session }) => ({
      text: "Stopping the workflow.",
      finished: () => { session.view.executionFailure = STOPS[words.slice(1).join(" ") as keyof typeof STOPS]; },
    }),
  },
  {
    usage: "close",
    help: "closes the session after this reply",
    matches: is("close"),
    turn: (_, { session }) => ({ text: "Closing the session.", finished: () => { closeSession(session, false); } }),
  },
  {
    usage: "queue <n>",
    help: "queues n follow-up runs behind this one (default 3)",
    matches: is("queue"),
    turn: queueTurn,
  },
  {
    usage: "subagents <n>",
    help: "delegates to n sub-agents, which finish one after another (default 3)",
    matches: is("subagents"),
    turn: subagentsTurn,
  },
  {
    usage: "tools",
    help: "calls one tool of each kind, one of them failing",
    matches: is("tools"),
    turn: TOOLS_TURN,
  },
  {
    usage: "error",
    help: "fails the run with a provider error",
    matches: is("error"),
    turn: () => ({
      thinkMs: 1_500,
      text: "",
      fail: { kind: "modelFailure", message: "Debug: the provider is overloaded (529); try again shortly." },
    }),
  },
  {
    usage: "long reply",
    help: "answers with a long Markdown reply (lists, a table, code)",
    matches: is("long", "reply"),
    turn: () => reply(LONG_REPLY),
  },
  {
    usage: `env <${ENV_STATUSES.join("|")}>`,
    help: "sets the status of the session's active environment",
    matches: (words) => words[0] === "env" && (ENV_STATUSES as readonly string[]).includes(words[1] ?? ""),
    turn: environmentTurn,
  },
  {
    usage: "mcp <auth|ok>",
    help: "marks the session's MCP server as needing authentication, or active again",
    matches: (words) => words[0] === "mcp" && (words[1] === "auth" || words[1] === "ok"),
    turn: mcpTurn,
  },
  {
    usage: `bot <${BOT_STATES.join("|")}>`,
    help: "puts this session's bot in that state (in a bot's chat)",
    matches: (words) => words[0] === "bot" && (BOT_STATES as readonly string[]).includes(words[1] ?? ""),
    turn: botTurn,
  },
  {
    usage: "bot events <n>",
    help: "adds n events waiting for this session's bot (default 3)",
    matches: is("bot", "events"),
    turn: botTurn,
  },
  {
    usage: "bot failed",
    help: "makes this session's bot's latest event fail",
    matches: is("bot", "failed"),
    turn: botTurn,
  },
  {
    usage: "help",
    help: "lists the debug commands",
    matches: is("help"),
    turn: () => reply(commandList("Debug commands:")),
  },
];

function commandList(heading: string): string {
  return [heading, "", ...COMMANDS.map((command) => `- \`debug: ${command.usage}\`: ${command.help}`)].join("\n");
}

/// The turn a debug message asks for, or null for an ordinary message.
export function debugTurn(input: string, context: DebugContext): DemoTurn | null {
  if (!PREFIX.test(input)) return null;
  const text = input.replace(PREFIX, "").trim().replace(/\s+/g, " ").toLowerCase();
  const words = text ? text.split(" ") : [];
  const command = COMMANDS.find((candidate) => candidate.matches(words));
  if (command) return command.turn(words, context);
  return reply(commandList(text ? `Unknown debug command \`${text}\`. Debug commands:` : "Debug commands:"));
}

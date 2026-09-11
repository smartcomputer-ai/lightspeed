/// Software Factory: a software-delivery team whose feature work runs as a
/// pipeline of federated bots. One Linear issue — LIN-1421, per-API-key
/// rate limits — is followable end to end: intake writes the spec, planner
/// splits it into tasks, implementer builds each task in its own
/// provisioned sandbox with sub-agents and opens a pull request, ci-watch
/// steers a CI failure into the running task, pr-reviewer reviews, and
/// release-scribe drafts the changelog. Everything the universe pages show
/// is seeded here, with timestamps hung off boot time.
import type { Environment, GitHubApp, ProfileEnvironment, SecretGrant, SessionOrigin, UniverseSetup } from "@/api";
import type { BotEventOutcome, ModelConfig, SessionSummaryView } from "@lightspeed-ai/agent-client";
import { appendExchange, appendScriptedRun, closeSession, newSession } from "../engine";
import type { DemoResponder, DemoStore, DemoToolCall, DemoTurn, SessionRecord, UniverseState } from "../store";
import {
  BOT_TOOLS,
  DAY_MS,
  EMIT_TOOL,
  GPT,
  HOUR_MS,
  INTRODUCTION_PROMPT,
  MINUTE_MS,
  NOW,
  OPUS,
  SONNET,
  ago,
  agentRun,
  agentSpawn,
  agoIso,
  at,
  atIso,
  awaitPromises,
  bot,
  botEmit,
  botSession,
  botState,
  eventLog,
  github,
  hex,
  inboxTrigger,
  managedSession,
  mcpServer,
  member,
  modelDiscovery,
  modelOption,
  modelProvider,
  pollTrigger,
  profile,
  providerBinding,
  readFile,
  recent,
  runCommand,
  scheduleTrigger,
  template,
  tool,
  uuidLike,
  vfsReadFile,
  vfsWriteFile,
  webFetch,
  webhookTrigger,
  workspace,
  writeFile,
  type EventLog,
  type ScriptedEvent,
  mcpDisplay,
} from "./builders";
import { INCUS_PROVIDER_ID } from "./platform";

export const SOFTWARE_FACTORY_UNIVERSE_ID = "11111111-1111-4111-8111-111111111111";
export const SOFTWARE_FACTORY_SLUG = "software-factory";
const ENGINE_UNIVERSE_ID = "0000a11e-0000-4000-8000-00000000a11e";

const PROFILE = {
  intake: "intake-triage",
  planner: "planner",
  implementer: "implementer",
  explorer: "repo-explorer",
  tests: "test-writer",
  reviewer: "code-reviewer",
  scribe: "release-scribe",
  ci: "ci-watch",
} as const;
const BOT = {
  intake: "intake",
  planner: "planner",
  implementer: "implementer",
  reviewer: "pr-reviewer",
  ci: "ci-watch",
  scribe: "release-scribe",
} as const;
const WORKSPACE = { web: "acme-web", runbooks: "runbooks", specs: "specs" } as const;
const ENV = {
  ci: "env-ci-runner",
  laptop: "env-priya-laptop",
  old: "env-old-sandbox",
  taskA: "env-lin-1421-a",
  taskB: "env-lin-1421-b",
  taskC: "env-lin-1421-c",
} as const;
const GRANT = {
  github: "grant-github-app-acme-dev",
  linear: "grant-linear-api",
  linearWebhook: "grant-linear-webhook-secret",
  anthropic: "grant-anthropic-api-key",
} as const;
const GITHUB_WEBHOOK_TOKEN = "9f4c2e7ab1d84c0f8e3a5b6d7c2f1e0a";
const LINEAR_WEBHOOK_TOKEN = "3b8d1f6e0c2a4e7b9d5f8a1c3e6b0d4f";
const ISSUE = "LIN-1421";
const SPEC_PATH = "/specs/LIN-1421-rate-limits.md";
const PLAN_PATH = "/specs/LIN-1421-plan.md";

/// Deliberately realistic correlation data for exercising the sessions UI
/// with the long identifiers produced by external evaluation harnesses.
const EVALUATION_METADATA = {
  agent: "lightspeed-software-factory-agent-with-provisioned-incus-environment",
  campaign: "terminal-bench-lightspeed-rerun-hosted-20260904-113000-software-factory",
  harborContextId: "802d0778-f22c-4a1e-ab4d-3da8486ab4d8",
  job: "software-factory-regression-benchmark-linux-amd64-production-candidate",
  owner: "platform-evaluations-and-agent-reliability",
  repository: "acme/acme-web",
  source: "harbor",
  workflowRunId: "github-actions-9187-rerun-attempt-03-integration-and-browser-suite",
} as const;

const keyed = (botId: string, key: string): string => `bot:v1:${botId}:k-${key}-${hex(`${botId}:${key}`, 8)}`;
const SESSION = {
  flaky: "session-flaky-scheduler",
  audit: "session-scheduler-audit",
  auth: "session-auth-middleware",
  ci: "session-ci-slow",
  specsSpike: "session-specs-migration-spike",
  intakeMain: `bot:v1:${BOT.intake}`,
  intake1418: keyed(BOT.intake, "lin-1418"),
  intake1421: keyed(BOT.intake, "lin-1421"),
  plannerMain: `bot:v1:${BOT.planner}`,
  implMain: `bot:v1:${BOT.implementer}`,
  taskA: keyed(BOT.implementer, "lin-1421-a"),
  taskB: keyed(BOT.implementer, "lin-1421-b"),
  taskC: keyed(BOT.implementer, "lin-1421-c"),
  subExploreBucket: "session-sub-lin-1421-a-explore",
  subTestsBucket: "session-sub-lin-1421-a-tests",
  subConventions: "session-sub-lin-1421-a-conventions",
  subExploreConfig: "session-sub-lin-1421-b-explore",
  subTestsMetrics: "session-sub-lin-1421-c-tests",
  reviewerMain: `bot:v1:${BOT.reviewer}`,
  pr486: keyed(BOT.reviewer, "pr-486"),
  pr491: keyed(BOT.reviewer, "pr-491"),
  pr492: keyed(BOT.reviewer, "pr-492"),
  pr493: keyed(BOT.reviewer, "pr-493"),
  ciMain: `bot:v1:${BOT.ci}`,
  scribeMain: `bot:v1:${BOT.scribe}`,
} as const;

/// The pipeline ran this morning: `p(m)` is `m` pipeline-minutes after it
/// started, stretched so CI runs and reviews take believable wall time.
const PIPELINE_START = ago(3 * HOUR_MS + 20 * MINUTE_MS);
const PIPELINE_SCALE = 4 * MINUTE_MS;
const p = (minutes: number): number => PIPELINE_START + Math.round(minutes * PIPELINE_SCALE);
/// Milliseconds between two pipeline moments.
const span = (from: number, to: number): number => p(to) - p(from);

/// Receiver-side #N of every LIN-1421 event, fixed up front so a sender's
/// transcript can quote it; `expectSeq` catches drift when a log changes.
const SEQ = {
  plannerSpec: 3,
  intakeReceipt: 5,
  intakeApproved: 6,
  implTaskA: 1,
  implTaskB: 2,
  implTaskC: 3,
  implReviewA: 4,
  implCiFailure: 5,
  implReviewB: 6,
  implReviewC: 7,
  plannerReceiptA: 4,
  plannerReceiptB: 5,
  plannerReceiptC: 6,
  reviewerPr486: 1,
  reviewerPr491: 4,
  reviewerPr492: 6,
  reviewerCiFailure: 8,
  reviewerSync492: 9,
  reviewerPr493: 10,
  ciFailure: 3,
  ciReceiptImpl: 4,
  ciReceiptReviewer: 5,
  scribeNote486: 5,
  scribeNote1421: 6,
} as const;

function expectSeq(event: ScriptedEvent, seq: number): ScriptedEvent {
  if (event.envelope.seq !== seq) {
    throw new Error(`${event.envelope.eventId}: expected #${seq}, got #${event.envelope.seq}`);
  }
  return event;
}

const GPT_MINI: ModelConfig = { providerId: "openai", apiKind: "openai:responses", model: "gpt-5.4-mini" };

// ---------------------------------------------------------------------------
// Tool calls the builders do not cover
// ---------------------------------------------------------------------------

function vfsListDir(path: string, entries: string[]): DemoToolCall {
  return tool("vfs.list_dir", "vfs_list_dir", { path }, { group: "explore", verb: "List", target: path }, entries.join("\n"));
}

/// A Linear MCP tool, shown under the Linear verb.
function linear(name: string, args: Record<string, unknown>, detail: string, output: string, isError = false): DemoToolCall {
  return tool(`linear.${name}`, `linear.${name}`, args, mcpDisplay(`linear.${name}`, args, detail), output, isError);
}

const LINEAR_401 = "request failed: 401 Unauthorized — the Linear MCP server rejected the workspace token (rotated 2026-08-24). Reconnect the `linear` server on the Integrations page.";

function closeAt(session: SessionRecord, atMs: number): void {
  closeSession(session, true, atMs);
}

interface ReceiptInit {
  /// The bot that handled our event.
  from: string;
  /// Our event's #N at that bot.
  askedSeq: number;
  status: BotEventOutcome;
  /// The answering delivery's one-line summary.
  summary: string;
  at: number;
  hops: number;
  session: { sessionId: string; label: string };
  outcome: BotEventOutcome;
  detail: string;
  resolvedAfterMs?: number;
}

/// The deterministic `bot.reply` receipt a receiver's controller sends when
/// a delivery finishes: the outcome, never a model-authored message.
function receipt(log: EventLog, init: ReceiptInit): ScriptedEvent {
  return log.add({
    kind: "bot.reply",
    source: `bot:${init.from}`,
    at: init.at,
    summary: `#${init.askedSeq} at ${init.from} finished ${init.status}: ${init.summary}`,
    eventId: `reply:${init.from}:${hex(`${log.botId}:${init.from}:${init.askedSeq}`, 12)}`,
    session: init.session,
    sender: init.from,
    hops: init.hops,
    inReplyTo: { bot: init.from, seq: init.askedSeq },
    outcome: init.outcome,
    detail: init.detail,
    resolvedAfterMs: init.resolvedAfterMs ?? 20_000,
    data: { status: init.status },
  });
}

interface SubagentInit {
  id: string;
  displayName: string;
  profile: ProfileSpec;
  parent: SessionRecord;
  parentRunId: string;
  root: string;
  depth: number;
  environmentId: string;
  createdAtMs: number;
}

/// A sub-agent session: `origin` records who delegated it, under which
/// root, at what depth, from which pinned profile revision.
function subagentSession(store: DemoStore, universe: UniverseState, init: SubagentInit): SessionRecord {
  const origin: SessionOrigin = {
    kind: "subagent",
    parentSessionId: init.parent.view.id,
    parentRunId: init.parentRunId,
    rootSessionId: init.root,
    depth: init.depth,
    invocationId: `inv-${hex(init.id, 10)}`,
    agent: { profileId: init.profile.profileId, revision: init.profile.revision },
    limits: SUBAGENT_LIMITS,
  };
  return newSession(store, universe, {
    id: init.id,
    displayName: init.displayName,
    config: structuredClone(init.profile.config),
    instructions: init.profile.instructions,
    metadata: structuredClone(init.parent.view.metadata ?? {}),
    origin,
    activeEnvironmentId: init.environmentId,
    createdAtMs: init.createdAtMs,
  });
}

/// One descendant as the bot state's flat list carries it; profile and
/// depth already travel on the session's own `origin`.
function lineageChild(session: SessionRecord): SessionSummaryView {
  const view = session.view;
  return {
    id: view.id,
    displayName: view.displayName ?? null,
    createdAtMs: view.createdAtMs,
    updatedAtMs: view.updatedAtMs,
    lifecycleStatus: view.status === "closed" ? "closed" : "open",
    retention: view.retention,
    managed: view.managed,
    origin: view.origin ?? null,
  };
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

const SUBAGENT_LIMITS = { maxDepth: 2, maxDescendants: 6, maxConcurrent: 2, deadlineMs: 20 * MINUTE_MS };

const INTAKE_INSTRUCTIONS = [
  "You turn Linear issues that are ready to build into specs the planner can split into tasks.",
  "",
  "Read the issue and the code it touches (the acme-web workspace is linked read-only) before writing. A spec has: summary, context (what exists today, with file paths), requirements, non-goals, acceptance criteria, and open questions. Ask at most one clarifying question on the issue; when you cannot get an answer, state the assumption you are proceeding on in the spec and continue — a documented assumption beats a blocked pipeline.",
  "",
  "Write the spec to /specs/<ISSUE>-<slug>.md, then hand it to the planner with bot_emit (kind feature.spec, reply requested). Never write code.",
].join("\n");

const PLANNER_INSTRUCTIONS = [
  "You split a feature spec into tasks the implementer can build independently and in parallel.",
  "",
  "Read the spec and the repository layout. Each task names its files, tests, branch, and pull-request title, and depends on nothing that is not already on main; when two tasks must share an interface, put the type in the spec and reference it from both. Prefer three tasks of a morning each over one of a week. Write the plan to /specs/<ISSUE>-plan.md and send one task.ready per task to the implementer (reply requested). Do not write code.",
].join("\n");

const IMPLEMENTER_INSTRUCTIONS = [
  "You implement one task of a plan in a fresh sandbox with acme-web checked out on main.",
  "",
  "Read the plan and the spec it links first. Use repo-explorer to find what exists before writing, and test-writer for the tests; keep the change to the files the task names. Run the affected tests before opening the pull request; name the branch after the task id and put the issue key in the PR title. Tell pr-reviewer with bot_emit (kind pr.opened, reply requested) as soon as the PR exists.",
  "",
  "A ci.failure steered into your run means a job failed on your branch: read the log, decide whether it is yours, fix it in the same PR, and say so. Never push to main, never force-push, never merge.",
].join("\n");

const EXPLORER_INSTRUCTIONS = [
  "You answer questions about the repository checked out in the environment you inherit from your parent.",
  "",
  "Search and read before answering; run tests only when the question is about behaviour. Report file:line for everything you cite, say what you did not check, and do not change any file. Three paragraphs at most.",
].join("\n");

const TEST_WRITER_INSTRUCTIONS = [
  "You write the tests for a change another agent made, in the environment you inherit.",
  "",
  "Read the code under test and the existing tests around it, follow their conventions (vitest, fake timers, injected clocks — ask repo-explorer when you are unsure), and cover the behaviour the task names plus one failure path. Run the file you wrote and report the result with the test names. Do not change production code; if a test cannot be written without a change, say which and why.",
].join("\n");

const REVIEWER_INSTRUCTIONS = [
  "You are Acme's pull-request reviewer for the acme-web repository.",
  "",
  "Review for correctness first, then security, then maintainability. Read the diff and the surrounding code before forming an opinion; run the affected tests in the CI runner environment when a change touches behaviour. Prefer one blocking finding with a concrete fix over a list of nits. When a finding is a matter of taste, say so and do not block on it.",
  "",
  "Post reviews through the GitHub tools: request changes only for correctness or security problems, otherwise approve with comments. Quote file paths and line numbers. Never push commits to a contributor's branch.",
  "",
  'Delegate repository-wide questions ("does anything else depend on this?") to the repo-explorer sub-agent and fold its answer into the review.',
].join("\n");

const SCRIBE_INSTRUCTIONS = [
  "You write release notes for acme-web.",
  "",
  "Every Friday, list everything merged to main since the last release tag, group it by area (API, auth, scheduler, billing, internal), and write notes a customer engineer can paste into the changelog: one line per change, the PR number in parentheses, breaking changes first and marked. Skip dependency bumps unless they change behaviour. Ask the PR author (by @-mention in the docs PR) when a change has no description you can use.",
  "",
  "A release.note from another bot is a line to keep: draft it into the Unreleased section of CHANGELOG.md in the acme-web workspace right away, so Friday's notes start from a full list. Open the notes as a pull request against docs/releases/. Do not tag or publish anything.",
].join("\n");

const CI_INSTRUCTIONS = [
  "You watch GitHub Actions for acme/acme-web. Each poll event is one failed workflow run.",
  "",
  "Read the failing job's log and name the failing test or step. A failure on a branch named after a task (lin-<issue>-<task>-…) goes to the implementer as ci.failure with the task id, and to pr-reviewer with the PR number — one event per run, never a repeat. A failure on main gets an issue with the log excerpt, assigned to the author of the breaking commit. A run cancelled by a newer push is not a failure: ignore it and say so. Two failures of the same job within a few minutes with an infrastructure cause (timeouts, missing secrets) are one problem: one issue, not two.",
].join("\n");

const GITHUB_REVIEW_TOOLS = [
  "get_pull_request",
  "get_pull_request_diff",
  "get_pull_request_files",
  "list_pull_requests",
  "create_pull_request_review",
  "add_issue_comment",
  "list_workflow_runs",
  "list_workflow_jobs",
  "get_job_logs",
];
const GITHUB_IMPLEMENT_TOOLS = ["create_pull_request", "get_pull_request", "get_job_logs", "list_workflow_runs"];
const GITHUB_CI_TOOLS = ["list_workflow_runs", "list_workflow_jobs", "get_job_logs", "create_issue", "add_issue_comment"];
const GITHUB_SCRIBE_TOOLS = ["list_commits", "list_pull_requests", "get_pull_request", "list_tags", "create_pull_request", "create_or_update_file"];

const link = (workspaceId: string, access: "readOnly" | "readWrite") => ({
  path: `/${workspaceId}`,
  access,
  target: { type: "workspace", workspaceId },
});

const INTAKE_CONFIG: Record<string, unknown> = {
  model: SONNET,
  generation: { reasoningEffort: "medium", maxOutputTokens: 12_000 },
  limits: { maxTurns: 12, maxToolRounds: 20 },
  features: {
    mcp: { servers: [{ serverId: "linear" }] },
    vfs: { tools: "edit", workspaceLinks: [link(WORKSPACE.specs, "readWrite"), link(WORKSPACE.web, "readOnly")] },
  },
};

const PLANNER_CONFIG: Record<string, unknown> = {
  model: OPUS,
  generation: { reasoningEffort: "high", maxOutputTokens: 16_000 },
  limits: { maxTurns: 12, maxToolRounds: 24 },
  features: {
    vfs: { tools: "edit", workspaceLinks: [link(WORKSPACE.specs, "readWrite"), link(WORKSPACE.web, "readOnly")] },
  },
};

const IMPLEMENTER_CONFIG: Record<string, unknown> = {
  model: OPUS,
  generation: { reasoningEffort: "high", maxOutputTokens: 32_000 },
  limits: { maxTurns: 40, maxToolRounds: 120 },
  features: {
    environments: { selectionTools: false },
    mcp: { servers: [{ serverId: "github" }] },
    subagents: { agents: [{ profileId: PROFILE.explorer }, { profileId: PROFILE.tests }], ...SUBAGENT_LIMITS },
    vfs: { tools: "readOnly", workspaceLinks: [link(WORKSPACE.specs, "readOnly")] },
  },
};

const IMPLEMENTER_ENVIRONMENT: ProfileEnvironment = {
  type: "provision",
  providerId: INCUS_PROVIDER_ID,
  templateId: "dev-small-v1",
  retention: "closeWithSession",
  displayName: "implementer sandbox",
  idlePolicy: { pauseAfterMs: 15 * MINUTE_MS, stopAfterMs: 2 * HOUR_MS },
  metadata: { repo: "acme/acme-web", checkout: "main" },
  credentials: [{ envName: "GITHUB_TOKEN", source: { type: "authGrant", grantId: GRANT.github } }],
};

const EXPLORER_CONFIG: Record<string, unknown> = {
  model: OPUS,
  generation: { reasoningEffort: "medium" },
  limits: { maxToolRounds: 60 },
  features: {
    environments: { selectionTools: false },
    web: { fetch: {}, search: { allowedDomains: ["docs.github.com", "nodejs.org", "developer.mozilla.org", "hono.dev", "vitest.dev"] } },
  },
};

const TEST_WRITER_CONFIG: Record<string, unknown> = {
  model: SONNET,
  generation: { reasoningEffort: "medium", maxOutputTokens: 16_000 },
  limits: { maxToolRounds: 40 },
  features: {
    environments: { selectionTools: false },
    subagents: { agents: [{ profileId: PROFILE.explorer }], ...SUBAGENT_LIMITS },
  },
};

const REVIEWER_CONFIG: Record<string, unknown> = {
  model: OPUS,
  generation: { reasoningEffort: "high", maxOutputTokens: 16_000 },
  limits: { maxTurns: 24, maxToolRounds: 40 },
  features: {
    environments: {},
    mcp: { servers: [{ serverId: "github" }] },
    subagents: { agents: [{ profileId: PROFILE.explorer }], maxDepth: 1, maxDescendants: 4, maxConcurrent: 2, deadlineMs: 15 * MINUTE_MS },
    web: { fetch: {} },
  },
};

const SCRIBE_CONFIG: Record<string, unknown> = {
  model: GPT,
  generation: { reasoningEffort: "medium" },
  limits: { maxTurns: 12 },
  features: {
    mcp: { servers: [{ serverId: "github" }] },
    vfs: { tools: "edit", workspaceLinks: [link(WORKSPACE.web, "readWrite")] },
    web: { fetch: {} },
  },
};

const CI_CONFIG: Record<string, unknown> = {
  model: GPT_MINI,
  generation: { reasoningEffort: "low", maxOutputTokens: 6_000 },
  limits: { maxTurns: 6, maxToolRounds: 12 },
  features: {
    mcp: { servers: [{ serverId: "github" }] },
  },
};

/// What a session takes from a profile: metadata defaults, config,
/// instructions, and the revision pinned on sub-agent origins.
interface ProfileSpec {
  profileId: string;
  metadata?: Record<string, string>;
  config: Record<string, unknown>;
  instructions: string;
  revision: number;
}
const INTAKE_PROFILE: ProfileSpec = { profileId: PROFILE.intake, config: INTAKE_CONFIG, instructions: INTAKE_INSTRUCTIONS, revision: 3 };
const PLANNER_PROFILE: ProfileSpec = { profileId: PROFILE.planner, config: PLANNER_CONFIG, instructions: PLANNER_INSTRUCTIONS, revision: 2 };
const IMPLEMENTER_PROFILE: ProfileSpec = {
  profileId: PROFILE.implementer,
  metadata: { ...EVALUATION_METADATA, profileRole: "parallel-task-implementation-and-pull-request-authoring" },
  config: IMPLEMENTER_CONFIG,
  instructions: IMPLEMENTER_INSTRUCTIONS,
  revision: 6,
};
const EXPLORER_PROFILE: ProfileSpec = { profileId: PROFILE.explorer, config: EXPLORER_CONFIG, instructions: EXPLORER_INSTRUCTIONS, revision: 5 };
const TEST_WRITER_PROFILE: ProfileSpec = { profileId: PROFILE.tests, config: TEST_WRITER_CONFIG, instructions: TEST_WRITER_INSTRUCTIONS, revision: 2 };
const REVIEWER_PROFILE: ProfileSpec = { profileId: PROFILE.reviewer, config: REVIEWER_CONFIG, instructions: REVIEWER_INSTRUCTIONS, revision: 5 };
const SCRIBE_PROFILE: ProfileSpec = { profileId: PROFILE.scribe, config: SCRIBE_CONFIG, instructions: SCRIBE_INSTRUCTIONS, revision: 3 };
const CI_PROFILE: ProfileSpec = { profileId: PROFILE.ci, config: CI_CONFIG, instructions: CI_INSTRUCTIONS, revision: 2 };

function seedProfiles(universe: UniverseState): void {
  const documents = [
    profile({
      profileId: PROFILE.intake,
      displayName: "Intake triage",
      description: "Turns a ready-for-build Linear issue into a spec in the specs workspace and hands it to the planner.",
      instructions: INTAKE_INSTRUCTIONS,
      config: INTAKE_CONFIG,
      revision: INTAKE_PROFILE.revision,
      createdAtMs: ago(31 * DAY_MS),
      updatedAtMs: ago(6 * DAY_MS),
    }),
    profile({
      profileId: PROFILE.planner,
      displayName: "Planner",
      description: "Splits a spec into independent tasks with files, tests, and branch names, and sends each to the implementer.",
      instructions: PLANNER_INSTRUCTIONS,
      config: PLANNER_CONFIG,
      revision: PLANNER_PROFILE.revision,
      createdAtMs: ago(31 * DAY_MS),
      updatedAtMs: ago(9 * DAY_MS),
    }),
    profile({
      profileId: PROFILE.implementer,
      displayName: "Implementer",
      description: "Builds one task per session in a fresh Incus sandbox, delegating exploration and tests to sub-agents, and opens the pull request.",
      instructions: IMPLEMENTER_INSTRUCTIONS,
      config: IMPLEMENTER_CONFIG,
      metadata: IMPLEMENTER_PROFILE.metadata,
      environment: IMPLEMENTER_ENVIRONMENT,
      revision: IMPLEMENTER_PROFILE.revision,
      createdAtMs: ago(30 * DAY_MS),
      updatedAtMs: ago(2 * DAY_MS),
    }),
    profile({
      profileId: PROFILE.explorer,
      displayName: "Repo explorer",
      description: "Read-only sub-agent that answers repository questions with file:line citations in the environment it inherits.",
      instructions: EXPLORER_INSTRUCTIONS,
      config: EXPLORER_CONFIG,
      environment: { type: "inherit" },
      revision: EXPLORER_PROFILE.revision,
      createdAtMs: ago(62 * DAY_MS),
      updatedAtMs: ago(12 * DAY_MS),
    }),
    profile({
      profileId: PROFILE.tests,
      displayName: "Test writer",
      description: "Sub-agent that writes vitest coverage for a change in the inherited sandbox and may ask repo-explorer about conventions.",
      instructions: TEST_WRITER_INSTRUCTIONS,
      config: TEST_WRITER_CONFIG,
      environment: { type: "inherit" },
      revision: TEST_WRITER_PROFILE.revision,
      createdAtMs: ago(28 * DAY_MS),
      updatedAtMs: ago(12 * DAY_MS),
    }),
    profile({
      profileId: PROFILE.reviewer,
      displayName: "Code reviewer",
      description: "Reviews acme-web pull requests with the GitHub tools and the shared CI runner; delegates repo-wide questions to repo-explorer.",
      instructions: REVIEWER_INSTRUCTIONS,
      config: REVIEWER_CONFIG,
      environment: { type: "existing", environmentId: ENV.ci },
      revision: REVIEWER_PROFILE.revision,
      createdAtMs: ago(65 * DAY_MS),
      updatedAtMs: ago(3 * DAY_MS),
    }),
    profile({
      profileId: PROFILE.scribe,
      displayName: "Release scribe",
      description: "Turns a week of merged pull requests into changelog-ready release notes; keeps CHANGELOG.md's Unreleased section current from review notes.",
      instructions: SCRIBE_INSTRUCTIONS,
      config: SCRIBE_CONFIG,
      revision: SCRIBE_PROFILE.revision,
      createdAtMs: ago(40 * DAY_MS),
      updatedAtMs: ago(9 * DAY_MS),
    }),
    profile({
      profileId: PROFILE.ci,
      displayName: "CI watch",
      description: "Small, fast model that reads failed GitHub Actions runs and routes them: branch failures to the implementer and reviewer, main failures to an issue.",
      instructions: CI_INSTRUCTIONS,
      config: CI_CONFIG,
      revision: CI_PROFILE.revision,
      createdAtMs: ago(20 * DAY_MS),
      updatedAtMs: ago(5 * DAY_MS),
    }),
  ];
  for (const document of documents) universe.profiles.set(document.profileId, document);
}

// ---------------------------------------------------------------------------
// Members and API keys
// ---------------------------------------------------------------------------

function seedMembers(store: DemoStore, universe: UniverseState): void {
  universe.members.push(member(store, universe, "user-marco", "admin", ago(68 * DAY_MS)), member(store, universe, "user-priya", "member", ago(61 * DAY_MS)));
  universe.apiKeys.push(
    {
      keyPrefix: "lsk_acme_cfg_9b21",
      displayName: "Configurator MCP",
      createdAtMs: ago(38 * DAY_MS),
      lastUsedAtMs: ago(2 * HOUR_MS + 11 * MINUTE_MS),
    },
    {
      keyPrefix: "lsk_acme_mr_c19e",
      displayName: "Marco's laptop (rotated)",
      createdAtMs: ago(60 * DAY_MS),
      revokedAtMs: ago(12 * DAY_MS),
      lastUsedAtMs: ago(13 * DAY_MS),
    },
  );
}

// ---------------------------------------------------------------------------
// Workspaces
// ---------------------------------------------------------------------------

const ACME_WEB_README = `# acme-web

Customer-facing web API for Acme: sessions, billing webhooks, and the job scheduler.

## Development

    npm install
    npm run dev        # http://localhost:3000
    npm test           # vitest
    npm run lint

## Layout

- src/index.ts — Hono app, routes, and server bootstrap
- src/auth/ — session-cookie middleware and token verification
- src/ratelimit/ — per-API-key token buckets in front of /api (LIN-1421)
- src/scheduler/ — retrying job scheduler (see docs/adr-0007.md for the retry policy)
- src/billing/ — Stripe webhook intake
- docs/ — architecture decision records

## Deploy

Every merge to main builds an image and rolls it to staging. Production is promoted
from the release tag; see the runbooks workspace, deploy-checklist.md.
`;

const ACME_WEB_PACKAGE_JSON = `{
  "name": "acme-web",
  "version": "2.14.1",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "tsx watch src/index.ts",
    "build": "tsc -p tsconfig.json",
    "test": "vitest run",
    "lint": "eslint src"
  },
  "dependencies": {
    "hono": "^4.7.0",
    "jose": "^6.0.0",
    "pg": "^8.13.0",
    "prom-client": "^15.1.0",
    "zod": "^3.24.0"
  },
  "devDependencies": {
    "@types/node": "^22.10.0",
    "eslint": "^9.20.0",
    "tsx": "^4.19.0",
    "typescript": "^5.7.0",
    "vitest": "^3.0.0"
  }
}
`;

const ACME_WEB_INDEX = `import { Hono } from "hono";
import { logger } from "hono/logger";
import { requireSession } from "./auth/middleware.js";
import { billingWebhook } from "./billing/webhook.js";
import { pool } from "./db.js";
import { Scheduler } from "./scheduler/scheduler.js";

const app = new Hono();
app.use(logger());

app.get("/healthz", (c) => c.json({ ok: true, version: process.env.APP_VERSION ?? "dev" }));
app.post("/webhooks/billing", billingWebhook);

const api = app.basePath("/api").use(requireSession);
api.get("/me", (c) => c.json(c.get("session").user));
api.get("/projects", async (c) => {
  const { rows } = await pool.query("select id, name from projects where org_id = $1", [
    c.get("session").orgId,
  ]);
  return c.json(rows);
});

const scheduler = new Scheduler(pool, { maxAttempts: 5, baseDelayMs: 250 });
scheduler.start();

export default { port: Number(process.env.PORT ?? 3000), fetch: app.fetch };
`;

/// index.ts as task b leaves it: the limiter mounted after the session check.
const ACME_WEB_INDEX_LIMITED = ACME_WEB_INDEX.replace('import { pool } from "./db.js";\n', 'import { pool } from "./db.js";\nimport { rateLimit } from "./ratelimit/middleware.js";\n').replace(
  'const api = app.basePath("/api").use(requireSession);',
  'const api = app.basePath("/api").use(requireSession, rateLimit);',
);

const ACME_WEB_MIDDLEWARE = `import type { MiddlewareHandler } from "hono";
import { getCookie } from "hono/cookie";
import { jwtVerify, type JWTPayload } from "jose";

export interface Session {
  user: { id: string; email: string };
  orgId: string;
  expiresAt: number;
}

declare module "hono" {
  interface ContextVariableMap {
    session: Session;
  }
}

const SESSION_COOKIE = "acme_session";
const secret = new TextEncoder().encode(process.env.SESSION_SECRET ?? "");

/// Accepts either the session cookie (browsers) or a bearer token (API
/// clients). Both carry the same signed claims; the cookie path additionally
/// rejects tokens within five minutes of expiry so a refresh happens before
/// a request fails mid-flight.
export const requireSession: MiddlewareHandler = async (c, next) => {
  const fromHeader = bearerToken(c.req.header("authorization"));
  const token = fromHeader ?? getCookie(c, SESSION_COOKIE);
  if (!token) return c.json({ error: "unauthenticated" }, 401);
  let payload: JWTPayload;
  try {
    ({ payload } = await jwtVerify(token, secret, { issuer: "acme-web", clockTolerance: 30 }));
  } catch {
    return c.json({ error: "invalid session" }, 401);
  }
  const expiresAt = (payload.exp ?? 0) * 1000;
  if (fromHeader === undefined && expiresAt - Date.now() < 5 * 60_000) {
    return c.json({ error: "session expiring", refresh: true }, 401);
  }
  c.set("session", {
    user: { id: String(payload.sub), email: String(payload.email) },
    orgId: String(payload.org),
    expiresAt,
  });
  await next();
};

function bearerToken(header: string | undefined): string | undefined {
  if (!header?.startsWith("Bearer ")) return undefined;
  return header.slice("Bearer ".length).trim() || undefined;
}
`;

const RATELIMIT_BUCKET = `export interface BucketOptions {
  /// Tokens a full bucket holds; also the burst a quiet key may spend at once.
  capacity: number;
  /// Tokens added per interval.
  refill: number;
  intervalMs: number;
  /// Injectable clock so tests never wait on wall time (ADR-0007).
  now?: () => number;
}

export interface TakeResult {
  ok: boolean;
  remaining: number;
  /// Milliseconds until the request would be admitted; 0 when \`ok\`.
  retryAfterMs: number;
}

interface Bucket {
  tokens: number;
  /// Start of the last interval that was credited.
  creditedAt: number;
}

/// Token buckets keyed by caller. Refill is credited lazily from the number
/// of *whole* intervals elapsed since the last credit, so a caller cannot
/// refill itself by calling often — the bug #482 shipped and reverted.
export class TokenBuckets {
  private readonly buckets = new Map<string, Bucket>();
  private readonly now: () => number;

  constructor(private readonly options: BucketOptions) {
    this.now = options.now ?? Date.now;
  }

  take(key: string, cost = 1): TakeResult {
    const { capacity, refill, intervalMs } = this.options;
    const now = this.now();
    const bucket = this.buckets.get(key) ?? { tokens: capacity, creditedAt: now };
    const intervals = Math.floor((now - bucket.creditedAt) / intervalMs);
    if (intervals > 0) {
      bucket.tokens = Math.min(capacity, bucket.tokens + intervals * refill);
      bucket.creditedAt += intervals * intervalMs;
    }
    this.buckets.set(key, bucket);
    if (bucket.tokens >= cost) {
      bucket.tokens -= cost;
      return { ok: true, remaining: bucket.tokens, retryAfterMs: 0 };
    }
    const deficit = cost - bucket.tokens;
    const wait = Math.ceil(deficit / refill) * intervalMs;
    return { ok: false, remaining: bucket.tokens, retryAfterMs: bucket.creditedAt + wait - now };
  }

  /// Drops keys that have been full for longer than one interval, so the
  /// map does not grow with every key that ever called.
  sweep(): number {
    const { capacity, intervalMs } = this.options;
    const now = this.now();
    let dropped = 0;
    for (const [key, bucket] of this.buckets) {
      if (bucket.tokens >= capacity && now - bucket.creditedAt > intervalMs) {
        this.buckets.delete(key);
        dropped += 1;
      }
    }
    return dropped;
  }
}
`;

const RATELIMIT_MIDDLEWARE_DRAFT = `import type { MiddlewareHandler } from "hono";
import { TokenBuckets } from "./bucket.js";

export interface RateLimitConfig {
  enabled: boolean;
  /// Requests per minute per API key.
  perMinute: number;
  /// Burst a quiet key may spend at once; defaults to one minute's worth.
  burst: number;
}

export function readConfig(env: NodeJS.ProcessEnv): RateLimitConfig {
  const perMinute = Number(env.RATE_LIMIT_PER_KEY);
  return {
    enabled: env.RATE_LIMIT_ENABLED !== "false",
    perMinute,
    burst: Number(env.RATE_LIMIT_BURST ?? perMinute),
  };
}

const config = readConfig(process.env);
const buckets = new TokenBuckets({ capacity: config.burst, refill: config.perMinute, intervalMs: 60_000 });

/// Limits /api per API key. Browser sessions have no key and are bucketed
/// by org so one tab cannot starve the org's integrations. A refusal is a
/// 429 with Retry-After in whole seconds and the RateLimit-* headers.
export const rateLimit: MiddlewareHandler = async (c, next) => {
  if (!config.enabled) return next();
  const key = c.req.header("x-api-key") ?? \`org:\${c.get("session").orgId}\`;
  const result = buckets.take(key);
  c.header("RateLimit-Limit", String(config.perMinute));
  c.header("RateLimit-Remaining", String(result.remaining));
  if (!result.ok) {
    c.header("Retry-After", String(Math.ceil(result.retryAfterMs / 1000)));
    return c.json({ error: "rate limited", retryAfterMs: result.retryAfterMs }, 429);
  }
  await next();
};
`;

/// The middleware as merged: the default and the guard the CI failure asked for.
const RATELIMIT_MIDDLEWARE = RATELIMIT_MIDDLEWARE_DRAFT.replace(
  `export function readConfig(env: NodeJS.ProcessEnv): RateLimitConfig {
  const perMinute = Number(env.RATE_LIMIT_PER_KEY);
  return {`,
  `const DEFAULT_PER_MINUTE = 600;

/// RATE_LIMIT_PER_KEY defaults to 600/min and must be a positive integer:
/// an unset variable used to become NaN and refuse every request.
export function readConfig(env: NodeJS.ProcessEnv): RateLimitConfig {
  const perMinute = env.RATE_LIMIT_PER_KEY === undefined ? DEFAULT_PER_MINUTE : Number(env.RATE_LIMIT_PER_KEY);
  if (!Number.isInteger(perMinute) || perMinute <= 0) {
    throw new Error(\`RATE_LIMIT_PER_KEY must be a positive integer, got \${JSON.stringify(env.RATE_LIMIT_PER_KEY)}\`);
  }
  return {`,
);

const RATELIMIT_METRICS = `import { Counter, Histogram } from "prom-client";
import { registry } from "../metrics.js";
import type { TakeResult } from "./bucket.js";

const decisions = new Counter({
  name: "acme_ratelimit_decisions_total",
  help: "Rate-limit decisions per API key, by outcome.",
  labelNames: ["key", "outcome"] as const,
  registers: [registry],
});

const retryAfter = new Histogram({
  name: "acme_ratelimit_retry_after_seconds",
  help: "Retry-After the limiter asked refused callers to wait.",
  buckets: [1, 2, 5, 10, 30, 60],
  registers: [registry],
});

/// Records one decision. Keys are hashed to their first eight hex digits so
/// the label set stays bounded and no API key lands in Prometheus.
export function observe(key: string, result: TakeResult): void {
  decisions.inc({ key: shortHash(key), outcome: result.ok ? "allowed" : "limited" });
  if (!result.ok) retryAfter.observe(result.retryAfterMs / 1000);
}

function shortHash(key: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < key.length; i++) h = Math.imul(h ^ key.charCodeAt(i), 0x01000193) >>> 0;
  return h.toString(16).padStart(8, "0");
}
`;

const ACME_WEB_ADR_0007 = `# ADR-0007: Retry policy for scheduled jobs

Status: accepted · Owners: Marco Ruiz, Priya Natarajan

## Context

The scheduler retried failed jobs immediately and forever. During the July billing
outage this turned one failing webhook into 40k retries an hour and starved
unrelated jobs.

## Decision

- Retries use exponential backoff with full jitter: delay = random(0, base * 2^attempt),
  base 250 ms, capped at 5 minutes.
- Jobs get at most five attempts; the sixth failure parks the job in jobs_dead with
  the last error.
- Transient errors (network, 5xx, lock timeouts) are retried; validation errors are
  parked at once.
- Jitter and sleeping go through injectable random and sleep functions so tests can
  pin them.

## Consequences

- A dead-letter table and a small admin endpoint to requeue parked jobs.
- Tests must not depend on wall-clock timing; use fake timers and an injected clock.
- Alerting moves from "retry rate" to "parked jobs per hour".
`;

const CHANGELOG_BASE = `# Changelog

## Unreleased

## v2.14.1

- Scheduler retry test no longer depends on wall-clock time; CI's one-in-five flake is gone. (#487)
- CI restores the npm cache dropped in #483; \`install\` is back under a minute. (#486)

## v2.14.0

### Breaking
- **Scheduler retries are bounded.** Jobs stop after five attempts and are parked in \`jobs_dead\` with the last error; anything that relied on unbounded retries needs the new requeue endpoint. (#471, #473)

### API
- Token-bucket rate limiter scaffolding behind \`RATE_LIMIT_ENABLED\`; off by default, no behaviour change yet. (#472)
- \`/healthz\` now reports \`version\` from the image label instead of \`dev\`. (#469)

### Auth
- Browser sessions within five minutes of expiry get \`401 { refresh: true }\` so the client refreshes before a request fails mid-flight. Bearer callers are unaffected. (#467)

### Billing
- Stripe webhook deliveries are idempotent by event id; a retried delivery returns \`{ received: true, duplicate: true }\`. (#475)
`;

const CHANGELOG_UNRELEASED_ENTRY = `### API
- **Per-API-key rate limits on \`/api\`** (${ISSUE}). Each API key gets 600 requests per minute by default (\`RATE_LIMIT_PER_KEY\`), with a burst of one minute's worth; browser sessions are bucketed per org. Over the limit, requests get \`429\` with \`Retry-After\` in seconds and \`RateLimit-Limit\` / \`RateLimit-Remaining\` headers. Integrations that retry on 5xx only should also honour 429. (#491, #492)
`;

const CHANGELOG_WITH_ENTRY = CHANGELOG_BASE.replace("## Unreleased\n", `## Unreleased\n\n${CHANGELOG_UNRELEASED_ENTRY}`);

const RUNBOOK_INCIDENT = `# Incident response

## First five minutes

1. Acknowledge the page in #ops-incidents and say who is driving.
2. Check /healthz on both regions and the scheduler lag panel.
3. If the scheduler is the problem, pause it (POST /admin/scheduler/pause) before
   anything else; parked jobs are cheaper than duplicate side effects.

## Rollback

- Every deploy is a tagged image; roll back with the previous tag, not a rebuild.
- Database migrations are forward-only. If a migration is the cause, ship a fix
  forward and say so in the incident doc.

## Afterwards

Write the timeline the same day. Action items go into Linear with the incident id
in the title, and the review happens within a week.
`;

const RUNBOOK_DEPLOY = `# Deploy checklist

Before promoting a release tag to production:

- [ ] Release notes merged under docs/releases/ (Release Scribe opens the PR on Friday).
- [ ] Staging soaked for at least four hours with no new Sentry issues.
- [ ] Any migration in the release has run on staging and its duration is noted.
- [ ] Scheduler dead-letter count is flat.
- [ ] Someone other than the deployer has read the notes and can roll back.

Promote with: gh workflow run promote.yml -f tag=vX.Y.Z

Watch /healthz, the error rate, and scheduler lag for fifteen minutes. If any of them
moves, roll back first and investigate second.
`;

const RUNBOOK_PIPELINE = `# Feature pipeline

A Linear issue becomes pull requests without a person in the loop until review:

1. **intake** — the \`ready-for-build\` label fires the Linear webhook. Intake reads the
   issue and the code, writes \`/specs/<ISSUE>-<slug>.md\`, and emits \`feature.spec\`.
2. **planner** — splits the spec into independent tasks in \`/specs/<ISSUE>-plan.md\`
   and emits one \`task.ready\` per task.
3. **implementer** — one keyed session per task (\`task.id\`), each in its own Incus
   sandbox that closes with the session. Sub-agents: repo-explorer (read-only),
   test-writer. Opens the PR and emits \`pr.opened\`.
4. **ci-watch** — polls Actions every two minutes; a failure on a task branch is
   steered into the implementer's running task and told to pr-reviewer.
5. **pr-reviewer** — reviews on \`pr.opened\` and on GitHub \`synchronize\`; emits
   \`release.note\` for customer-visible changes and \`review.approved\` back to intake.
6. **release-scribe** — keeps CHANGELOG.md's Unreleased section current; Friday's
   notes PR starts from it.

Humans merge. Nothing in the pipeline pushes to main.

## When it stalls

- A task thread with no PR after an hour: open the implementer's thread; a
  provisioning failure shows as the run waiting on \`await_environment_ready\`.
- A \`run_failed\` on pr-reviewer is usually a provider error; replay the event from
  the Activity tab once the provider recovers.
- Linear returning 401 stops intake commenting but not the pipeline; the open
  question lands in the spec instead. Reconnect Linear on the Integrations page.
`;

const SPECS_README = `# specs

One file per feature: \`<ISSUE>-<slug>.md\` written by the intake bot, and
\`<ISSUE>-plan.md\` written by the planner. Both are the source of truth for what
the implementer builds; edit them here and replay the \`task.ready\` event to
rebuild a task.
`;

const SPEC_LIN_1421 = `# ${ISSUE}: Per-API-key rate limits

Status: ready for planning · Requested by Marco Ruiz · Priority: high

## Summary

Limit requests to \`/api\` per API key so one noisy integration cannot starve the
others in the same org. Over the limit, respond \`429\` with \`Retry-After\`.

## Context

- \`src/api/rate-limit.ts\` (#472) is a token bucket behind \`RATE_LIMIT_ENABLED\`,
  never mounted. Its refill runs on every request, which is the bug #482 shipped and
  reverted: a fast caller refills itself as quickly as it drains.
- \`src/index.ts:14\` mounts \`requireSession\` on \`/api\`; the limiter belongs after it,
  so unauthenticated requests are refused before they cost a token.
- API keys arrive as \`X-Api-Key\`; browser sessions carry no key.
- \`/webhooks/billing\` receives Stripe retries and must never be limited.

## Requirements

1. One bucket per API key: 600 requests per minute by default (\`RATE_LIMIT_PER_KEY\`),
   burst of one minute's worth (\`RATE_LIMIT_BURST\`). Refill is credited from whole
   elapsed intervals, never per request.
2. Requests without a key (browser sessions) share one bucket per org.
3. A refusal is \`429\` with \`Retry-After\` in whole seconds, plus \`RateLimit-Limit\`
   and \`RateLimit-Remaining\` on every response.
4. Metrics: decisions per key (hashed) by outcome, and a histogram of the
   \`Retry-After\` we hand out, on the existing Prometheus registry.
5. Documented for customers under \`docs/api/limits.md\` and for operators under
   \`docs/config.md\`.

## Non-goals

- Per-route limits (explicitly out of scope on the issue).
- The admin UI for per-key overrides (a later issue; the config shape must allow it).
- Limiting \`/webhooks/*\` or \`/healthz\`.

## Interface shared by the tasks

\`\`\`ts
export interface TakeResult {
  ok: boolean;
  remaining: number;
  retryAfterMs: number;
}
\`\`\`

## Acceptance criteria

- 1,000 requests with one key in one tick: the first 600 succeed, the rest are 429
  with \`Retry-After: 60\` or less.
- A second key is unaffected by the first key's refusals.
- With \`RATE_LIMIT_ENABLED=false\` nothing changes.
- Stripe webhook deliveries are never refused.

## Open questions

1. Does the limit apply to \`/api\` only, or to every authenticated route? The
   issue names "the API"; Stripe retries on \`/webhooks/billing\` would trip a global
   limit. **Assumption while unanswered: \`/api\` only.** Asked on the issue —
   the comment could not be posted (Linear returned 401; the workspace key was
   rotated), so the question lives here until someone reconnects Linear.
`;

const PLAN_LIN_1421 = `# ${ISSUE} plan

Three tasks, independent, buildable in parallel; each is a pull request against
main. They share the \`TakeResult\` type named in the spec.

## a — token bucket

- Branch \`lin-1421-a-bucket\` · PR title "ratelimit: token bucket keyed by API key (${ISSUE})"
- Files: \`src/ratelimit/bucket.ts\` (new), \`src/ratelimit/bucket.test.ts\` (new)
- \`TokenBuckets\` with \`take(key, cost)\` returning \`TakeResult\`; refill from whole
  elapsed intervals; injectable \`now\`; a \`sweep()\` that drops idle full buckets.
- Tests: burst then refusal; refill after one interval; a hot caller does not
  refill itself; two keys are independent; \`retryAfterMs\` arithmetic.
- Delete \`src/api/rate-limit.ts\` (#472) once this lands — a follow-up, not here.

## b — middleware and configuration

- Branch \`lin-1421-b-middleware\` · PR title "ratelimit: mount per-key limiter on /api (${ISSUE})"
- Files: \`src/ratelimit/middleware.ts\` (new), \`src/index.ts\` (mount after
  \`requireSession\`), \`docs/config.md\` (the two variables)
- \`X-Api-Key\` or \`org:<orgId>\` as the bucket key; 429 + \`Retry-After\` in whole
  seconds; \`RateLimit-*\` headers on every response. Reads \`RATE_LIMIT_ENABLED\`,
  \`RATE_LIMIT_PER_KEY\`, \`RATE_LIMIT_BURST\` once at start.
- Tests: under the limit passes; over the limit is 429 with the headers; disabled
  flag is a no-op; \`/webhooks/billing\` untouched.

## c — metrics and customer docs

- Branch \`lin-1421-c-metrics\` · PR title "ratelimit: decision metrics and customer docs (${ISSUE})"
- Files: \`src/ratelimit/metrics.ts\` (new), \`src/ratelimit/metrics.test.ts\` (new),
  \`docs/api/limits.md\` (new)
- \`observe(key, result)\` on the existing registry (\`src/metrics.ts\`): counter by
  hashed key and outcome, histogram of \`Retry-After\`. Task b calls it once both land.
- Docs: limits, headers, what to do on 429, the org bucket for browser sessions.

## Order

a and b can merge in either order; c wires into b in a two-line follow-up after
both are on main.
`;

function seedWorkspaces(store: DemoStore, universe: UniverseState): void {
  workspace(store, universe, {
    id: WORKSPACE.web,
    displayName: "acme-web",
    files: {
      "README.md": ACME_WEB_README,
      "CHANGELOG.md": CHANGELOG_WITH_ENTRY,
      "package.json": ACME_WEB_PACKAGE_JSON,
      "src/index.ts": ACME_WEB_INDEX_LIMITED,
      "src/auth/middleware.ts": ACME_WEB_MIDDLEWARE,
      "src/ratelimit/bucket.ts": RATELIMIT_BUCKET,
      "src/ratelimit/middleware.ts": RATELIMIT_MIDDLEWARE,
      "docs/adr-0007.md": ACME_WEB_ADR_0007,
    },
    revision: 21,
    createdAtMs: ago(60 * DAY_MS),
    updatedAtMs: p(14.5),
  });
  workspace(store, universe, {
    id: WORKSPACE.runbooks,
    displayName: "Runbooks",
    files: {
      "incident-response.md": RUNBOOK_INCIDENT,
      "deploy-checklist.md": RUNBOOK_DEPLOY,
      "feature-pipeline.md": RUNBOOK_PIPELINE,
    },
    revision: 6,
    createdAtMs: ago(45 * DAY_MS),
    updatedAtMs: ago(8 * DAY_MS),
  });
  workspace(store, universe, {
    id: WORKSPACE.specs,
    displayName: "Specs",
    files: {
      "README.md": SPECS_README,
      "LIN-1421-rate-limits.md": SPEC_LIN_1421,
      "LIN-1421-plan.md": PLAN_LIN_1421,
    },
    revision: 9,
    createdAtMs: ago(33 * DAY_MS),
    updatedAtMs: p(1.8),
  });
}

// ---------------------------------------------------------------------------
// Environments
// ---------------------------------------------------------------------------

const POWER_STATES: Environment["desiredPower"][] = ["running", "paused", "stopped"];

interface SandboxInit {
  id: string;
  displayName: string;
  session: string;
  createdAtMs: number;
  /// Closed with its session at this time; open (ready) when absent.
  closedAtMs?: number;
}

/// A task sandbox the implementer profile provisioned for one session.
function sandbox(init: SandboxInit): Environment {
  const requestId = `req-${hex(init.session, 12)}`;
  const updatedAtMs = init.closedAtMs ?? init.createdAtMs + 3 * MINUTE_MS;
  return {
    environmentId: init.id,
    requestId,
    source: { type: "provisioned", providerId: INCUS_PROVIDER_ID, bindingId: INCUS_PROVIDER_ID },
    displayName: init.displayName,
    status: init.closedAtMs === undefined ? "ready" : "closed",
    desiredPower: init.closedAtMs === undefined ? "running" : "stopped",
    idlePolicy: { pauseAfterMs: 15 * MINUTE_MS, stopAfterMs: 2 * HOUR_MS },
    incarnation: {
      incarnationId: `inc-${hex(`${init.id}-inc`, 10)}`,
      provisionRequestId: requestId,
      providerTargetId: `ls-${init.id.slice(4)}-${hex(init.id, 6)}`,
      templateId: "dev-small-v1",
      powerStates: POWER_STATES,
      createdAtMs: init.createdAtMs,
      updatedAtMs,
    },
    originSession: { sessionId: init.session, profileId: PROFILE.implementer, closeWithSession: true },
    publicIngressEnabled: false,
    metadata: { repo: "acme/acme-web", checkout: "main", issue: ISSUE },
    createdAtMs: init.createdAtMs,
    updatedAtMs,
  };
}

function seedEnvironments(universe: UniverseState): void {
  universe.providerBindings.push(
    providerBinding({
      revision: 2,
      metadata: { project: "acme", network: "lightspeed-envs" },
      createdAtMs: ago(50 * DAY_MS),
      updatedAtMs: ago(17 * DAY_MS),
    }),
  );
  universe.environmentTemplates.push(
    template({
      templateId: "dev-small-v1",
      displayName: "Development VM (small)",
      description: "2 vCPU / 4 GiB, Git, Node 22, Docker, envd. The default for task sandboxes.",
      publicIngress: true,
      deprecated: false,
      metadata: { cpu: "2", memory: "4GiB", disk: "40GiB", image: "acme/dev:2026.08" },
    }),
    template({
      templateId: "dev-large-v1",
      displayName: "Development VM (large)",
      description: "8 vCPU / 16 GiB, same image as small; for e2e suites and container builds.",
      publicIngress: true,
      deprecated: false,
      metadata: { cpu: "8", memory: "16GiB", disk: "120GiB", image: "acme/dev:2026.08" },
    }),
    template({
      templateId: "dev-small-v0",
      displayName: "Development VM (small, previous)",
      description: "Superseded image (Node 20); kept for environments that still run it.",
      publicIngress: false,
      deprecated: true,
      metadata: { cpu: "2", memory: "4GiB", disk: "40GiB", image: "acme/dev:2026.05" },
    }),
  );

  const ciRequestId = `req-${hex("ci-runner", 12)}`;
  universe.environments.set(ENV.ci, {
    environmentId: ENV.ci,
    requestId: ciRequestId,
    source: { type: "provisioned", providerId: INCUS_PROVIDER_ID, bindingId: INCUS_PROVIDER_ID },
    displayName: "CI runner",
    status: "ready",
    desiredPower: "running",
    idlePolicy: { pauseAfterMs: 30 * MINUTE_MS, stopAfterMs: 6 * HOUR_MS },
    incarnation: {
      incarnationId: `inc-${hex("ci-runner-inc", 10)}`,
      provisionRequestId: ciRequestId,
      providerTargetId: "ls-ci-runner-4e1d9b",
      templateId: "dev-small-v1",
      powerStates: POWER_STATES,
      createdAtMs: ago(30 * DAY_MS),
      updatedAtMs: p(13),
    },
    publicIngressEnabled: true,
    publicEndpoint: "https://ci-runner-4e1d9b.env.acme.example",
    metadata: { role: "ci", repo: "acme/acme-web", team: "platform" },
    createdAtMs: ago(30 * DAY_MS),
    updatedAtMs: p(13),
  });
  universe.environments.set(ENV.laptop, {
    environmentId: ENV.laptop,
    requestId: `req-${hex("priya-laptop", 12)}`,
    source: {
      type: "external",
      connection: { endpoint: "ws://priya-mbp.tailnet.acme.example:19091", transport: "webSocket" },
    },
    displayName: "Priya's laptop (envd)",
    status: "ready",
    desiredPower: "running",
    incarnation: {
      incarnationId: `inc-${hex("priya-laptop-inc", 10)}`,
      createdAtMs: ago(20 * DAY_MS),
      updatedAtMs: ago(25 * MINUTE_MS),
    },
    publicIngressEnabled: false,
    metadata: { owner: "priya", os: "macOS 15" },
    createdAtMs: ago(20 * DAY_MS),
    updatedAtMs: ago(25 * MINUTE_MS),
  });
  const oldRequestId = `req-${hex("old-sandbox", 12)}`;
  universe.environments.set(ENV.old, {
    environmentId: ENV.old,
    requestId: oldRequestId,
    source: { type: "provisioned", providerId: INCUS_PROVIDER_ID, bindingId: INCUS_PROVIDER_ID },
    displayName: "repo-explorer sandbox",
    status: "closed",
    desiredPower: "stopped",
    incarnation: {
      incarnationId: `inc-${hex("old-sandbox-inc", 10)}`,
      provisionRequestId: oldRequestId,
      providerTargetId: "ls-sandbox-9f8e7d",
      templateId: "dev-small-v0",
      powerStates: POWER_STATES,
      createdAtMs: ago(33 * DAY_MS + 2 * HOUR_MS),
      updatedAtMs: ago(33 * DAY_MS),
    },
    originSession: { sessionId: SESSION.specsSpike, profileId: PROFILE.explorer, closeWithSession: true },
    publicIngressEnabled: false,
    metadata: { repo: "acme/acme-web", checkout: "main" },
    createdAtMs: ago(33 * DAY_MS + 2 * HOUR_MS),
    updatedAtMs: ago(33 * DAY_MS),
  });
  universe.environments.set(ENV.taskA, sandbox({ id: ENV.taskA, displayName: "implementer sandbox · lin-1421-a", session: SESSION.taskA, createdAtMs: p(2.1), closedAtMs: p(4.9) }));
  universe.environments.set(ENV.taskB, sandbox({ id: ENV.taskB, displayName: "implementer sandbox · lin-1421-b", session: SESSION.taskB, createdAtMs: p(2.1), closedAtMs: p(12.4) }));
  universe.environments.set(ENV.taskC, sandbox({ id: ENV.taskC, displayName: "implementer sandbox · lin-1421-c", session: SESSION.taskC, createdAtMs: p(2.1) }));
  universe.environmentCredentials.push(
    {
      environmentId: ENV.ci,
      envName: "GITHUB_TOKEN",
      source: { type: "authGrant", grantId: GRANT.github },
      createdAtMs: ago(29 * DAY_MS),
      updatedAtMs: ago(29 * DAY_MS),
    },
    {
      environmentId: ENV.taskC,
      envName: "GITHUB_TOKEN",
      source: { type: "authGrant", grantId: GRANT.github },
      createdAtMs: p(2.1),
      updatedAtMs: p(2.1),
    },
  );
}

// ---------------------------------------------------------------------------
// MCP servers, secrets, GitHub App, models, setups
// ---------------------------------------------------------------------------

function seedIntegrations(universe: UniverseState): void {
  universe.mcpServers.set(
    "github",
    mcpServer({
      serverId: "github",
      displayName: "GitHub",
      serverUrl: "https://api.githubcopilot.com/mcp/",
      description: "GitHub's hosted MCP server, scoped to the acme organisation through the App installation.",
      allowedTools: [...new Set([...GITHUB_REVIEW_TOOLS, ...GITHUB_IMPLEMENT_TOOLS, ...GITHUB_CI_TOOLS, ...GITHUB_SCRIBE_TOOLS])],
      approvalDefault: "never",
      deferLoadingDefault: false,
      authPolicy: { type: "requiredBearer" },
      credential: { type: "authGrant", grantId: GRANT.github },
      status: "active",
      revision: 4,
      createdAtMs: ago(58 * DAY_MS),
      updatedAtMs: ago(9 * DAY_MS),
    }),
  );
  universe.mcpServers.set(
    "linear",
    mcpServer({
      serverId: "linear",
      displayName: "Linear",
      serverUrl: "https://mcp.linear.app/mcp",
      description: "Issues and comments for the intake bot. Needs a fresh OAuth connection since the workspace token was rotated on Monday.",
      allowedTools: ["list_issues", "get_issue", "create_issue", "create_comment", "update_issue"],
      authPolicy: { type: "requiredOAuth", resource: "https://mcp.linear.app/mcp", scopes: ["read", "write"] },
      status: "needsAuthConfig",
      revision: 2,
      createdAtMs: ago(31 * DAY_MS),
      updatedAtMs: ago(4 * DAY_MS),
    }),
  );

  const githubApp: GitHubApp = {
    providerId: "github",
    providerKind: "gitHubApp",
    displayName: "acme-lightspeed-app",
    config: { type: "githubApp", appId: "1187342", apiBaseUrl: "https://api.github.com" },
    hasCredential: true,
    status: "active",
    createdAtMs: ago(58 * DAY_MS),
    updatedAtMs: ago(58 * DAY_MS),
  };
  universe.githubApps.push(githubApp);

  const grants: SecretGrant[] = [
    {
      grantId: GRANT.github,
      providerId: "github",
      providerKind: "gitHubApp",
      displayName: "acme-lightspeed-app · acme-dev",
      subjectHint: "acme-dev",
      status: "active",
      exposure: "brokered",
      principal: { kind: "universeDefault" },
      scopes: [],
      hasAccessToken: true,
      hasRefreshToken: false,
      expiresAtMs: NOW + 47 * MINUTE_MS,
      lastLeasedAtMs: ago(13 * MINUTE_MS),
      leaseCount: 1_184,
      metadata: {
        installation_id: 48211937,
        account_login: "acme-dev",
        repository_selection: "selected",
        permissions: { contents: "write", pull_requests: "write", checks: "read", issues: "write", metadata: "read", actions: "read" },
      },
      createdAtMs: ago(58 * DAY_MS),
      updatedAtMs: ago(13 * MINUTE_MS),
    },
    {
      grantId: GRANT.linear,
      providerId: "linear",
      providerKind: "staticBearer",
      displayName: "Linear API key (acme workspace)",
      subjectHint: "ops@acme.example",
      status: "needsReauth",
      exposure: "brokered",
      principal: { kind: "user", id: "user-marco" },
      scopes: ["read", "write"],
      hasAccessToken: true,
      hasRefreshToken: false,
      lastLeasedAtMs: p(0.3),
      leaseCount: 212,
      metadata: { note: "Key rotated in Linear on Monday; intake's comments fail with 401 until the server is reconnected." },
      createdAtMs: ago(31 * DAY_MS),
      updatedAtMs: ago(4 * DAY_MS),
    },
    {
      grantId: GRANT.linearWebhook,
      providerId: "linear-webhooks",
      providerKind: "staticBearer",
      displayName: "Linear webhook signing secret",
      subjectHint: "intake · linear-webhook",
      status: "active",
      exposure: "brokered",
      principal: { kind: "universeDefault" },
      scopes: [],
      hasAccessToken: true,
      hasRefreshToken: false,
      lastLeasedAtMs: p(0),
      leaseCount: 96,
      metadata: {},
      createdAtMs: ago(31 * DAY_MS),
      updatedAtMs: ago(31 * DAY_MS),
    },
    {
      grantId: GRANT.anthropic,
      providerId: "model:anthropic",
      providerKind: "modelApiKey",
      displayName: "Anthropic API key",
      subjectHint: "software-factory (workspace key)",
      status: "active",
      exposure: "brokered",
      principal: { kind: "universeDefault" },
      hasAccessToken: true,
      hasRefreshToken: false,
      lastLeasedAtMs: ago(3 * HOUR_MS),
      leaseCount: 4_310,
      metadata: {},
      createdAtMs: ago(64 * DAY_MS),
      updatedAtMs: ago(64 * DAY_MS),
    },
  ];
  universe.secrets = {
    providers: [
      modelProvider("anthropic", "Anthropic", { type: "modelApiKey" }, true, ago(64 * DAY_MS), ago(64 * DAY_MS)),
      modelProvider("openai", "OpenAI", { type: "modelApiKey" }, true, ago(40 * DAY_MS), ago(40 * DAY_MS)),
      modelProvider(
        "ollama-lab",
        "Ollama (lab box)",
        { type: "modelEndpoint", endpoint: { baseUrl: "http://ollama.lab.internal:11434/v1", apiKinds: ["openai:completions"] } },
        false,
        ago(16 * DAY_MS),
        ago(16 * DAY_MS),
      ),
    ],
    grants,
  };

  const fetchedAtMs = ago(12 * MINUTE_MS);
  const efforts = ["none", "low", "medium", "high", "xhigh"];
  universe.models = {
    models: [
      modelOption(OPUS, "Claude Opus 5", { maxInputTokens: 1_000_000, maxOutputTokens: 128_000, parallelToolUse: true, reasoningEfforts: [...efforts, "max"] }, fetchedAtMs),
      modelOption(SONNET, "Claude Sonnet 5", { maxInputTokens: 1_000_000, maxOutputTokens: 64_000, parallelToolUse: true, reasoningEfforts: efforts }, fetchedAtMs),
      modelOption(
        { providerId: "anthropic", apiKind: "anthropic:messages", model: "claude-haiku-5" },
        "Claude Haiku 5",
        { maxInputTokens: 400_000, maxOutputTokens: 64_000, parallelToolUse: true, reasoningEfforts: efforts },
        fetchedAtMs,
      ),
      modelOption(GPT, "GPT-5.4", { maxInputTokens: 272_000, maxOutputTokens: 128_000, parallelToolUse: true, reasoningEfforts: efforts }, fetchedAtMs),
      modelOption(GPT_MINI, "GPT-5.4 mini", { maxInputTokens: 272_000, maxOutputTokens: 128_000, parallelToolUse: true, reasoningEfforts: ["none", "low", "medium", "high"] }, fetchedAtMs),
      modelOption(
        { providerId: "ollama-lab", apiKind: "openai:completions", model: "qwen3-coder:30b" },
        "Qwen3 Coder 30B (lab)",
        { maxInputTokens: 128_000, maxOutputTokens: 32_000, parallelToolUse: false, reasoningEfforts: null },
        fetchedAtMs,
      ),
    ],
    providers: [
      modelDiscovery("anthropic", ["anthropic:messages"], "configured", "universe", fetchedAtMs),
      modelDiscovery("openai", ["openai:responses"], "configured", "universe", fetchedAtMs),
      modelDiscovery("ollama-lab", ["openai:completions"], "notRequired", "none", fetchedAtMs),
    ],
  };

  const configurator: UniverseSetup = {
    id: "configurator",
    name: "Configurator",
    description: "An MCP server that lets coding agents configure this universe: profiles, MCP servers, environments, and bots, with an API key scoped to it.",
    version: 4,
    available: true,
    status: "ready",
    installedVersion: 4,
    resources: { keyPrefix: "lsk_acme_cfg_9b21", serverId: "configurator" },
  };
  universe.setups.push(configurator);
}

// ---------------------------------------------------------------------------
// Interactive sessions
// ---------------------------------------------------------------------------

const RETRY_TEST_BEFORE = `import { describe, expect, it, vi } from "vitest";
import { withRetry } from "./retry.js";

describe("withRetry", () => {
  it("returns the first successful result", async () => {
    const op = vi.fn().mockResolvedValue("ok");
    await expect(withRetry(op, { maxAttempts: 3, baseDelayMs: 10 })).resolves.toBe("ok");
    expect(op).toHaveBeenCalledTimes(1);
  });

  it("retries after a transient failure", async () => {
    const op = vi
      .fn()
      .mockRejectedValueOnce(Object.assign(new Error("ECONNRESET"), { transient: true }))
      .mockResolvedValueOnce("ok");
    const started = Date.now();
    const result = await withRetry(op, { maxAttempts: 3, baseDelayMs: 50 });
    expect(result).toBe("ok");
    expect(op).toHaveBeenCalledTimes(2);
    // full jitter: the delay is somewhere in [0, 100) ms
    expect(Date.now() - started).toBeLessThan(100);
  });
});
`;

const RETRY_TEST_AFTER = `import { afterEach, describe, expect, it, vi } from "vitest";
import { withRetry } from "./retry.js";

const transient = () => Object.assign(new Error("ECONNRESET"), { transient: true });

describe("withRetry", () => {
  afterEach(() => vi.useRealTimers());

  it("returns the first successful result", async () => {
    const op = vi.fn().mockResolvedValue("ok");
    await expect(withRetry(op, { maxAttempts: 3, baseDelayMs: 10 })).resolves.toBe("ok");
    expect(op).toHaveBeenCalledTimes(1);
  });

  it("retries after a transient failure", async () => {
    vi.useFakeTimers();
    const op = vi.fn().mockRejectedValueOnce(transient()).mockResolvedValueOnce("ok");
    const sleep = vi.fn((ms: number) => vi.advanceTimersByTimeAsync(ms).then(() => undefined));
    const pending = withRetry(op, { maxAttempts: 3, baseDelayMs: 50, random: () => 0.5, sleep });
    await vi.runAllTimersAsync();
    await expect(pending).resolves.toBe("ok");
    expect(op).toHaveBeenCalledTimes(2);
    // attempt 0 asks for base * 2^0 * 0.5 = 25 ms of sleep; wall time never enters
    expect(sleep).toHaveBeenCalledWith(25);
  });

  it("gives up after maxAttempts and parks the error", async () => {
    vi.useFakeTimers();
    const op = vi.fn().mockRejectedValue(transient());
    const sleep = vi.fn(() => Promise.resolve());
    const pending = withRetry(op, { maxAttempts: 3, baseDelayMs: 50, random: () => 0, sleep });
    await expect(pending).rejects.toMatchObject({ attempts: 3, parked: true });
    expect(op).toHaveBeenCalledTimes(3);
  });
});
`;

const VITEST_SCHEDULER_OUTPUT = [
  " RUN  v3.0.5 /workspace/acme-web",
  "",
  " ✓ src/scheduler/retry.test.ts (3 tests) 14ms",
  " ✓ src/scheduler/backoff.test.ts (5 tests) 9ms",
  " ✓ src/scheduler/scheduler.test.ts (7 tests) 212ms",
  "",
  " Test Files  3 passed (3)",
  "      Tests  15 passed (15)",
  "   Duration  1.38s",
].join("\n");

const SCHEDULER_AUDIT_TASK =
  "Audit src/scheduler for code that depends on wall-clock time (Date.now, setTimeout, setInterval, performance.now, Math.random) outside the injected clock and random. Report file:line, what it is used for, and whether tests cover it. Do not change anything.";

const SCHEDULER_AUDIT_REPORT = [
  "Audited src/scheduler (4 files). Wall-clock or randomness outside the injected hooks:",
  "",
  "- src/scheduler/scheduler.ts:88 — `Date.now()` stamps `lockedAt` on a claimed job. Used for logging and the stale-lock sweep threshold; not a scheduling decision. Covered indirectly by scheduler.test.ts (`releases stale locks`).",
  "- src/scheduler/backoff.ts:12 — `Math.random` is the default `random`. Every caller injects one except `Scheduler` itself (scheduler.ts:31), whose tests pass `random: () => 0`.",
  "",
  "No `setTimeout`, `setInterval`, or `performance.now` anywhere in the directory; sleeping goes through the injected `sleep`. Recommendation: thread `clock.now()` through scheduler.ts:88 for consistency. Not a flake risk.",
].join("\n");

function seedSessions(store: DemoStore, universe: UniverseState): void {
  // Fix a flaky test: two runs, a sub-agent, the shared CI runner.
  const flaky = newSession(store, universe, {
    id: SESSION.flaky,
    displayName: "Fix flaky scheduler test",
    metadata: {
      ...EVALUATION_METADATA,
      harborSessionId: "fix-flaky-scheduler-test__CGQoz7Q__lightspeed-agent",
      task: "repair-nondeterministic-retry-timing-and-audit-wall-clock-dependencies",
      trial: "trial-20260904-113000-000042-attempt-03",
    },
    config: EXPLORER_CONFIG,
    instructions: EXPLORER_INSTRUCTIONS,
    activeEnvironmentId: ENV.ci,
    createdAtMs: at(2, 14, 2),
  });
  appendExchange(store, universe, flaky, {
    at: at(2, 14, 3),
    user: "The scheduler test `retries after a transient failure` fails about one run in five on CI but never locally. Can you find out why and fix it?",
    turn: {
      thinking:
        "**Narrowing down a timing flake**\n\nOne-in-five on CI and never locally points at wall-clock timing rather than logic: CI runners are slower and noisier. I'll find the test, then read it next to the retry policy to see whether it waits on real timers.",
      tools: [
        runCommand(["rg", "-n", "retries after a transient failure", "src"], 'src/scheduler/retry.test.ts:11:  it("retries after a transient failure", async () => {'),
        readFile("src/scheduler/retry.test.ts", RETRY_TEST_BEFORE),
      ],
      text: [
        "Found it. `retries after a transient failure` (src/scheduler/retry.test.ts:11) asserts on **real elapsed time**: it expects the whole call to finish in under 100 ms, but with full jitter the first retry sleeps anywhere in `[0, 100)` ms *plus* whatever the runner adds. On a busy CI box an ~80 ms draw plus scheduling latency crosses 100 ms about a fifth of the time; locally it never gets close.",
        "",
        "The assertion is also testing the wrong thing — the wall clock instead of the retry policy. `withRetry` already takes injectable `random` and `sleep` functions (ADR-0007 asks for exactly that), so the fix is to pin the jitter and use fake timers:",
        "",
        "- `vi.useFakeTimers()` and drive the run with `vi.runAllTimersAsync()`",
        "- pass `random: () => 0.5` so the first delay is a known 25 ms",
        "- assert on the sleep the policy *asked for*, not on `Date.now()`",
        "",
        "Want me to apply that and run the scheduler suite?",
      ].join("\n"),
    },
  });
  const flakyRun = appendExchange(store, universe, flaky, {
    at: at(2, 14, 11),
    user: "Yes, apply it and run the scheduler suite. Also have a sub-agent check whether anything else in src/scheduler depends on wall-clock time.",
    turn: {
      thinking:
        "**Applying the fix and delegating the audit**\n\nRewrite the test with fake timers and a pinned random, run the scheduler tests in the CI runner, and hand the wall-clock audit to repo-explorer so it runs in parallel.",
      tools: [
        writeFile("src/scheduler/retry.test.ts", RETRY_TEST_AFTER, "fake timers, pinned jitter, dead-letter case"),
        runCommand(["npx", "vitest", "run", "src/scheduler"], VITEST_SCHEDULER_OUTPUT),
        agentRun(PROFILE.explorer, SCHEDULER_AUDIT_TASK, SCHEDULER_AUDIT_REPORT),
      ],
      text: [
        "Done — the scheduler suite is green (15 tests, 1.4 s) and the test now checks the retry policy instead of the runner's mood.",
        "",
        "```diff",
        "-    const started = Date.now();",
        "-    const result = await withRetry(op, { maxAttempts: 3, baseDelayMs: 50 });",
        "+    vi.useFakeTimers();",
        "+    const sleep = vi.fn((ms: number) => vi.advanceTimersByTimeAsync(ms).then(() => undefined));",
        "+    const pending = withRetry(op, { maxAttempts: 3, baseDelayMs: 50, random: () => 0.5, sleep });",
        "+    await vi.runAllTimersAsync();",
        '+    await expect(pending).resolves.toBe("ok");',
        "     expect(op).toHaveBeenCalledTimes(2);",
        "-    // full jitter: the delay is somewhere in [0, 100) ms",
        "-    expect(Date.now() - started).toBeLessThan(100);",
        "+    // attempt 0 asks for base * 2^0 * 0.5 = 25 ms of sleep; wall time never enters",
        "+    expect(sleep).toHaveBeenCalledWith(25);",
        "```",
        "",
        "I also added `gives up after maxAttempts and parks the error`, since the file had no coverage for the dead-letter path from ADR-0007.",
        "",
        "The sub-agent's audit found nothing else that can flake: the only other wall-clock read is `lockedAt` in `scheduler.ts:88`, which is log-only, and the only bare `Math.random` is the default that every caller overrides. It suggests threading `clock.now()` through scheduler.ts:88 for consistency — a one-line follow-up, not part of this fix.",
        "",
        "The change is uncommitted in the CI runner's checkout; say the word and I'll open a PR. (Marco did: #487.)",
      ].join("\n"),
    },
  });

  // The sub-agent the run above delegated to: one shot, closed when done.
  const audit = subagentSession(store, universe, {
    id: SESSION.audit,
    displayName: "Audit src/scheduler for wall-clock dependencies",
    profile: EXPLORER_PROFILE,
    parent: flaky,
    parentRunId: flakyRun.id,
    root: SESSION.flaky,
    depth: 1,
    environmentId: ENV.ci,
    createdAtMs: at(2, 14, 12),
  });
  appendExchange(store, universe, audit, {
    at: at(2, 14, 12) + 5_000,
    user: SCHEDULER_AUDIT_TASK,
    turn: {
      thinking: "**Scoping the audit**\n\nOne search for every wall-clock and randomness call in the directory, then read the one file where the default matters.",
      tools: [
        runCommand(
          ["rg", "-n", "Date\\.now\\(|setTimeout|setInterval|performance\\.now|Math\\.random", "src/scheduler"],
          "src/scheduler/scheduler.ts:88:      lockedAt: Date.now(),\nsrc/scheduler/backoff.ts:12:  random = Math.random,",
        ),
        readFile(
          "src/scheduler/backoff.ts",
          [
            "export interface BackoffOptions {",
            "  baseDelayMs: number;",
            "  maxDelayMs?: number;",
            "  random?: () => number;",
            "}",
            "",
            "/// Full jitter per ADR-0007: random(0, base * 2^attempt), capped.",
            "export function backoffDelay(",
            "  attempt: number,",
            "  { baseDelayMs, maxDelayMs = 300_000, random = Math.random }: BackoffOptions,",
            "): number {",
            "  const ceiling = Math.min(maxDelayMs, baseDelayMs * 2 ** attempt);",
            "  return Math.floor(random() * ceiling);",
            "}",
          ].join("\n"),
        ),
      ],
      text: SCHEDULER_AUDIT_REPORT,
    },
  });
  closeAt(audit, at(2, 14, 14));

  // Explain code: a read and a markdown answer, from Priya's own laptop.
  const auth = newSession(store, universe, {
    id: SESSION.auth,
    displayName: "Explain the auth middleware",
    config: EXPLORER_CONFIG,
    instructions: EXPLORER_INSTRUCTIONS,
    activeEnvironmentId: ENV.laptop,
    createdAtMs: at(5, 11, 20),
  });
  appendExchange(store, universe, auth, {
    at: at(5, 11, 21),
    user: "Explain how src/auth/middleware.ts decides whether a request is authenticated. I'm adding a service-to-service caller and want to know which path it should use.",
    turn: {
      thinking: "**Reading the middleware before answering**\n\nThe question hinges on the cookie-versus-bearer split and the early-expiry rule, so I'll read the file rather than guess.",
      tools: [readFile("src/auth/middleware.ts", ACME_WEB_MIDDLEWARE)],
      text: [
        "`requireSession` accepts two credentials that carry the **same signed claims** and differ only in transport:",
        "",
        "1. **Bearer token** — `Authorization: Bearer <jwt>`; checked first.",
        "2. **Session cookie** — `acme_session`; used only when there is no bearer header.",
        "",
        'Either way the token is verified with `jose` against `SESSION_SECRET` (issuer `acme-web`, 30 s clock tolerance), and the claims become `c.get("session")`.',
        "",
        'The one asymmetry is the **early-expiry rule**: a *cookie* within five minutes of `exp` is rejected with `{ error: "session expiring", refresh: true }` so the browser refreshes before a request fails mid-flight. Bearer callers skip that rule and are accepted right up to `exp`.',
        "",
        "For your service-to-service caller: use the **bearer path**. Mint a JWT with `sub`, `email` (a service identity is fine), and `org` for the org it acts on, signed with the same secret. Two things I'd add while you're there:",
        "",
        '- put an `aud: "service"` claim in service tokens and check it here, so a leaked browser session can\'t be replayed as a service and vice versa;',
        "- keep service tokens short-lived and mint them per call — the middleware has no revocation list, so lifetime *is* the revocation story.",
        "",
        "One more thing now that LIN-1421 is in flight: a service caller without an `X-Api-Key` header lands in the org's shared rate-limit bucket. Give it its own key.",
      ].join("\n"),
    },
  });
  appendExchange(store, universe, auth, {
    at: at(5, 11, 40),
    user: "Should service tokens be long-lived then, to save the round trip?",
    turn: {
      text: [
        "No — keep them short (5–15 minutes) and mint them from a signing endpoint the service calls with its own credential. The round trip is one HTTP call per token, not per request, and it buys you the only revocation mechanism this middleware has: waiting for expiry.",
        "",
        "If the extra call really matters, cache the token in the caller until `exp - 60s`. That keeps the blast radius of a leaked token at minutes while costing one signing call every ten minutes or so.",
      ].join("\n"),
    },
  });

  // Investigate CI: a failed fetch, then recovery through the GitHub tools.
  const ci = newSession(store, universe, {
    id: SESSION.ci,
    displayName: "Why is CI slow this week?",
    config: REVIEWER_CONFIG,
    instructions: REVIEWER_INSTRUCTIONS,
    activeEnvironmentId: ENV.ci,
    createdAtMs: at(3, 9, 48),
  });
  appendExchange(store, universe, ci, {
    at: at(3, 9, 49),
    user: "CI has felt slow all week. Can you tell me what changed? The status board is at https://ci.acme.example/status",
    turn: {
      thinking: "**Checking the status board first**\n\nThe board should have per-job timings for the week; if it does not answer, the GitHub workflow runs carry the same numbers.",
      tools: [webFetch("https://ci.acme.example/status", "CI status board", "request failed: upstream returned 502 Bad Gateway (2 attempts, 30 s each)", true)],
      text: "The status board isn't answering — its upstream returned 502 on both attempts — so I can't use it for timings. I can get the same picture from the last two weeks of `ci.yml` workflow runs through GitHub: per-job durations, and it doesn't depend on the board being up. Shall I go that way?",
    },
  });
  appendExchange(store, universe, ci, {
    at: at(3, 9, 53),
    user: "Yes, go ahead.",
    turn: {
      thinking: "**Comparing this week with last**\n\nPull `ci.yml` runs for both weeks and compare per-job medians rather than totals, so a single slow job stands out instead of averaging away.",
      tools: [
        github(
          "list_workflow_runs",
          { owner: "acme", repo: "acme-web", workflow_id: "ci.yml", branch: "main", per_page: 100 },
          "ci.yml on main, last 14 days",
          [
            "run_id  created            conclusion  duration",
            "9140    (this week)        failure     8m 51s",
            "9138    (this week)        success     9m 04s",
            "9134    (this week)        failure     8m 47s",
            "9131    (this week)        cancelled   2m 10s",
            "9129    (this week)        success     8m 58s",
            "9127    (last week)        failure     6m 22s",
            "9125    (last week)        success     6m 09s",
            "9122    (last week)        success     6m 14s",
            "… 42 more runs",
          ].join("\n"),
        ),
        github(
          "list_workflow_jobs",
          { owner: "acme", repo: "acme-web", run_id: 9138 },
          "job timings, run 9138 vs 9125",
          "job                 9125 (last week)   9138 (this week)\ninstall             41s                2m 58s\nlint                48s                51s\ntest (node 22)      3m 10s             3m 22s\nbuild image         2m 05s             2m 11s",
        ),
      ],
      text: [
        "Everything is flat except **`install`**, which went from about 40 s to about 3 minutes starting Monday:",
        "",
        "| job | last week (median) | this week (median) |",
        "|---|---|---|",
        "| install | 41 s | 2 m 58 s |",
        "| lint | 48 s | 51 s |",
        "| test (node 22) | 3 m 10 s | 3 m 22 s |",
        "| build image | 2 m 05 s | 2 m 11 s |",
        "",
        "Monday is when #483 bumped `actions/setup-node` to v5 and, in the same edit, dropped the `cache: npm` line from `.github/workflows/ci.yml`. Every run now does a cold `npm ci`. Restoring `cache: npm` (or an explicit `actions/cache` keyed on `package-lock.json`) should bring `install` back to under a minute — that's ~2 minutes off every run, which matches the slowdown people are feeling.",
        "",
        "Want me to open a PR that restores the cache line?",
      ].join("\n"),
    },
  });

  // The spike that moved specs into a workspace, a month ago; its sandbox
  // closed with it and is the deprecated environment on the list.
  const spike = newSession(store, universe, {
    id: SESSION.specsSpike,
    displayName: "Move specs out of the wiki into a workspace",
    config: EXPLORER_CONFIG,
    instructions: EXPLORER_INSTRUCTIONS,
    activeEnvironmentId: ENV.old,
    createdAtMs: ago(33 * DAY_MS + 2 * HOUR_MS),
  });
  appendExchange(store, universe, spike, {
    at: ago(33 * DAY_MS + 2 * HOUR_MS - 60_000),
    user: "We keep feature specs in the wiki and the bots can't read them. Export the ones tagged `active` to markdown so we can put them in a Lightspeed workspace, and tell me what a naming convention should look like.",
    turn: {
      thinking:
        "**Export first, convention second**\n\nThe wiki's API gives page bodies as markdown already; the work is picking a stable file name and keeping the issue key in it so a bot can find a spec from a Linear event.",
      tools: [
        runCommand(
          ["wiki-export", "--tag", "active", "--format", "markdown", "--out", "/tmp/specs"],
          "exported 7 pages to /tmp/specs\n  LIN-1388-billing-ledger.md\n  LIN-1395-cookie-sessions.md\n  LIN-1402-retry-after-header.md\n  … 4 more",
        ),
        runCommand(
          ["ls", "-la", "/tmp/specs"],
          "total 64\n-rw-r--r-- 1 dev dev  4812 LIN-1388-billing-ledger.md\n-rw-r--r-- 1 dev dev  3190 LIN-1395-cookie-sessions.md\n-rw-r--r-- 1 dev dev  2710 LIN-1402-retry-after-header.md\n…",
        ),
      ],
      text: [
        "Exported seven active specs to `/tmp/specs` as markdown, ready to upload into a `specs` workspace.",
        "",
        "Convention that keeps a bot one string away from the right file: `<ISSUE>-<slug>.md` for the spec and `<ISSUE>-plan.md` for the plan next to it. The issue key is what every Linear event carries, so intake can write and the planner can read without a lookup table. Keep a `README.md` in the workspace stating exactly that, and you're done.",
      ].join("\n"),
    },
  });
  closeAt(spike, ago(33 * DAY_MS));
}

// ---------------------------------------------------------------------------
// Bots: the LIN-1421 pipeline
// ---------------------------------------------------------------------------

const REPO = { owner: "acme", repo: "acme-web" };
const ISSUE_URL = "https://linear.app/acme/issue/LIN-1421/per-api-key-rate-limits";
const ISSUE_DESCRIPTION =
  "Customers with several integrations share one org-level limit today (#482, reverted). We need limits per API key so one noisy integration cannot starve the others: 600 req/min per key by default, overridable per key from the admin UI later, 429 with Retry-After, and metrics so we can see who hits the ceiling. Out of scope: per-route limits and the admin UI.";

interface Task {
  id: string;
  title: string;
  branch: string;
  pr: number;
  prTitle: string;
  head: string;
}
const TASK: Record<"a" | "b" | "c", Task> = {
  a: { id: "lin-1421-a", title: "token bucket", branch: "lin-1421-a-bucket", pr: 491, prTitle: `ratelimit: token bucket keyed by API key (${ISSUE})`, head: "a71c4e2" },
  b: { id: "lin-1421-b", title: "middleware and configuration", branch: "lin-1421-b-middleware", pr: 492, prTitle: `ratelimit: mount per-key limiter on /api (${ISSUE})`, head: "3f2a9c1" },
  c: { id: "lin-1421-c", title: "metrics and customer docs", branch: "lin-1421-c-metrics", pr: 493, prTitle: `ratelimit: decision metrics and customer docs (${ISSUE})`, head: "c0d5b88" },
};
const CI_RUN_492 = 9187;

const CI_LOG_9187 = [
  "FAIL  src/ratelimit/middleware.test.ts > rateLimit > passes requests under the limit",
  "AssertionError: expected 429 to be 200",
  " ❯ src/ratelimit/middleware.test.ts:19:31",
  "",
  "  RateLimit-Limit: NaN",
  "  RateLimit-Remaining: NaN",
  "",
  "FAIL  src/ratelimit/middleware.test.ts > rateLimit > refuses the 6th request with Retry-After (same cause)",
  "FAIL  src/ratelimit/middleware.test.ts > rateLimit > buckets browser sessions per org (same cause)",
  "",
  " Test Files  1 failed | 12 passed (13)",
  "      Tests  3 failed | 71 passed (74)",
].join("\n");

/// One `pr.opened` from the implementer as pr-reviewer's inbox admits it.
function prOpened(log: EventLog, task: Task, atMs: number, session: { sessionId: string; label: string }, outcome: BotEventOutcome, detail: string, resolvedAfterMs: number): ScriptedEvent {
  return log.add({
    kind: "pr.opened",
    source: `bot:${BOT.implementer}`,
    at: atMs,
    summary: `#${task.pr} ${task.prTitle} — task ${task.id}, branch ${task.branch}`,
    eventId: `bot:${BOT.implementer}:${hex(`pr.opened:${task.pr}`, 12)}`,
    session,
    sender: BOT.implementer,
    hops: 3,
    outcome,
    detail,
    resolvedAfterMs,
    data: { pr: task.pr, issue: ISSUE, task: { id: task.id }, branch: task.branch, head: task.head, url: `https://github.com/acme/acme-web/pull/${task.pr}` },
  });
}

/// A GitHub `pull_request` webhook delivery.
function pullRequestEvent(
  log: EventLog,
  action: "opened" | "synchronize",
  task: Task,
  atMs: number,
  session: { sessionId: string; label: string },
  outcome: BotEventOutcome | null,
  detail: string,
  resolvedAfterMs: number,
  extra: { author?: string } = {},
): ScriptedEvent {
  const delivery = uuidLike(`gh:${task.pr}:${action}:${atMs}`);
  return log.add({
    kind: `pull_request.${action}`,
    source: "webhook:github-webhook",
    at: atMs,
    summary: `GitHub pull_request.${action} in acme/acme-web: #${task.pr} ${task.prTitle}`,
    body: [`action: ${action}`, `sender: ${extra.author ?? "lightspeed-factory[bot]"}`, `head: ${task.branch} → main (${task.head})`],
    eventId: `webhook:github-webhook:${delivery}`,
    session,
    outcome,
    detail,
    resolvedAfterMs,
    data: {
      action,
      repository: "acme/acme-web",
      sender: extra.author ?? "lightspeed-factory[bot]",
      pull_request: { number: task.pr, title: task.prTitle, head: { ref: task.branch, sha: task.head }, base: { ref: "main" }, draft: false },
    },
  });
}

const introduce = (store: DemoStore, universe: UniverseState, session: SessionRecord, atMs: number, text: string): void => {
  appendExchange(store, universe, session, { at: atMs, user: INTRODUCTION_PROMPT, turn: { text } });
};

const commit = (branch: string, sha: string, message: string, files: string): DemoToolCall =>
  runCommand(
    ["sh", "-c", `git add -A && git commit -m "${message}" && git push -u origin ${branch}`],
    `[${branch} ${sha}] ${message}\n ${files}\n * [new branch]      ${branch} -> ${branch}\nbranch '${branch}' set up to track 'origin/${branch}'.`,
    { detail: `commit ${sha} and push` },
  );

function seedBots(store: DemoStore, universe: UniverseState): void {
  seedIntake(store, universe);
  seedPlanner(store, universe);
  seedImplementer(store, universe);
  seedPrReviewer(store, universe);
  seedCiWatch(store, universe);
  seedReleaseScribe(store, universe);
}

// --- intake ----------------------------------------------------------------

const RATE_LIMIT_472 = `// Behind RATE_LIMIT_ENABLED; not mounted anywhere yet (#472).
export class RateLimiter {
  private tokens = new Map<string, number>();
  constructor(private readonly limit: number, private readonly refillPerRequest: number) {}

  allow(key: string): boolean {
    const current = Math.min(this.limit, (this.tokens.get(key) ?? this.limit) + this.refillPerRequest);
    if (current < 1) return false;
    this.tokens.set(key, current - 1);
    return true;
  }
}
`;

function seedIntake(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.intake,
    displayName: "Intake",
    description: "Turns a Linear issue labeled ready-for-build into a spec in /specs and hands it to planner; Monday backlog digest.",
    profileId: PROFILE.intake,
    brief: [
      "You are Intake for the acme-web feature pipeline.",
      "",
      "An issue.labeled event from Linear means the issue is ready to build: read it and the code, write /specs/<ISSUE>-<slug>.md, and emit feature.spec to planner with a reply requested. A review.approved from pr-reviewer is status for you: note it in your thread, nothing to write. The Monday backlog is a short digest of issues labeled ready-for-build that have no spec yet, with what is missing from each.",
    ].join("\n"),
    runsPerDay: 40,
    breaker: { fires: 10, windowMs: 10 * MINUTE_MS },
    routedSessionCloseAfterMs: 7 * DAY_MS,
    emit: true,
    createdAtMs: ago(31 * DAY_MS),
    updatedAtMs: ago(6 * DAY_MS),
  });
  const triggers = [
    webhookTrigger(
      universe,
      BOT.intake,
      "linear-webhook",
      { token: LINEAR_WEBHOOK_TOKEN, verification: { scheme: "hmac-sha256", grantId: GRANT.linearWebhook, header: "linear-signature" } },
      {
        filter: 'data.type == "Issue" && "ready-for-build" in data.data.labels[*].name',
        route: { policy: "perKey", key: "data.identifier" },
        deliver: { whenBusy: "queue" },
        createdAtMs: ago(31 * DAY_MS),
        updatedAtMs: ago(20 * DAY_MS),
      },
    ),
    inboxTrigger(BOT.intake, [BOT.planner, BOT.reviewer], {
      route: { policy: "perKey", key: "data.issue" },
      deliver: { whenBusy: "queue" },
      createdAtMs: ago(30 * DAY_MS),
    }),
    scheduleTrigger(
      BOT.intake,
      "weekly-backlog",
      { cron: "30 8 * * 1", summary: "Monday backlog: issues labeled ready-for-build without a spec, and what each is missing." },
      { route: { policy: "bot" }, deliver: { whenBusy: "queue" }, createdAtMs: ago(30 * DAY_MS) },
    ),
  ];

  const main = managedSession(store, universe, {
    id: SESSION.intakeMain,
    botId: BOT.intake,
    displayName: "Intake",
    profile: INTAKE_PROFILE,
    tools: [...BOT_TOOLS, EMIT_TOOL],
    createdAtMs: ago(31 * DAY_MS),
  });
  introduce(
    store,
    universe,
    main,
    ago(31 * DAY_MS) + 30_000,
    "I'm Intake: when a Linear issue gets the ready-for-build label I read it, write the spec into /specs, and hand it to planner. I wake on the Linear webhook, on planner's and pr-reviewer's replies, and on Monday mornings for the backlog; I have the Linear tools and read-write access to the specs workspace. One gap: the acme-web link is read-only, which is right, but I have no environment — fine for specs, say so if you ever want me to run anything.",
  );
  const thread1418 = managedSession(store, universe, {
    id: SESSION.intake1418,
    botId: BOT.intake,
    displayName: "Intake · LIN-1418",
    profile: INTAKE_PROFILE,
    tools: [...BOT_TOOLS, EMIT_TOOL],
    createdAtMs: at(5, 10, 14),
  });
  const thread1421 = managedSession(store, universe, {
    id: SESSION.intake1421,
    botId: BOT.intake,
    displayName: `Intake · ${ISSUE}`,
    profile: INTAKE_PROFILE,
    tools: [...BOT_TOOLS, EMIT_TOOL],
    createdAtMs: p(0),
  });
  const mainRef = { sessionId: SESSION.intakeMain, label: "Main" };
  const ref1418 = { sessionId: SESSION.intake1418, label: "LIN-1418" };
  const ref1421 = { sessionId: SESSION.intake1421, label: ISSUE };

  const log = eventLog(store, BOT.intake);
  const backlog = (atMs: number, outcome: BotEventOutcome, detail: string): ScriptedEvent =>
    log.add({
      kind: "schedule",
      source: "schedule:weekly-backlog",
      at: atMs,
      summary: "Monday backlog: issues labeled ready-for-build without a spec, and what each is missing.",
      body: ["cron: 30 8 * * 1 (Europe/Berlin)"],
      eventId: `schedule:weekly-backlog:${atIso(atMs).slice(0, 10)}`,
      session: mainRef,
      outcome,
      detail,
      resolvedAfterMs: 70_000,
    });
  const issueLabeled = (
    issue: { identifier: string; title: string; description: string; url: string },
    atMs: number,
    session: { sessionId: string; label: string },
    outcome: BotEventOutcome,
    detail: string,
    resolvedAfterMs: number,
  ): ScriptedEvent =>
    log.add({
      kind: "issue.labeled",
      source: "webhook:linear-webhook",
      at: atMs,
      summary: `${issue.identifier} ${issue.title} — labeled ready-for-build by Marco Ruiz`,
      body: ["priority: High", "assignee: Marco Ruiz", `url: ${issue.url}`, `description: ${issue.description}`],
      eventId: `webhook:linear-webhook:${uuidLike(`linear:${issue.identifier}:${atMs}`)}`,
      session,
      outcome,
      detail,
      resolvedAfterMs,
      data: {
        action: "update",
        type: "Issue",
        data: {
          identifier: issue.identifier,
          title: issue.title,
          description: issue.description,
          url: issue.url,
          priorityLabel: "High",
          state: { name: "Todo" },
          assignee: { name: "Marco Ruiz" },
          labels: [{ name: "ready-for-build" }, { name: "api" }],
        },
        updatedFrom: { labelIds: [] },
      },
    });

  const e1 = backlog(at(11, 8, 30), "handled", "4 ready-for-build issues without a spec; LIN-1409 and LIN-1412 have no acceptance criteria — commented on both.");
  const e2 = issueLabeled(
    {
      identifier: "LIN-1418",
      title: "Return Retry-After on 429 responses",
      description: "Clients retry blindly when we rate-limit them. Send Retry-After.",
      url: "https://linear.app/acme/issue/LIN-1418/return-retry-after-on-429-responses",
    },
    at(5, 10, 14),
    ref1418,
    "ignored",
    "Duplicate of LIN-1402, shipped in v2.14.1 — commented, removed the label, nothing to build.",
    55_000,
  );
  const e3 = backlog(at(4, 8, 30), "handled", "Linear returned 401 on list_issues: the workspace key was rotated. No digest this week; told Marco to reconnect the linear server.");
  const e4 = expectSeq(
    issueLabeled(
      { identifier: ISSUE, title: "Per-API-key rate limits", description: ISSUE_DESCRIPTION, url: ISSUE_URL },
      p(0),
      ref1421,
      "handled",
      `Spec at ${SPEC_PATH} with one open question (Linear 401 blocked the comment); handed to planner (#${SEQ.plannerSpec} there).`,
      span(0, 1),
    ),
    4,
  );
  const e5 = expectSeq(
    receipt(log, {
      from: BOT.planner,
      askedSeq: SEQ.plannerSpec,
      status: "handled",
      summary: `Plan at ${PLAN_PATH}: three independent tasks sent to implementer`,
      at: p(2),
      hops: 2,
      session: ref1421,
      outcome: "handled",
      detail: "planner split LIN-1421 into three tasks; waiting on pr-reviewer for status.",
    }),
    SEQ.intakeReceipt,
  );
  const e6 = expectSeq(
    log.add({
      kind: "review.approved",
      source: `bot:${BOT.reviewer}`,
      at: p(13.5),
      summary: `${ISSUE}: #491 (task a) and #492 (task b) approved; task c not yet opened`,
      eventId: `bot:${BOT.reviewer}:${hex("review.approved:LIN-1421", 12)}`,
      session: ref1421,
      sender: BOT.reviewer,
      hops: 1,
      outcome: "handled",
      detail: "Two of three PRs approved and waiting on a human merge; #493 still to come.",
      resolvedAfterMs: 25_000,
      data: { issue: ISSUE, approved: [491, 492], remaining: ["lin-1421-c"] },
    }),
    SEQ.intakeApproved,
  );

  e1.envelope.runId = appendScriptedRun(store, main, {
    at: at(11, 8, 30),
    user: e1.prompt,
    steps: [
      {
        tools: [
          linear(
            "list_issues",
            { filter: { labels: ["ready-for-build"], state: "Todo" } },
            "ready-for-build, Todo",
            "LIN-1409 Bulk export for projects — no acceptance criteria\nLIN-1412 Webhook replay from the admin UI — no acceptance criteria\nLIN-1415 Org-level audit log — spec drafted last week (/specs/LIN-1415-audit-log.md)\nLIN-1416 Rotate session secret without logout — ready",
          ),
        ],
      },
      {
        text: "Backlog: four issues carry ready-for-build without a spec. LIN-1415 has one from last week and is waiting on planner; LIN-1416 is complete and small. LIN-1409 and LIN-1412 have no acceptance criteria, so I commented on both asking for what done looks like rather than guessing.",
      },
    ],
  }).id;
  e2.envelope.runId = appendScriptedRun(store, thread1418, {
    at: at(5, 10, 14),
    user: e2.prompt,
    steps: [
      {
        thinking: "**Retry-After already exists**\n\nThe v2.14.1 changelog mentions Retry-After from LIN-1402. Read the issue and check before writing a spec for something that shipped.",
        tools: [linear("get_issue", { id: "LIN-1402" }, "LIN-1402", "LIN-1402 Retry-After header on 429 — Done (2026-08-11) — shipped in #478, v2.14.1")],
      },
      {
        tools: [
          linear(
            "create_comment",
            {
              issueId: "LIN-1418",
              body: "This shipped in v2.14.1 as LIN-1402 (#478): every 429 from /api carries Retry-After in seconds. Closing as a duplicate; reopen if you see a 429 without it.",
            },
            "duplicate of LIN-1402",
            "comment created: https://linear.app/acme/issue/LIN-1418#comment-9c1f",
          ),
          linear("update_issue", { id: "LIN-1418", removeLabels: ["ready-for-build"], state: "Duplicate" }, "remove label, mark duplicate", "LIN-1418 updated: state Duplicate, labels [api]"),
        ],
        text: "Nothing to build: LIN-1418 asks for what LIN-1402 shipped in v2.14.1. Commented with the PR, marked it a duplicate, and removed the label so it does not wake me again.",
      },
    ],
  }).id;
  e3.envelope.runId = appendScriptedRun(store, main, {
    at: at(4, 8, 30),
    user: e3.prompt,
    steps: [
      {
        tools: [linear("list_issues", { filter: { labels: ["ready-for-build"], state: "Todo" } }, "ready-for-build, Todo", LINEAR_401, true)],
        text: "No digest this week: Linear refused the workspace token with 401 — the key was rotated. Until someone reconnects the `linear` server on the Integrations page I can still write specs from the webhook payload, but I cannot list or comment on issues. Told Marco.",
      },
    ],
  }).id;
  e4.envelope.runId = appendScriptedRun(store, thread1421, {
    at: p(0.2),
    user: e4.prompt,
    steps: [
      {
        thinking:
          "**Reading the code before the spec**\n\nThe issue names the outcome, not the mechanism. #472 left a limiter behind a flag and #482 tried to mount a per-org one and was reverted; read both so the spec builds on what exists instead of inventing a third.",
        tools: [
          vfsListDir("/acme-web/src", ["api/", "auth/", "billing/", "scheduler/", "db.ts", "index.ts", "metrics.ts"]),
          vfsReadFile("/acme-web/src/index.ts", ACME_WEB_INDEX),
          vfsReadFile("/acme-web/src/api/rate-limit.ts", RATE_LIMIT_472),
        ],
      },
      {
        thinking:
          "**One question worth asking**\n\nThe #472 limiter refills on every request — that is the bug #482 shipped, and the spec must say so. What the issue does not settle: does the limit cover /api only, or every authenticated route? Stripe retries on /webhooks/billing would trip a global limit. Ask on the issue; proceed on the conservative reading.",
        tools: [
          linear(
            "create_comment",
            {
              issueId: ISSUE,
              body: "One question before planning: should the per-key limit apply to /api only, or to every authenticated route? Stripe retries on /webhooks/billing would trip a global limit. Proceeding with /api only unless you say otherwise.",
            },
            "ask: /api only?",
            LINEAR_401,
            true,
          ),
        ],
      },
      {
        tools: [
          vfsWriteFile(SPEC_PATH, SPEC_LIN_1421, "requirements, non-goals, shared type, one open question"),
          botEmit(
            {
              to: BOT.planner,
              kind: "feature.spec",
              summary: `${ISSUE} Per-API-key rate limits — spec at ${SPEC_PATH}, one open question (assumption stated)`,
              data: { issue: ISSUE, spec: SPEC_PATH, openQuestions: 1, priority: "high" },
              reply: true,
            },
            SEQ.plannerSpec,
          ),
        ],
        text: [
          `Spec written to \`${SPEC_PATH}\` and handed to planner (their #${SEQ.plannerSpec}). It builds on what is in the repo: the #472 limiter refills on every request, which is exactly the bug that got #482 reverted, so the spec pins refill to whole elapsed intervals and names the \`TakeResult\` type the tasks will share.`,
          "",
          "One open question I could not ask: whether the limit covers `/api` only or every authenticated route (Stripe retries on `/webhooks/billing` would trip a global one). Linear returned 401 on the comment — the workspace key was rotated on Monday — so the question and the assumption I am proceeding on (`/api` only) are in the spec under *Open questions*. Reconnecting the `linear` server on the Integrations page fixes that for next time.",
        ].join("\n"),
      },
    ],
  }).id;
  e5.envelope.runId = appendExchange(store, universe, thread1421, {
    at: p(2.05),
    user: e5.prompt,
    turn: {
      text: `planner split ${ISSUE} into three tasks (bucket, middleware and config, metrics and docs) and sent each to implementer. Nothing for me now; pr-reviewer reports back when reviews land.`,
    },
  }).id;
  e6.envelope.runId = appendExchange(store, universe, thread1421, {
    at: p(13.6),
    user: e6.prompt,
    turn: {
      text: `Status for ${ISSUE}: #491 (bucket) and #492 (middleware) are approved and wait on a human merge; task c is still building. The open question in the spec stands — nobody has answered on Linear, and nothing so far depends on the answer.`,
    },
  }).id;

  universe.bots.set(BOT.intake, {
    bot: record,
    triggers: new Map(triggers.map((entry) => [entry.triggerId, entry])),
    events: log.events,
    state: botState({
      bot: record,
      sessions: [botSession(main, "main"), botSession(thread1421, "keyed", ISSUE), botSession(thread1418, "keyed", "LIN-1418")],
      recentEvents: [
        recent(e3.envelope, { inputTokens: 6_120, cachedInputTokens: 5_480 }),
        recent(e4.envelope, { inputTokens: 21_840, cachedInputTokens: 17_900 }),
        recent(e5.envelope, { inputTokens: 23_100, cachedInputTokens: 22_700 }),
        recent(e6.envelope, { inputTokens: 23_600, cachedInputTokens: 23_200 }),
      ],
      eventsProcessed: 37,
      appliedProfileRevision: INTAKE_PROFILE.revision,
      runsToday: 3,
    }),
    descendants: [],
  });
}

// --- planner ---------------------------------------------------------------

function seedPlanner(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.planner,
    displayName: "Planner",
    description: "Splits a feature.spec from intake into independent tasks in /specs and sends one task.ready per task to implementer.",
    profileId: PROFILE.planner,
    brief: [
      "You are Planner for the acme-web feature pipeline.",
      "",
      "A feature.spec from intake means: read the spec, write /specs/<ISSUE>-plan.md with tasks that can be built in parallel, and send implementer one task.ready per task with a reply requested. A bot.reply from implementer is a task finishing: note it in one line. Never write code and never re-plan a task that has a pull request open.",
    ].join("\n"),
    runsPerDay: 30,
    breaker: { fires: 10, windowMs: 10 * MINUTE_MS },
    emit: true,
    createdAtMs: ago(31 * DAY_MS),
    updatedAtMs: ago(5 * DAY_MS),
  });
  const triggers = [
    inboxTrigger(BOT.planner, [BOT.intake, BOT.implementer], { route: { policy: "bot" }, deliver: { whenBusy: "queue" }, createdAtMs: ago(31 * DAY_MS) }),
    scheduleTrigger(
      BOT.planner,
      "nightly-replan",
      { cron: "0 2 * * *", summary: "Re-read every open plan against main and note tasks that no longer apply." },
      {
        route: { policy: "bot" },
        deliver: { whenBusy: "queue" },
        enabled: false,
        disabledReason: "operator",
        disabledAtMs: ago(5 * DAY_MS - HOUR_MS),
        createdAtMs: ago(28 * DAY_MS),
        updatedAtMs: ago(5 * DAY_MS - HOUR_MS),
      },
    ),
  ];
  const main = managedSession(store, universe, {
    id: SESSION.plannerMain,
    botId: BOT.planner,
    displayName: "Planner",
    profile: PLANNER_PROFILE,
    tools: [...BOT_TOOLS, EMIT_TOOL],
    createdAtMs: ago(31 * DAY_MS),
  });
  introduce(
    store,
    universe,
    main,
    ago(31 * DAY_MS) + 90_000,
    "I'm Planner: I take intake's specs and split them into tasks the implementer can build in parallel, each with its files, tests, branch, and PR title, written to /specs/<ISSUE>-plan.md. I wake on intake's feature.spec and on implementer's replies, and nightly to re-read open plans; I have read-write access to specs and read-only access to acme-web, no environment, no code tools — which is right. Nothing unclear.",
  );
  const mainRef = { sessionId: SESSION.plannerMain, label: "Main" };

  const log = eventLog(store, BOT.planner);
  const nightly = (atMs: number, outcome: BotEventOutcome, detail: string): ScriptedEvent =>
    log.add({
      kind: "schedule",
      source: "schedule:nightly-replan",
      at: atMs,
      summary: "Re-read every open plan against main and note tasks that no longer apply.",
      body: ["cron: 0 2 * * * (Europe/Berlin)"],
      eventId: `schedule:nightly-replan:${atIso(atMs).slice(0, 10)}`,
      session: mainRef,
      outcome,
      detail,
      resolvedAfterMs: 50_000,
    });
  const e1 = nightly(at(6, 2, 0), "handled", "LIN-1415 plan: task b's file moved in #481 — updated the path; nothing else changed.");
  const e2 = nightly(at(5, 2, 0), "ignored", "No open plan changed against main since yesterday.");
  const e3 = expectSeq(
    log.add({
      kind: "feature.spec",
      source: `bot:${BOT.intake}`,
      at: p(1),
      summary: `${ISSUE} Per-API-key rate limits — spec at ${SPEC_PATH}, one open question (assumption stated)`,
      eventId: `bot:${BOT.intake}:${hex("feature.spec:LIN-1421", 12)}`,
      session: mainRef,
      sender: BOT.intake,
      hops: 1,
      outcome: "handled",
      detail: `Three independent tasks in ${PLAN_PATH}; task.ready sent to implementer (#${SEQ.implTaskA}–#${SEQ.implTaskC} there).`,
      resolvedAfterMs: span(1, 2),
      data: { issue: ISSUE, spec: SPEC_PATH, openQuestions: 1, priority: "high" },
    }),
    SEQ.plannerSpec,
  );
  const receiptFor = (askedSeq: number, atMs: number, summary: string, detail: string): ScriptedEvent =>
    receipt(log, { from: BOT.implementer, askedSeq, status: "handled", summary, at: atMs, hops: 3, session: mainRef, outcome: "handled", detail });
  const e4 = expectSeq(
    receiptFor(SEQ.implTaskA, p(3.4), `PR #${TASK.a.pr} opened: TokenBuckets with whole-interval refill, 5 tests green`, "Task a done: #491 open and with pr-reviewer."),
    SEQ.plannerReceiptA,
  );
  const e5 = expectSeq(
    receiptFor(SEQ.implTaskB, p(11.5), `PR #${TASK.b.pr} opened; the CI failure steered in mid-run was the config default — fixed in 3f2a9c1`, "Task b done: #492 open; one CI round-trip on the way."),
    SEQ.plannerReceiptB,
  );
  const e6 = expectSeq(
    receiptFor(
      SEQ.implTaskC,
      p(20.2),
      `PR #${TASK.c.pr} opened: metrics and customer docs; test-writer's histogram tests still pending`,
      "Task c done: #493 open. All three PRs exist; the plan is complete.",
    ),
    SEQ.plannerReceiptC,
  );

  e1.envelope.runId = appendScriptedRun(store, main, {
    at: at(6, 2, 0),
    user: e1.prompt,
    steps: [
      {
        tools: [
          vfsListDir("/specs", ["README.md", "LIN-1415-audit-log.md", "LIN-1415-plan.md"]),
          vfsReadFile(
            "/specs/LIN-1415-plan.md",
            "# LIN-1415 plan\n\n## a — audit table and writer\n- Files: src/audit/log.ts (new), migrations/0045_audit.sql\n\n## b — admin read endpoint\n- Files: src/admin/audit.ts (new) …",
          ),
        ],
        text: "One plan open (LIN-1415). Task b named `src/admin/routes.ts`, which #481 moved to `src/admin/index.ts`; updated the path. Nothing else drifted.",
      },
    ],
  }).id;
  e2.envelope.runId = appendExchange(store, universe, main, { at: at(5, 2, 0), user: e2.prompt, turn: { text: "No open plan changed against main since yesterday's pass; nothing to edit." } }).id;
  e3.envelope.runId = appendScriptedRun(store, main, {
    at: p(1.2),
    user: e3.prompt,
    steps: [
      {
        thinking:
          "**Splitting by file, not by layer**\n\nRead the spec, then the source layout, and cut where the files do not overlap: the bucket is pure logic with its own tests; the middleware plus config touches index.ts; metrics and docs touch neither. Three tasks that can merge in any order, sharing only the TakeResult type the spec already names.",
        tools: [
          vfsReadFile(SPEC_PATH, SPEC_LIN_1421),
          vfsListDir("/acme-web/src", ["api/", "auth/", "billing/", "scheduler/", "db.ts", "index.ts", "metrics.ts"]),
          vfsReadFile("/acme-web/package.json", ACME_WEB_PACKAGE_JSON),
        ],
      },
      {
        tools: [
          vfsWriteFile(PLAN_PATH, PLAN_LIN_1421, "three independent tasks, shared TakeResult"),
          ...(["a", "b", "c"] as const).map((key) =>
            botEmit(
              {
                to: BOT.implementer,
                kind: "task.ready",
                summary: `${ISSUE} task ${TASK[key].id}: ${TASK[key].title} — plan at ${PLAN_PATH}`,
                data: { issue: ISSUE, task: { id: TASK[key].id, title: TASK[key].title, branch: TASK[key].branch, prTitle: TASK[key].prTitle }, plan: PLAN_PATH, spec: SPEC_PATH },
                reply: true,
              },
              SEQ[key === "a" ? "implTaskA" : key === "b" ? "implTaskB" : "implTaskC"],
            ),
          ),
        ],
        text: [
          `Plan at \`${PLAN_PATH}\`: three tasks, independent, each a PR against main.`,
          "",
          "- **a — token bucket**: `src/ratelimit/bucket.ts` + tests. Refill credited from whole elapsed intervals, injectable clock, a `sweep()`.",
          "- **b — middleware and configuration**: `src/ratelimit/middleware.ts`, the mount in `src/index.ts` after `requireSession`, `docs/config.md`. 429 with `Retry-After` and the `RateLimit-*` headers.",
          "- **c — metrics and customer docs**: `src/ratelimit/metrics.ts` on the existing registry, `docs/api/limits.md`.",
          "",
          `They share only the \`TakeResult\` type from the spec. Sent task.ready for each to implementer (their #${SEQ.implTaskA}, #${SEQ.implTaskB}, #${SEQ.implTaskC}); the two-line wiring of c into b is a follow-up once both are on main. The spec's open question (\`/api\` only) does not change the split either way.`,
        ].join("\n"),
      },
    ],
  }).id;
  e4.envelope.runId = appendExchange(store, universe, main, {
    at: p(3.45),
    user: e4.prompt,
    turn: { text: `Task a is done: #${TASK.a.pr} open, tests green, pr-reviewer has it. Two tasks still building.` },
  }).id;
  e5.envelope.runId = appendExchange(store, universe, main, {
    at: p(11.55),
    user: e5.prompt,
    turn: {
      text: `Task b is done: #${TASK.b.pr} open after one CI round-trip on the config default — the plan asked for the variables to be read once at start, and a default was implied, not written; I will say "with a default" next time. Task c remains.`,
    },
  }).id;
  e6.envelope.runId = appendExchange(store, universe, main, {
    at: p(20.25),
    user: e6.prompt,
    turn: { text: `Task c is done: #${TASK.c.pr} open. All three ${ISSUE} pull requests exist; the plan is complete and the two-line wiring of c into b waits for both to merge.` },
  }).id;

  universe.bots.set(BOT.planner, {
    bot: record,
    triggers: new Map(triggers.map((entry) => [entry.triggerId, entry])),
    events: log.events,
    state: botState({
      bot: record,
      sessions: [botSession(main, "main")],
      recentEvents: [
        recent(e3.envelope, { inputTokens: 19_400, cachedInputTokens: 15_100 }),
        recent(e4.envelope, { inputTokens: 26_800, cachedInputTokens: 26_300 }),
        recent(e5.envelope, { inputTokens: 27_300, cachedInputTokens: 26_900 }),
        recent(e6.envelope, { inputTokens: 27_900, cachedInputTokens: 27_500 }),
      ],
      eventsProcessed: 24,
      appliedProfileRevision: PLANNER_PROFILE.revision,
      runsToday: 4,
    }),
    descendants: [],
  });
}

// --- implementer -----------------------------------------------------------

const EXPLORE_BUCKET_TASK = "What rate-limiting code exists in this repository today? Report the file, its interface, how it refills, and whether anything imports it. Do not change anything.";
const EXPLORE_BUCKET_REPORT = [
  "One file: src/api/rate-limit.ts (#472), 14 lines, exported class `RateLimiter` with `allow(key): boolean`. Nothing imports it — `rg RateLimiter src` finds only the definition and its own test.",
  "",
  "Refill happens on every call (`rate-limit.ts:7`: tokens + refillPerRequest before the check), so a caller that calls often refills as fast as it drains; the test (`rate-limit.test.ts`) only checks the first refusal and never sends more than `limit` requests, which is why it passes. There is no clock in the class at all.",
  "",
  "Nothing to build on: the plan's `TokenBuckets` is a replacement, not an extension. The `X-Api-Key` header is not read anywhere yet.",
].join("\n");

const CONVENTIONS_TASK = "How do the existing tests in this repository fake time? Name the pattern to follow for a class that takes an injectable `now` clock.";
const CONVENTIONS_REPORT = [
  "Two patterns, both in src/scheduler:",
  "",
  "- `retry.test.ts:16` uses `vi.useFakeTimers()` with `vi.runAllTimersAsync()` for code that sleeps, and pins jitter with an injected `random`.",
  "- `scheduler.test.ts:22` injects a clock object (`{ now: () => t }`) and moves `t` by hand; no fake timers at all, because the code under test never sleeps.",
  "",
  "For a class with an injectable `now`, the second pattern is the one: a `let t = 0` and `new TokenBuckets({ …, now: () => t })`, then `t += 60_000` between takes. `afterEach(() => vi.useRealTimers())` is only needed if fake timers are used.",
].join("\n");

const BUCKET_TEST = `import { describe, expect, it } from "vitest";
import { TokenBuckets } from "./bucket.js";

function buckets(capacity = 5, refill = 5) {
  let t = 0;
  const b = new TokenBuckets({ capacity, refill, intervalMs: 60_000, now: () => t });
  return { b, tick: (ms: number) => (t += ms) };
}

describe("TokenBuckets", () => {
  it("admits a burst of capacity, then refuses with a Retry-After", () => {
    const { b } = buckets();
    for (let i = 0; i < 5; i++) expect(b.take("k1").ok).toBe(true);
    const refused = b.take("k1");
    expect(refused).toEqual({ ok: false, remaining: 0, retryAfterMs: 60_000 });
  });

  it("credits a whole interval at once", () => {
    const { b, tick } = buckets();
    for (let i = 0; i < 5; i++) b.take("k1");
    tick(59_999);
    expect(b.take("k1").ok).toBe(false);
    tick(1);
    expect(b.take("k1")).toMatchObject({ ok: true, remaining: 4 });
  });

  it("does not let a hot caller refill itself", () => {
    const { b, tick } = buckets();
    for (let i = 0; i < 5; i++) b.take("k1");
    for (let i = 0; i < 1_000; i++) {
      tick(10);
      expect(b.take("k1").ok).toBe(false);
    }
  });

  it("keeps keys independent", () => {
    const { b } = buckets();
    for (let i = 0; i < 5; i++) b.take("k1");
    expect(b.take("k1").ok).toBe(false);
    expect(b.take("k2")).toMatchObject({ ok: true, remaining: 4 });
  });

  it("sweeps keys that have been full for an interval", () => {
    const { b, tick } = buckets();
    b.take("idle");
    tick(120_001);
    b.take("busy");
    expect(b.sweep()).toBe(1);
  });
});
`;

const TESTS_BUCKET_TASK =
  "Write vitest coverage for src/ratelimit/bucket.ts (TokenBuckets): burst then refusal, whole-interval refill, a hot caller cannot refill itself, keys are independent, and sweep(). Use the injected clock; no real timers. Run the file and report.";
const TESTS_BUCKET_REPORT =
  "Wrote src/ratelimit/bucket.test.ts: 5 tests, all using the injected `now` (repo-explorer confirmed that is the scheduler tests' pattern for clock-taking classes). `npx vitest run src/ratelimit/bucket.test.ts`: 5 passed in 21 ms. One observation, not a failure: `take()` with `cost > capacity` can never succeed and returns a finite retryAfterMs; the plan does not ask for a cost above 1, so I left it.";

const VITEST_BUCKET_OUTPUT = " RUN  v3.0.5 /workspace/acme-web\n\n ✓ src/ratelimit/bucket.test.ts (5 tests) 21ms\n\n Test Files  1 passed (1)\n      Tests  5 passed (5)\n   Duration  0.62s";

const EXPLORE_CONFIG_TASK =
  "How does this repository read configuration from the environment, and where is middleware mounted on /api? Report file:line and the conventions to follow. Do not change anything.";
const EXPLORE_CONFIG_REPORT = [
  "Configuration is read straight from `process.env` at module load, never through a config module: `src/index.ts:26` (`PORT`), `src/auth/middleware.ts:18` (`SESSION_SECRET`), `src/scheduler/scheduler.ts:14` (`SCHEDULER_POLL_MS`, with `Number(process.env.… ?? 1000)`). No validation anywhere; the `?? default` idiom is the only guard.",
  "",
  'Middleware on `/api` is mounted once, `src/index.ts:14`: `app.basePath("/api").use(requireSession)`. `use` takes several handlers, so `.use(requireSession, rateLimit)` keeps the order explicit. `/webhooks/billing` and `/healthz` are registered on `app` directly (`index.ts:11–12`) and are not under the base path.',
  "",
  'Tests set env before importing (`src/scheduler/scheduler.test.ts:6`, `process.env.SCHEDULER_POLL_MS = "10"` above the dynamic `import()`); follow that for `RATE_LIMIT_*`.',
].join("\n");

const MIDDLEWARE_TEST = `import { beforeAll, describe, expect, it } from "vitest";
import { Hono } from "hono";

process.env.RATE_LIMIT_PER_KEY = "5";
const { rateLimit } = await import("./middleware.js");

function app() {
  const a = new Hono();
  a.use("*", async (c, next) => {
    c.set("session", { user: { id: "u1", email: "u@acme.example" }, orgId: "org-1", expiresAt: 0 });
    await next();
  });
  a.use("/api/*", rateLimit);
  a.get("/api/me", (c) => c.json({ ok: true }));
  return a;
}

describe("rateLimit", () => {
  it("passes requests under the limit", async () => {
    const res = await app().request("/api/me", { headers: { "x-api-key": "key-1" } });
    expect(res.status).toBe(200);
    expect(res.headers.get("RateLimit-Remaining")).toBe("4");
  });

  it("refuses the 6th request with Retry-After", async () => {
    const a = app();
    for (let i = 0; i < 5; i++) await a.request("/api/me", { headers: { "x-api-key": "key-2" } });
    const res = await a.request("/api/me", { headers: { "x-api-key": "key-2" } });
    expect(res.status).toBe(429);
    expect(Number(res.headers.get("Retry-After"))).toBeLessThanOrEqual(60);
  });

  it("buckets browser sessions per org", async () => {
    const res = await app().request("/api/me");
    expect(res.status).toBe(200);
    expect(res.headers.get("RateLimit-Limit")).toBe("5");
  });

  it("defaults to 600 per minute when RATE_LIMIT_PER_KEY is unset", async () => {
    const { readConfig } = await import("./middleware.js");
    expect(readConfig({}).perMinute).toBe(600);
    expect(() => readConfig({ RATE_LIMIT_PER_KEY: "0" })).toThrow(/positive integer/);
  });
});
`;
/// The test file as first pushed: the default case came with the CI fix.
const MIDDLEWARE_TEST_DRAFT = MIDDLEWARE_TEST.replace(/\n  it\("defaults to 600[\s\S]*?\n  }\);\n/, "\n");

const CONFIG_DOC = `# Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| \`PORT\` | \`3000\` | HTTP port |
| \`SESSION_SECRET\` | — | HMAC key for session tokens (required) |
| \`RATE_LIMIT_ENABLED\` | \`true\` | Set to \`false\` to disable the per-key limiter on \`/api\` |
| \`RATE_LIMIT_PER_KEY\` | \`600\` | Requests per minute per API key; browser sessions share one bucket per org |
| \`RATE_LIMIT_BURST\` | \`RATE_LIMIT_PER_KEY\` | Requests a quiet key may spend at once |

All variables are read once at start; a change needs a restart.
`;

const LIMITS_DOC = `# Rate limits

Every API key may make **600 requests per minute** to \`/api\`. A key that has been
quiet can spend up to a minute's worth at once; after that, tokens refill at the
per-minute rate. Browser sessions have no key and share one bucket per organisation.

Every response carries:

- \`RateLimit-Limit\` — the key's limit per minute
- \`RateLimit-Remaining\` — requests left before a refusal

A refused request is a \`429\` with \`Retry-After\` in whole seconds and a JSON body
\`{ "error": "rate limited", "retryAfterMs": … }\`. Wait for \`Retry-After\` before
retrying; retrying sooner does not refill the bucket.

Webhooks we call you with, and the webhooks you send us (\`/webhooks/*\`), are never
limited. Per-key overrides are not self-service yet — ask support.
`;

const METRICS_TS = `import { Registry, collectDefaultMetrics } from "prom-client";

/// One registry for the process; /metrics serves it (index.ts).
export const registry = new Registry();
collectDefaultMetrics({ register: registry, prefix: "acme_" });
`;

const METRICS_TEST = `import { beforeEach, describe, expect, it } from "vitest";
import { registry } from "../metrics.js";
import { observe } from "./metrics.js";

describe("ratelimit metrics", () => {
  beforeEach(() => registry.resetMetrics());

  it("counts decisions by outcome under a hashed key", async () => {
    observe("key-1", { ok: true, remaining: 4, retryAfterMs: 0 });
    observe("key-1", { ok: false, remaining: 0, retryAfterMs: 30_000 });
    const text = await registry.metrics();
    expect(text).not.toContain("key-1");
    expect(text).toMatch(/acme_ratelimit_decisions_total\\{key="[0-9a-f]{8}",outcome="allowed"\\} 1/);
    expect(text).toMatch(/acme_ratelimit_decisions_total\\{key="[0-9a-f]{8}",outcome="limited"\\} 1/);
  });

  it("observes Retry-After only for refusals", async () => {
    observe("key-2", { ok: true, remaining: 1, retryAfterMs: 0 });
    observe("key-2", { ok: false, remaining: 0, retryAfterMs: 4_000 });
    const text = await registry.metrics();
    expect(text).toContain("acme_ratelimit_retry_after_seconds_count 1");
    expect(text).toContain('acme_ratelimit_retry_after_seconds_bucket{le="5"} 1');
  });
});
`;

const TESTS_METRICS_TASK =
  "Write vitest coverage for src/ratelimit/metrics.ts: decisions are counted by outcome under a hashed key (the raw key never appears in the exposition), and the Retry-After histogram is observed only for refusals. Use the shared registry from src/metrics.ts and reset it between tests. Run the file and report.";

function seedImplementer(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.implementer,
    displayName: "Implementer",
    description: "Builds one task per thread in a fresh sandbox with repo-explorer and test-writer sub-agents, opens the PR, and tells pr-reviewer.",
    profileId: PROFILE.implementer,
    brief: [
      "You are Implementer for the acme-web feature pipeline. Each task.ready from planner is one thread and one sandbox.",
      "",
      "Read the plan and the spec, use repo-explorer before writing and test-writer for the tests, run the affected tests, open the PR with the issue key in its title, and emit pr.opened to pr-reviewer with a reply requested. A ci.failure steered into your run is a job that failed on your branch: read the log, fix it in the same PR, say what it was. A bot.reply from pr-reviewer is the review result: act on requested changes in the same PR; an approval needs nothing from you.",
    ].join("\n"),
    runsPerDay: 60,
    breaker: { fires: 12, windowMs: 10 * MINUTE_MS },
    routedSessionCloseAfterMs: 3 * DAY_MS,
    emit: true,
    createdAtMs: ago(30 * DAY_MS),
    updatedAtMs: ago(2 * DAY_MS),
  });
  const triggers = [
    inboxTrigger(BOT.implementer, [BOT.planner, BOT.ci, BOT.reviewer], {
      route: { policy: "perKey", key: "data.task.id" },
      deliver: { whenBusy: "steer" },
      createdAtMs: ago(30 * DAY_MS),
      updatedAtMs: ago(2 * DAY_MS),
    }),
  ];
  const profileTools = [...BOT_TOOLS, EMIT_TOOL];
  const main = managedSession(store, universe, {
    id: SESSION.implMain,
    botId: BOT.implementer,
    displayName: "Implementer",
    profile: IMPLEMENTER_PROFILE,
    tools: profileTools,
    createdAtMs: ago(30 * DAY_MS),
  });
  introduce(
    store,
    universe,
    main,
    ago(30 * DAY_MS) + 60_000,
    "I'm Implementer: every task.ready from planner becomes its own thread with a fresh Incus sandbox (dev-small-v1, acme-web on main, closed when the thread closes) where I build the task, hand exploration to repo-explorer and tests to test-writer, and open the PR. I wake only on my inbox — planner's tasks, ci-watch's failures, pr-reviewer's replies — routed by task id, and a failure arriving while I work is steered into the run. GitHub App access covers PRs and job logs; nothing missing.",
  );
  const thread = (id: string, task: Task, environmentId: string, createdAtMs: number): SessionRecord =>
    managedSession(store, universe, { id, botId: BOT.implementer, displayName: `Implementer · ${task.id}`, profile: IMPLEMENTER_PROFILE, tools: profileTools, createdAtMs, environmentId });
  const taskA = thread(SESSION.taskA, TASK.a, ENV.taskA, p(2.1));
  const taskB = thread(SESSION.taskB, TASK.b, ENV.taskB, p(2.1) + 1_000);
  const taskC = thread(SESSION.taskC, TASK.c, ENV.taskC, p(2.1) + 2_000);
  const refA = { sessionId: SESSION.taskA, label: TASK.a.id };
  const refB = { sessionId: SESSION.taskB, label: TASK.b.id };
  const refC = { sessionId: SESSION.taskC, label: TASK.c.id };

  const log = eventLog(store, BOT.implementer);
  const taskReady = (task: Task, atMs: number, session: { sessionId: string; label: string }, detail: string, resolvedAfterMs: number): ScriptedEvent =>
    log.add({
      kind: "task.ready",
      source: `bot:${BOT.planner}`,
      at: atMs,
      summary: `${ISSUE} task ${task.id}: ${task.title} — plan at ${PLAN_PATH}`,
      eventId: `bot:${BOT.planner}:${hex(`task.ready:${task.id}`, 12)}`,
      session,
      sender: BOT.planner,
      hops: 2,
      outcome: "handled",
      detail,
      resolvedAfterMs,
      data: { issue: ISSUE, task: { id: task.id, title: task.title, branch: task.branch, prTitle: task.prTitle }, plan: PLAN_PATH, spec: SPEC_PATH },
    });
  const e1 = expectSeq(
    taskReady(TASK.a, p(2), refA, `PR #${TASK.a.pr} opened: TokenBuckets with whole-interval refill, 5 tests green; pr-reviewer told (#${SEQ.reviewerPr491} there).`, span(2, 3.4)),
    SEQ.implTaskA,
  );
  const e2 = expectSeq(
    taskReady(TASK.b, p(2) + 1_000, refB, `PR #${TASK.b.pr} opened; the CI failure steered in mid-run was the config default — fixed in 3f2a9c1 and pushed.`, span(2, 11.5)),
    SEQ.implTaskB,
  );
  const e3 = expectSeq(
    taskReady(TASK.c, p(2) + 2_000, refC, `PR #${TASK.c.pr} opened: metrics and customer docs; test-writer's histogram tests still pending (promise_1).`, span(2, 20)),
    SEQ.implTaskC,
  );
  const e4 = expectSeq(
    receipt(log, {
      from: BOT.reviewer,
      askedSeq: SEQ.reviewerPr491,
      status: "handled",
      summary: `Approved #${TASK.a.pr}: refill arithmetic and sweep are right, tests cover the #482 regression`,
      at: p(4.5),
      hops: 4,
      session: refA,
      outcome: "handled",
      detail: "#491 approved; waiting on a human merge, nothing to change.",
    }),
    SEQ.implReviewA,
  );
  const e5 = expectSeq(
    log.add({
      kind: "ci.failure",
      source: `bot:${BOT.ci}`,
      at: p(10.8),
      summary: `CI run ${CI_RUN_492} on ${TASK.b.branch} (#${TASK.b.pr}) failed: test (node 22) — middleware.test.ts, RateLimit-Limit is NaN`,
      body: [`run: ${CI_RUN_492}`, "job: test (node 22)", "failing: src/ratelimit/middleware.test.ts › passes requests under the limit (+2 same cause)"],
      eventId: `bot:${BOT.ci}:${hex(`ci.failure:${CI_RUN_492}:implementer`, 12)}`,
      session: refB,
      sender: BOT.ci,
      hops: 1,
      outcome: "steered",
      detail: `Steered into the run for #${SEQ.implTaskB}: run ${CI_RUN_492} failed test (node 22) on ${TASK.b.branch}.`,
      resolvedAfterMs: span(10.8, 11.5),
      data: {
        run: CI_RUN_492,
        pr: TASK.b.pr,
        branch: TASK.b.branch,
        task: { id: TASK.b.id },
        job: "test (node 22)",
        failing: "src/ratelimit/middleware.test.ts › passes requests under the limit",
        log: CI_LOG_9187,
      },
    }),
    SEQ.implCiFailure,
  );
  const e6 = expectSeq(
    receipt(log, {
      from: BOT.reviewer,
      askedSeq: SEQ.reviewerPr492,
      status: "handled",
      summary: `Requested changes on #${TASK.b.pr}: default and validate RATE_LIMIT_PER_KEY (middleware.ts:13); same cause as CI run ${CI_RUN_492}`,
      at: p(12),
      hops: 4,
      session: refB,
      outcome: "handled",
      detail: "Requested change is the fix already pushed in 3f2a9c1; the synchronize will re-review.",
    }),
    SEQ.implReviewB,
  );
  const e7 = expectSeq(
    receipt(log, {
      from: BOT.reviewer,
      askedSeq: SEQ.reviewerPr493,
      status: "run_failed",
      summary: "llm provider error: anthropic returned 529 overloaded_error (4 attempts over 96 s)",
      at: p(20.6),
      hops: 4,
      session: refC,
      outcome: "handled",
      detail: "pr-reviewer's run failed on a provider 529; #493 stays open for its morning triage.",
    }),
    SEQ.implReviewC,
  );

  // Task a: explore, write, spawn tests, join, test, open the PR.
  const runA = appendScriptedRun(store, taskA, {
    at: p(2.2),
    user: e1.prompt,
    steps: [
      {
        thinking:
          "**Task a: the bucket**\n\nRead the plan, branch, and have repo-explorer tell me what #472 left behind before I write anything — the plan says replace it, but I want to know whether anything imports it.",
        tools: [
          vfsReadFile(PLAN_PATH, PLAN_LIN_1421),
          runCommand(["git", "checkout", "-b", TASK.a.branch], `Switched to a new branch '${TASK.a.branch}'`),
          agentRun(PROFILE.explorer, EXPLORE_BUCKET_TASK, EXPLORE_BUCKET_REPORT),
        ],
      },
      {
        thinking:
          "**Refill from whole intervals, and only then**\n\nNothing imports the old limiter, so this is a clean file. The invariant that matters is that refill depends on elapsed intervals, not on calls; keep `creditedAt` on the interval boundary so a hot caller sees the same clock as a quiet one. Tests go to test-writer in parallel while I write the docstrings.",
        tools: [writeFile("src/ratelimit/bucket.ts", RATELIMIT_BUCKET, "TokenBuckets: whole-interval refill, sweep"), agentSpawn(PROFILE.tests, TESTS_BUCKET_TASK, "promise_1")],
      },
      {
        tools: [
          awaitPromises(["promise_1"], [{ agent: PROFILE.tests, sessionId: SESSION.subTestsBucket, output: TESTS_BUCKET_REPORT }]),
          runCommand(["npx", "vitest", "run", "src/ratelimit"], VITEST_BUCKET_OUTPUT),
        ],
      },
      {
        tools: [
          commit(TASK.a.branch, TASK.a.head, TASK.a.prTitle, "2 files changed, 118 insertions(+)"),
          github(
            "create_pull_request",
            {
              ...REPO,
              title: TASK.a.prTitle,
              head: TASK.a.branch,
              base: "main",
              body: `Task a of ${PLAN_PATH}. TokenBuckets keyed by caller; refill credited from whole elapsed intervals so a hot caller cannot refill itself (the #482 regression), injectable clock, sweep() for idle keys. 5 tests.`,
            },
            `#${TASK.a.pr}`,
            `#${TASK.a.pr} created: ${TASK.a.prTitle}\n${TASK.a.branch} → main · +118 −0 · 2 files · checks: queued`,
          ),
          botEmit(
            {
              to: BOT.reviewer,
              kind: "pr.opened",
              summary: `#${TASK.a.pr} ${TASK.a.prTitle} — task ${TASK.a.id}, branch ${TASK.a.branch}`,
              data: { pr: TASK.a.pr, issue: ISSUE, task: { id: TASK.a.id }, branch: TASK.a.branch, head: TASK.a.head },
              reply: true,
            },
            SEQ.reviewerPr491,
          ),
        ],
        text: [
          `**#${TASK.a.pr} is open** — \`${TASK.a.prTitle}\`.`,
          "",
          "- `src/ratelimit/bucket.ts`: `TokenBuckets.take(key, cost)` returns the spec's `TakeResult`. Refill is credited from *whole* elapsed intervals and `creditedAt` stays on the interval boundary, so calling often never refills faster than the clock — the #482 bug is now a test (`does not let a hot caller refill itself`, 1,000 calls at 10 ms).",
          "- `src/ratelimit/bucket.test.ts` (test-writer): 5 tests on the injected clock, the same pattern the scheduler tests use.",
          "- `npx vitest run src/ratelimit`: 5 passed.",
          "",
          `Nothing imports the #472 limiter, so deleting it is the one-line follow-up the plan mentions. pr-reviewer has the PR (their #${SEQ.reviewerPr491}).`,
        ].join("\n"),
      },
    ],
  });
  e1.envelope.runId = runA.id;
  const exploreBucket = subagentSession(store, universe, {
    id: SESSION.subExploreBucket,
    displayName: "What rate-limiting code exists today?",
    profile: EXPLORER_PROFILE,
    parent: taskA,
    parentRunId: runA.id,
    root: SESSION.taskA,
    depth: 1,
    environmentId: ENV.taskA,
    createdAtMs: p(2.2) + 20_000,
  });
  appendExchange(store, universe, exploreBucket, {
    at: p(2.2) + 22_000,
    user: EXPLORE_BUCKET_TASK,
    turn: {
      tools: [
        runCommand(
          ["rg", "-n", "-i", "rate.?limit|bucket|X-Api-Key", "src"],
          'src/api/rate-limit.ts:1:// Behind RATE_LIMIT_ENABLED; not mounted anywhere yet (#472).\nsrc/api/rate-limit.ts:2:export class RateLimiter {\nsrc/api/rate-limit.test.ts:2:import { RateLimiter } from "./rate-limit.js";',
        ),
        readFile("src/api/rate-limit.ts", RATE_LIMIT_472),
        runCommand(["rg", "-l", "RateLimiter", "src"], "src/api/rate-limit.ts\nsrc/api/rate-limit.test.ts"),
      ],
      text: EXPLORE_BUCKET_REPORT,
    },
  });
  closeAt(exploreBucket, p(2.5));
  const testsBucket = subagentSession(store, universe, {
    id: SESSION.subTestsBucket,
    displayName: "Tests for src/ratelimit/bucket.ts",
    profile: TEST_WRITER_PROFILE,
    parent: taskA,
    parentRunId: runA.id,
    root: SESSION.taskA,
    depth: 1,
    environmentId: ENV.taskA,
    createdAtMs: p(2.2) + 50_000,
  });
  const testsBucketRun = appendScriptedRun(store, testsBucket, {
    at: p(2.2) + 52_000,
    user: TESTS_BUCKET_TASK,
    steps: [
      {
        thinking:
          "**Read the class, then match the house style**\n\nThe class takes `now`; the question is whether this repo fakes time with vitest timers or an injected clock. Ask repo-explorer rather than guess.",
        tools: [readFile("src/ratelimit/bucket.ts", RATELIMIT_BUCKET), agentRun(PROFILE.explorer, CONVENTIONS_TASK, CONVENTIONS_REPORT)],
      },
      {
        tools: [writeFile("src/ratelimit/bucket.test.ts", BUCKET_TEST, "5 cases on the injected clock"), runCommand(["npx", "vitest", "run", "src/ratelimit/bucket.test.ts"], VITEST_BUCKET_OUTPUT)],
        text: TESTS_BUCKET_REPORT,
      },
    ],
  });
  closeAt(testsBucket, p(3.1));
  const conventions = subagentSession(store, universe, {
    id: SESSION.subConventions,
    displayName: "How do existing tests fake time?",
    profile: EXPLORER_PROFILE,
    parent: testsBucket,
    parentRunId: testsBucketRun.id,
    root: SESSION.taskA,
    depth: 2,
    environmentId: ENV.taskA,
    createdAtMs: p(2.2) + 70_000,
  });
  appendExchange(store, universe, conventions, {
    at: p(2.2) + 72_000,
    user: CONVENTIONS_TASK,
    turn: {
      tools: [
        runCommand(
          ["rg", "-n", "useFakeTimers|now: \\(\\)|clock", "src", "--glob", "*.test.ts"],
          "src/scheduler/retry.test.ts:16:    vi.useFakeTimers();\nsrc/scheduler/retry.test.ts:28:    vi.useFakeTimers();\nsrc/scheduler/scheduler.test.ts:22:  const clock = { now: () => t };\nsrc/scheduler/scheduler.test.ts:23:  const scheduler = new Scheduler(pool, { maxAttempts: 3, baseDelayMs: 10, clock, random: () => 0 });",
        ),
        readFile("src/scheduler/retry.test.ts", RETRY_TEST_AFTER),
      ],
      text: CONVENTIONS_REPORT,
    },
  });
  closeAt(conventions, p(2.8));

  // Task b: middleware and config, with the CI failure steered in.
  const runB = appendScriptedRun(store, taskB, {
    at: p(2.3),
    user: e2.prompt,
    steps: [
      {
        thinking:
          "**Task b: mount the limiter**\n\nThe plan names the mount point; what I do not know is how this codebase reads configuration and whether the base path takes several handlers. repo-explorer first.",
        tools: [
          vfsReadFile(PLAN_PATH, PLAN_LIN_1421),
          runCommand(["git", "checkout", "-b", TASK.b.branch], `Switched to a new branch '${TASK.b.branch}'`),
          agentRun(PROFILE.explorer, EXPLORE_CONFIG_TASK, EXPLORE_CONFIG_REPORT),
        ],
      },
      {
        thinking:
          "**Same shape as the rest of the code**\n\nRead `process.env` once at module load like the scheduler does; bucket by `X-Api-Key`, fall back to `org:<orgId>` from the session the previous middleware set. Tests set `RATE_LIMIT_PER_KEY` before the dynamic import, per the repo's own pattern.",
        tools: [
          writeFile("src/ratelimit/middleware.ts", RATELIMIT_MIDDLEWARE_DRAFT, "limiter, env config, RateLimit-* headers"),
          writeFile("src/index.ts", ACME_WEB_INDEX_LIMITED, "mount after requireSession"),
          writeFile("src/ratelimit/middleware.test.ts", MIDDLEWARE_TEST_DRAFT, "3 cases; RATE_LIMIT_PER_KEY=5"),
        ],
      },
      {
        tools: [
          runCommand(["npx", "vitest", "run", "src/ratelimit/middleware.test.ts"], " ✓ src/ratelimit/middleware.test.ts (3 tests) 48ms\n Test Files  1 passed (1)\n      Tests  3 passed (3)"),
          commit(TASK.b.branch, "b92e6d0", TASK.b.prTitle, "3 files changed, 94 insertions(+), 1 deletion(-)"),
          github(
            "create_pull_request",
            {
              ...REPO,
              title: TASK.b.prTitle,
              head: TASK.b.branch,
              base: "main",
              body: `Task b of ${PLAN_PATH}. Per-key limiter mounted on /api after requireSession; X-Api-Key or org bucket; 429 with Retry-After and RateLimit-* headers; RATE_LIMIT_ENABLED / RATE_LIMIT_PER_KEY / RATE_LIMIT_BURST.`,
            },
            `#${TASK.b.pr}`,
            `#${TASK.b.pr} created: ${TASK.b.prTitle}\n${TASK.b.branch} → main · +94 −1 · 3 files · checks: queued`,
          ),
          botEmit(
            {
              to: BOT.reviewer,
              kind: "pr.opened",
              summary: `#${TASK.b.pr} ${TASK.b.prTitle} — task ${TASK.b.id}, branch ${TASK.b.branch}`,
              data: { pr: TASK.b.pr, issue: ISSUE, task: { id: TASK.b.id }, branch: TASK.b.branch, head: "b92e6d0" },
              reply: true,
            },
            SEQ.reviewerPr492,
          ),
        ],
      },
      {
        thinking:
          "**Docs while CI runs**\n\nThe PR is open and pr-reviewer has it. The plan's last item for this task is the operator doc for the two variables; write it now rather than after the review.",
        tools: [
          writeFile("docs/config.md", CONFIG_DOC, "RATE_LIMIT_* variables"),
          runCommand(
            ["sh", "-c", `git add docs/config.md && git commit -m "docs: RATE_LIMIT_* variables" && git push`],
            `[${TASK.b.branch} 1c40f7a] docs: RATE_LIMIT_* variables\n 1 file changed, 12 insertions(+)`,
          ),
        ],
      },
      {
        thinking:
          "**CI failure steered in — it is mine**\n\nThe CI job has no `RATE_LIMIT_PER_KEY`, so `Number(undefined)` is `NaN`, the bucket capacity is `NaN`, and every request is refused; my tests set the variable before importing, which is exactly why they passed. Default it to 600 like the spec says, refuse non-positive values loudly at start, and add the test CI would have needed.",
        tools: [
          github("get_job_logs", { ...REPO, run_id: CI_RUN_492, job: "test (node 22)", tail_lines: 40 }, `run ${CI_RUN_492} · test (node 22)`, CI_LOG_9187),
          writeFile("src/ratelimit/middleware.ts", RATELIMIT_MIDDLEWARE, "default 600/min, validate at start"),
          writeFile("src/ratelimit/middleware.test.ts", MIDDLEWARE_TEST, "+ default and validation case"),
          runCommand(
            ["npx", "vitest", "run", "src/ratelimit"],
            " ✓ src/ratelimit/bucket.test.ts (5 tests) 20ms\n ✓ src/ratelimit/middleware.test.ts (4 tests) 51ms\n Test Files  2 passed (2)\n      Tests  9 passed (9)",
          ),
          runCommand(
            ["sh", "-c", `git add -A && git commit -m "ratelimit: default RATE_LIMIT_PER_KEY to 600 and validate it" && git push`],
            `[${TASK.b.branch} ${TASK.b.head}] ratelimit: default RATE_LIMIT_PER_KEY to 600 and validate it\n 2 files changed, 16 insertions(+), 2 deletions(-)`,
          ),
        ],
        text: [
          `**#${TASK.b.pr} is open** — \`${TASK.b.prTitle}\` — and the CI failure ci-watch steered in was mine.`,
          "",
          "- `src/ratelimit/middleware.ts`: buckets by `X-Api-Key`, falling back to `org:<orgId>` for browser sessions; 429 carries `Retry-After` in whole seconds; `RateLimit-Limit` / `RateLimit-Remaining` on every response.",
          "- `src/index.ts`: `.use(requireSession, rateLimit)` — unauthenticated requests are refused before they cost a token.",
          "- `docs/config.md`: the three variables.",
          "",
          `**The CI failure (run ${CI_RUN_492}):** \`RATE_LIMIT_PER_KEY\` is unset in the CI job, \`Number(undefined)\` made the capacity \`NaN\`, and every request was a 429. My tests set the variable before importing, so they passed locally — the wrong kind of green. Fixed in \`${TASK.b.head}\`: default 600/min, a positive-integer check that fails at start, and a test for both. 9 tests pass; CI is re-running and pr-reviewer has the PR (their #${SEQ.reviewerPr492}).`,
        ].join("\n"),
      },
    ],
    steer: { afterStep: 4, text: e5.prompt },
  });
  e2.envelope.runId = runB.id;
  const exploreConfig = subagentSession(store, universe, {
    id: SESSION.subExploreConfig,
    displayName: "How is configuration read and /api middleware mounted?",
    profile: EXPLORER_PROFILE,
    parent: taskB,
    parentRunId: runB.id,
    root: SESSION.taskB,
    depth: 1,
    environmentId: ENV.taskB,
    createdAtMs: p(2.3) + 20_000,
  });
  appendExchange(store, universe, exploreConfig, {
    at: p(2.3) + 22_000,
    user: EXPLORE_CONFIG_TASK,
    turn: {
      tools: [
        runCommand(
          ["rg", "-n", "process\\.env", "src"],
          'src/index.ts:26:export default { port: Number(process.env.PORT ?? 3000), fetch: app.fetch };\nsrc/auth/middleware.ts:18:const secret = new TextEncoder().encode(process.env.SESSION_SECRET ?? "");\nsrc/scheduler/scheduler.ts:14:const POLL_MS = Number(process.env.SCHEDULER_POLL_MS ?? 1000);\nsrc/scheduler/scheduler.test.ts:6:process.env.SCHEDULER_POLL_MS = "10";',
        ),
        readFile("src/index.ts", ACME_WEB_INDEX),
      ],
      text: EXPLORE_CONFIG_REPORT,
    },
  });
  closeAt(exploreConfig, p(2.6));
  e4.envelope.runId = appendExchange(store, universe, taskA, {
    at: p(4.55),
    user: e4.prompt,
    turn: { text: `#${TASK.a.pr} is approved. Nothing to change; a person merges it. This thread is done unless the PR changes.` },
  }).id;
  e6.envelope.runId = appendExchange(store, universe, taskB, {
    at: p(12.05),
    user: e6.prompt,
    turn: {
      text: `pr-reviewer's requested change — default and validate \`RATE_LIMIT_PER_KEY\` at \`middleware.ts:13\` — is the fix already on the branch as \`${TASK.b.head}\`, pushed for the CI failure two minutes before the review posted. The synchronize triggers the re-review; nothing more to push.`,
    },
  }).id;

  // Task c: metrics and docs; the spawned test-writer is still open.
  const runC = appendScriptedRun(store, taskC, {
    at: p(2.4),
    user: e3.prompt,
    steps: [
      {
        thinking: "**Task c: metrics and the customer doc**\n\nFind the existing registry before adding metrics to it; the plan says there is one in src/metrics.ts.",
        tools: [
          vfsReadFile(PLAN_PATH, PLAN_LIN_1421),
          runCommand(["git", "checkout", "-b", TASK.c.branch], `Switched to a new branch '${TASK.c.branch}'`),
          runCommand(
            ["rg", "-n", "prom-client|registry", "src"],
            'src/metrics.ts:1:import { Registry, collectDefaultMetrics } from "prom-client";\nsrc/metrics.ts:4:export const registry = new Registry();\nsrc/metrics.ts:5:collectDefaultMetrics({ register: registry, prefix: "acme_" });',
          ),
          readFile("src/metrics.ts", METRICS_TS),
        ],
      },
      {
        thinking:
          "**Bounded labels**\n\nA counter labelled by raw API key would put secrets in Prometheus and grow without bound. Hash the key to eight hex digits: enough to tell callers apart on a dashboard, useless to anyone who scrapes it. Tests to test-writer; I write the customer doc meanwhile.",
        tools: [
          writeFile("src/ratelimit/metrics.ts", RATELIMIT_METRICS, "decisions counter + Retry-After histogram"),
          writeFile("docs/api/limits.md", LIMITS_DOC, "customer-facing: limits, headers, 429"),
          agentSpawn(PROFILE.tests, TESTS_METRICS_TASK, "promise_1"),
        ],
      },
      {
        tools: [
          runCommand(["npx", "tsc", "-p", "tsconfig.json", "--noEmit"], ""),
          commit(TASK.c.branch, TASK.c.head, TASK.c.prTitle, "2 files changed, 61 insertions(+)"),
          github(
            "create_pull_request",
            {
              ...REPO,
              title: TASK.c.prTitle,
              head: TASK.c.branch,
              base: "main",
              body: `Task c of ${PLAN_PATH}. observe(key, result) on the shared registry: decisions by hashed key and outcome, Retry-After histogram; docs/api/limits.md for customers. Tests follow in this PR when test-writer finishes.`,
            },
            `#${TASK.c.pr}`,
            `#${TASK.c.pr} created: ${TASK.c.prTitle}\n${TASK.c.branch} → main · +61 −0 · 2 files · checks: queued`,
          ),
          botEmit(
            {
              to: BOT.reviewer,
              kind: "pr.opened",
              summary: `#${TASK.c.pr} ${TASK.c.prTitle} — task ${TASK.c.id}, branch ${TASK.c.branch}`,
              data: { pr: TASK.c.pr, issue: ISSUE, task: { id: TASK.c.id }, branch: TASK.c.branch, head: TASK.c.head },
              reply: true,
            },
            SEQ.reviewerPr493,
          ),
        ],
        text: [
          `**#${TASK.c.pr} is open** — \`${TASK.c.prTitle}\`.`,
          "",
          "- `src/ratelimit/metrics.ts`: `observe(key, result)` on the registry from `src/metrics.ts` — `acme_ratelimit_decisions_total{key, outcome}` with the key hashed to eight hex digits, and `acme_ratelimit_retry_after_seconds` observed on refusals only.",
          "- `docs/api/limits.md`: the customer page — 600/min per key, the burst, the headers, what to do on 429, and that `/webhooks/*` is never limited.",
          "- Typecheck clean.",
          "",
          `test-writer is still writing \`metrics.test.ts\` (\`promise_1\` pending); its result lands at my next turn and I will push the file to the same PR. pr-reviewer has the PR (their #${SEQ.reviewerPr493}). Wiring \`observe\` into the middleware is the two-line follow-up once #${TASK.b.pr} and this are on main.`,
        ].join("\n"),
      },
    ],
  });
  e3.envelope.runId = runC.id;
  const testsMetrics = subagentSession(store, universe, {
    id: SESSION.subTestsMetrics,
    displayName: "Tests for src/ratelimit/metrics.ts",
    profile: TEST_WRITER_PROFILE,
    parent: taskC,
    parentRunId: runC.id,
    root: SESSION.taskC,
    depth: 1,
    environmentId: ENV.taskC,
    createdAtMs: p(2.4) + 50_000,
  });
  appendScriptedRun(store, testsMetrics, {
    at: p(2.4) + 52_000,
    user: TESTS_METRICS_TASK,
    steps: [
      { tools: [readFile("src/ratelimit/metrics.ts", RATELIMIT_METRICS), readFile("src/metrics.ts", METRICS_TS)] },
      {
        tools: [
          writeFile("src/ratelimit/metrics.test.ts", METRICS_TEST, "hashed key, refusal-only histogram"),
          runCommand(["npx", "vitest", "run", "src/ratelimit/metrics.test.ts"], " ✓ src/ratelimit/metrics.test.ts (2 tests) 33ms\n Test Files  1 passed (1)\n      Tests  2 passed (2)"),
        ],
        text: 'Wrote src/ratelimit/metrics.test.ts: 2 tests — decisions counted by outcome under a hashed key (asserting the raw key never appears in the exposition), and the Retry-After histogram observed only for refusals (`_count 1` after one allow and one refusal, `le="5"` bucket hit). `registry.resetMetrics()` between tests. 2 passed in 33 ms. The file is in the sandbox, uncommitted.',
      },
    ],
  });
  e7.envelope.runId = appendExchange(store, universe, taskC, {
    at: p(20.65),
    user: e7.prompt,
    turn: {
      text: `pr-reviewer's run on #${TASK.c.pr} failed on a provider error (529, overloaded), not on the PR; its morning triage picks the review up. Nothing for me to change. Still waiting on test-writer's \`promise_1\` for the metrics tests.`,
    },
  }).id;

  const descendants: SessionSummaryView[] = [
    lineageChild(exploreBucket),
    lineageChild(testsBucket),
    lineageChild(conventions),
    lineageChild(exploreConfig),
    lineageChild(testsMetrics),
  ];
  universe.bots.set(BOT.implementer, {
    bot: record,
    triggers: new Map(triggers.map((entry) => [entry.triggerId, entry])),
    events: log.events,
    state: botState({
      bot: record,
      sessions: [botSession(main, "main"), botSession(taskA, "keyed", TASK.a.id), botSession(taskB, "keyed", TASK.b.id), botSession(taskC, "keyed", TASK.c.id)],
      recentEvents: [
        recent(e1.envelope, { inputTokens: 48_200, cachedInputTokens: 41_900 }),
        recent(e2.envelope, { inputTokens: 71_600, cachedInputTokens: 63_100 }),
        recent(e3.envelope, { inputTokens: 44_900, cachedInputTokens: 39_800 }),
        recent(e4.envelope, { inputTokens: 52_300, cachedInputTokens: 51_600 }),
        recent(e5.envelope),
        recent(e6.envelope, { inputTokens: 75_800, cachedInputTokens: 75_100 }),
        recent(e7.envelope, { inputTokens: 48_100, cachedInputTokens: 47_600 }),
      ],
      eventsProcessed: 58,
      appliedProfileRevision: IMPLEMENTER_PROFILE.revision,
      runsToday: 6,
      descendantsToday: 5,
    }),
    descendants,
  });
}

// --- pr-reviewer -----------------------------------------------------------

/// Runs the PR's branch through the affected tests in the shared CI runner.
const reviewTests = (task: Task, output: string): DemoToolCall =>
  runCommand(["sh", "-c", `git fetch origin ${task.branch} && git checkout -q FETCH_HEAD && npx vitest run src/ratelimit`], output, { detail: `${task.branch} in the CI runner` });
const review = (pr: number, event: "APPROVE" | "REQUEST_CHANGES", detail: string, id: number): DemoToolCall =>
  github("create_pull_request_review", { ...REPO, pull_number: pr, event }, detail, `review ${id} submitted: ${event}`);

function seedPrReviewer(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.reviewer,
    displayName: "PR Reviewer",
    description: "Reviews acme-web pull requests as they open and change; approves or requests changes; tells release-scribe and intake.",
    profileId: PROFILE.reviewer,
    brief: [
      "You are PR Reviewer for acme/acme-web.",
      "",
      "A pr.opened from implementer or a GitHub pull_request event means: read the PR, run the affected tests in the CI runner, and post one review — request changes only for correctness or security. A GitHub event for a PR you already reviewed with no new commits is nothing new: say so and stop. A ci.failure from ci-watch steered into a review is evidence: reproduce it and fold it into the verdict. On approval of a customer-visible change, tell release-scribe with bot_emit (release.note); when a feature's PRs are approved, tell intake (review.approved). The morning triage is a short digest of open PRs and who they wait on.",
    ].join("\n"),
    runsPerDay: 120,
    breaker: { fires: 20, windowMs: 10 * MINUTE_MS },
    routedSessionCloseAfterMs: 2 * DAY_MS,
    emit: true,
    createdAtMs: ago(12 * DAY_MS),
    updatedAtMs: ago(3 * DAY_MS),
  });
  const triggers = [
    webhookTrigger(
      universe,
      BOT.reviewer,
      "github-webhook",
      { token: GITHUB_WEBHOOK_TOKEN, verification: { scheme: "hmac-sha256", grantId: GRANT.github, header: "x-hub-signature-256", prefix: "sha256=" }, preset: "github" },
      {
        filter: 'data.action in ["opened", "synchronize"]',
        route: { policy: "perKey", key: "pull_request.number" },
        coalesce: { debounceMs: 45_000, maxWaitMs: 180_000, maxCount: 10 },
        deliver: { whenBusy: "queue" },
        createdAtMs: ago(12 * DAY_MS),
        updatedAtMs: ago(5 * DAY_MS),
      },
    ),
    inboxTrigger(BOT.reviewer, [BOT.implementer, BOT.ci], {
      route: { policy: "perKey", key: "data.pr" },
      deliver: { whenBusy: "steer" },
      createdAtMs: ago(8 * DAY_MS),
      updatedAtMs: ago(2 * DAY_MS),
    }),
    scheduleTrigger(
      BOT.reviewer,
      "morning-triage",
      { cron: "0 9 * * 1-5", summary: "Weekday morning triage: open PRs and who they wait on, anything older than two days, reviews deferred yesterday." },
      { route: { policy: "bot" }, deliver: { whenBusy: "queue" }, createdAtMs: ago(11 * DAY_MS) },
    ),
  ];
  const tools = [...BOT_TOOLS, EMIT_TOOL];
  const main = managedSession(store, universe, {
    id: SESSION.reviewerMain,
    botId: BOT.reviewer,
    displayName: "PR Reviewer",
    profile: REVIEWER_PROFILE,
    tools,
    createdAtMs: ago(12 * DAY_MS),
    environmentId: ENV.ci,
  });
  introduce(
    store,
    universe,
    main,
    ago(12 * DAY_MS) + 45_000,
    "I'm PR Reviewer for acme/acme-web: I review pull requests as they open and change, run the affected tests in the shared CI runner, and post one review with a clear verdict. I wake on the GitHub webhook (opened, synchronize), on implementer's and ci-watch's events, and weekday mornings for a triage; I have the GitHub App tools, the CI runner environment, and repo-explorer as a sub-agent. All present.",
  );
  const thread = (id: string, task: Task, createdAtMs: number): SessionRecord =>
    managedSession(store, universe, { id, botId: BOT.reviewer, displayName: `PR Reviewer · #${task.pr}`, profile: REVIEWER_PROFILE, tools, createdAtMs, environmentId: ENV.ci });
  const PR486: Task = { id: "ci-cache", title: "restore npm cache", branch: "marco/ci-cache", pr: 486, prTitle: "ci: restore npm cache in ci.yml", head: "f08b2e5" };
  const pr486 = thread(SESSION.pr486, PR486, at(3, 10, 5));
  const pr491 = thread(SESSION.pr491, TASK.a, p(3.4));
  const pr492 = thread(SESSION.pr492, TASK.b, p(5));
  const pr493 = thread(SESSION.pr493, TASK.c, p(20));
  const mainRef = { sessionId: SESSION.reviewerMain, label: "Main" };
  const ref = (session: SessionRecord, task: Task) => ({ sessionId: session.view.id, label: `pr-${task.pr}` });

  const log = eventLog(store, BOT.reviewer);
  const triage = (atMs: number, detail: string): ScriptedEvent =>
    log.add({
      kind: "schedule",
      source: "schedule:morning-triage",
      at: atMs,
      summary: "Weekday morning triage: open PRs and who they wait on, anything older than two days, reviews deferred yesterday.",
      body: ["cron: 0 9 * * 1-5 (Europe/Berlin)"],
      eventId: `schedule:morning-triage:${atIso(atMs).slice(0, 10)}`,
      session: mainRef,
      outcome: "handled",
      detail,
      resolvedAfterMs: 80_000,
    });
  const e1 = expectSeq(
    pullRequestEvent(
      log,
      "opened",
      PR486,
      at(3, 10, 5),
      ref(pr486, PR486),
      "handled",
      "Approved #486: restores the cache line #483 dropped; install back under a minute on the PR's own run. Told release-scribe.",
      240_000,
      { author: "marco-r" },
    ),
    SEQ.reviewerPr486,
  );
  const e2 = triage(at(1, 9, 0), "2 open PRs: #487 (flaky scheduler test) approved, waiting on Priya to merge; #486 merged yesterday. Nothing stale.");
  const e3 = triage(p(-13.75), "1 open PR: #487 waits on Priya to merge. Nothing older than two days, nothing deferred.");
  const e4 = expectSeq(
    prOpened(log, TASK.a, p(3.4), ref(pr491, TASK.a), "handled", "Approved #491: refill arithmetic and sweep are right, tests cover the #482 regression.", span(3.4, 4.5)),
    SEQ.reviewerPr491,
  );
  const e5 = pullRequestEvent(log, "opened", TASK.a, p(3.45), ref(pr491, TASK.a), "ignored", `Same PR as #${SEQ.reviewerPr491}, no new commits; reviewed already.`, span(3.45, 4.8));
  const e6 = expectSeq(
    prOpened(
      log,
      TASK.b,
      p(5),
      ref(pr492, TASK.b),
      "handled",
      `Requested changes on #492: default and validate RATE_LIMIT_PER_KEY (middleware.ts:13); same cause as CI run ${CI_RUN_492}.`,
      span(5, 12),
    ),
    SEQ.reviewerPr492,
  );
  const e7 = pullRequestEvent(log, "opened", TASK.b, p(5.05), ref(pr492, TASK.b), "ignored", `Reviewed on #${SEQ.reviewerPr492} (changes requested); the webhook adds nothing.`, span(5.05, 12.3));
  const e8 = expectSeq(
    log.add({
      kind: "ci.failure",
      source: `bot:${BOT.ci}`,
      at: p(10.8),
      summary: `CI run ${CI_RUN_492} on ${TASK.b.branch} (#${TASK.b.pr}) failed: test (node 22) — middleware.test.ts, RateLimit-Limit is NaN`,
      body: [`run: ${CI_RUN_492}`, "job: test (node 22)", "failing: src/ratelimit/middleware.test.ts › passes requests under the limit (+2 same cause)"],
      eventId: `bot:${BOT.ci}:${hex(`ci.failure:${CI_RUN_492}:pr-reviewer`, 12)}`,
      session: ref(pr492, TASK.b),
      sender: BOT.ci,
      hops: 1,
      outcome: "steered",
      detail: `Steered into the review for #${SEQ.reviewerPr492}: reproduced without RATE_LIMIT_PER_KEY, folded into the verdict.`,
      resolvedAfterMs: span(10.8, 12),
      data: {
        run: CI_RUN_492,
        pr: TASK.b.pr,
        branch: TASK.b.branch,
        task: { id: TASK.b.id },
        job: "test (node 22)",
        failing: "src/ratelimit/middleware.test.ts › passes requests under the limit",
        log: CI_LOG_9187,
      },
    }),
    SEQ.reviewerCiFailure,
  );
  const e9 = expectSeq(
    pullRequestEvent(
      log,
      "synchronize",
      TASK.b,
      p(11.4),
      ref(pr492, TASK.b),
      "handled",
      "Approved #492 after 3f2a9c1: default, validation, and the test. Told release-scribe and intake.",
      span(11.4, 13.5),
    ),
    SEQ.reviewerSync492,
  );
  const e10 = expectSeq(
    prOpened(log, TASK.c, p(20), ref(pr493, TASK.c), "run_failed", "llm provider error: anthropic returned 529 overloaded_error (4 attempts over 96 s)", span(20, 20.6)),
    SEQ.reviewerPr493,
  );
  const e11 = pullRequestEvent(
    log,
    "opened",
    TASK.c,
    p(20.05),
    ref(pr493, TASK.c),
    "deferred",
    `Provider is shedding load (#${SEQ.reviewerPr493} failed on a 529); #493 deferred to the morning triage.`,
    span(20.05, 21),
  );

  e1.envelope.runId = appendScriptedRun(store, pr486, {
    at: at(3, 10, 6),
    user: e1.prompt,
    steps: [
      {
        thinking: "**A one-line CI change**\n\nThe diff should be the cache line; the proof is the PR's own run timing.",
        tools: [
          github("get_pull_request_files", { ...REPO, pull_number: 486 }, "#486 files", ".github/workflows/ci.yml (+1 −0: cache: npm under setup-node)"),
          github("list_workflow_runs", { ...REPO, workflow_id: "ci.yml", branch: PR486.branch, per_page: 1 }, "run on the PR branch", "9160  success  6m 31s  (install 44s)"),
        ],
      },
      {
        tools: [
          review(486, "APPROVE", "approve", 2241108),
          botEmit(
            {
              to: BOT.scribe,
              kind: "release.note",
              summary: "#486 restores the npm cache in CI; internal, not customer-facing.",
              data: { pr: 486, area: "internal", breaking: false, note: "CI restores the npm cache dropped in #483; install is back under a minute." },
            },
            SEQ.scribeNote486,
          ),
        ],
        text: "Approved #486: one line restoring `cache: npm` under `setup-node`, and the PR's own run has `install` back at 44 s. Sent release-scribe the note as internal; it decides whether it belongs in customer notes.",
      },
    ],
  }).id;
  closeAt(pr486, at(1, 9, 30));
  e2.envelope.runId = appendExchange(store, universe, main, {
    at: at(1, 9, 0),
    user: e2.prompt,
    turn: {
      tools: [
        github(
          "list_pull_requests",
          { ...REPO, state: "open", sort: "created" },
          "open PRs",
          "#487 scheduler: make retry test independent of wall time — marco-r — approved (pr-reviewer), waiting on merge",
        ),
      ],
      text: "**Triage — 1 open PR**\n\n- #487 scheduler retry test — approved, waiting on Priya to merge. #486 merged yesterday.\n\nNothing older than two days.",
    },
  }).id;
  e3.envelope.runId = appendExchange(store, universe, main, {
    at: p(-13.7),
    user: e3.prompt,
    turn: {
      tools: [
        github(
          "list_pull_requests",
          { ...REPO, state: "open", sort: "created" },
          "open PRs",
          "#487 scheduler: make retry test independent of wall time — marco-r — approved (pr-reviewer), waiting on merge",
        ),
      ],
      text: "**Triage — 1 open PR**\n\n- #487 scheduler retry test — approved two days ago, still waiting on Priya to merge; pinged her on the PR.\n\nNothing deferred from yesterday.",
    },
  }).id;
  e4.envelope.runId = appendScriptedRun(store, pr491, {
    at: p(3.5),
    user: e4.prompt,
    steps: [
      {
        thinking:
          "**Reviewing the bucket**\n\nThe thing that got #482 reverted was refill-per-request. Read the arithmetic around `creditedAt`, then run the PR's tests in the CI runner — including the thousand-call hot-caller case.",
        tools: [
          github("get_pull_request_diff", { ...REPO, pull_number: TASK.a.pr }, `#${TASK.a.pr} diff`, "(2 files, +118 −0; src/ratelimit/bucket.ts, src/ratelimit/bucket.test.ts)"),
          reviewTests(TASK.a, VITEST_BUCKET_OUTPUT),
        ],
      },
      {
        tools: [review(TASK.a.pr, "APPROVE", "approve with 1 comment", 2246710)],
        text: `Approved #${TASK.a.pr}. Refill is credited from whole elapsed intervals and \`creditedAt\` advances by whole intervals too, so a hot caller cannot move the clock — the test fires 1,000 calls at 10 ms and stays refused, which is exactly the #482 regression. \`retryAfterMs\` rounds up to the next interval boundary; \`sweep()\` only drops full buckets. One comment, non-blocking: \`take(key, cost)\` with \`cost > capacity\` returns a finite wait for something that can never succeed; worth a guard if a cost above 1 is ever used.`,
      },
    ],
  }).id;
  e5.envelope.runId = appendExchange(store, universe, pr491, {
    at: p(4.6),
    user: e5.prompt,
    turn: { text: `GitHub's own opened event for #${TASK.a.pr}: same head (\`${TASK.a.head}\`) I reviewed on #${SEQ.reviewerPr491}. Nothing new.` },
  }).id;
  e6.envelope.runId = appendScriptedRun(store, pr492, {
    at: p(5.1),
    user: e6.prompt,
    steps: [
      {
        thinking:
          "**Reviewing the mount**\n\nThe order of handlers on the base path and the fallback key for browser sessions are the two places this can go wrong. Read the diff, run its tests in the CI runner.",
        tools: [
          github(
            "get_pull_request_diff",
            { ...REPO, pull_number: TASK.b.pr },
            `#${TASK.b.pr} diff`,
            "(4 files, +106 −1; src/ratelimit/middleware.ts, src/ratelimit/middleware.test.ts, src/index.ts, docs/config.md)",
          ),
          reviewTests(TASK.b, " ✓ src/ratelimit/bucket.test.ts (5 tests) 20ms\n ✓ src/ratelimit/middleware.test.ts (3 tests) 48ms\n Test Files  2 passed (2)\n      Tests  8 passed (8)"),
        ],
      },
      {
        thinking:
          "**The CI failure is a configuration hole**\n\nci-watch says the same suite fails in CI with `RateLimit-Limit: NaN`. The tests set `RATE_LIMIT_PER_KEY` before importing; CI does not. `readConfig` at `middleware.ts:13` does `Number(env.RATE_LIMIT_PER_KEY)` with no default, so an unset variable refuses every request in production too. Reproduce it, then block on it.",
        tools: [runCommand(["sh", "-c", "env -u RATE_LIMIT_PER_KEY npx vitest run src/ratelimit/middleware.test.ts"], CI_LOG_9187, { detail: "without RATE_LIMIT_PER_KEY", isError: true })],
      },
      {
        tools: [review(TASK.b.pr, "REQUEST_CHANGES", "request changes: default and validate RATE_LIMIT_PER_KEY", 2247055)],
        text: [
          `Requested changes on #${TASK.b.pr} with one blocking finding.`,
          "",
          `**Blocking** — \`src/ratelimit/middleware.ts:13\`: \`Number(env.RATE_LIMIT_PER_KEY)\` has no default, so an unset variable makes the capacity \`NaN\` and every \`/api\` request a 429. That is what CI run ${CI_RUN_492} shows (\`RateLimit-Limit: NaN\`), and production would do the same on a deploy without the variable. Default to 600 per the spec and refuse a non-positive or non-integer value at start, with a test that imports without the variable set.`,
          "",
          "The mount order (`requireSession, rateLimit`), the `org:<orgId>` fallback, and the headers are right; the doc is fine. Approve on the fix-up.",
        ].join("\n"),
      },
    ],
    steer: { afterStep: 1, text: e8.prompt },
  }).id;
  e7.envelope.runId = appendExchange(store, universe, pr492, {
    at: p(12.1),
    user: e7.prompt,
    turn: { text: `GitHub's opened event for #${TASK.b.pr} waited behind the review; the review (#${SEQ.reviewerPr492}, changes requested) covers this head. Nothing new.` },
  }).id;
  e9.envelope.runId = appendScriptedRun(store, pr492, {
    at: p(12.4),
    user: e9.prompt,
    steps: [
      {
        thinking: "**Re-reviewing the fix-up**\n\nOne commit since the review. Check the default, the guard, and that the new test imports without the variable; run the suite the way CI does.",
        tools: [
          github(
            "get_pull_request_files",
            { ...REPO, pull_number: TASK.b.pr },
            `#${TASK.b.pr} files since review`,
            "src/ratelimit/middleware.ts (+9 −2: DEFAULT_PER_MINUTE, positive-integer guard)\nsrc/ratelimit/middleware.test.ts (+7: defaults to 600 when unset, rejects 0)",
          ),
          runCommand(
            ["sh", "-c", `git fetch origin ${TASK.b.branch} && git checkout -q FETCH_HEAD && env -u RATE_LIMIT_PER_KEY npx vitest run src/ratelimit`],
            " ✓ src/ratelimit/bucket.test.ts (5 tests) 20ms\n ✓ src/ratelimit/middleware.test.ts (4 tests) 52ms\n Test Files  2 passed (2)\n      Tests  9 passed (9)",
            { detail: "without RATE_LIMIT_PER_KEY, like CI" },
          ),
        ],
      },
      {
        tools: [
          review(TASK.b.pr, "APPROVE", "approve", 2247390),
          botEmit(
            {
              to: BOT.scribe,
              kind: "release.note",
              summary: `${ISSUE}: per-API-key rate limits on /api — 600/min default, 429 with Retry-After; customer-visible.`,
              data: {
                pr: [491, 492],
                issue: ISSUE,
                area: "api",
                breaking: false,
                note: "Per-API-key rate limits on /api: 600 requests per minute per key by default, browser sessions bucketed per org; 429 with Retry-After and RateLimit-* headers.",
              },
            },
            SEQ.scribeNote1421,
          ),
          botEmit(
            {
              to: BOT.intake,
              kind: "review.approved",
              summary: `${ISSUE}: #491 (task a) and #492 (task b) approved; task c not yet opened`,
              data: { issue: ISSUE, approved: [491, 492], remaining: ["lin-1421-c"] },
            },
            SEQ.intakeApproved,
          ),
        ],
        text: `Approved #${TASK.b.pr}: \`${TASK.b.head}\` defaults \`RATE_LIMIT_PER_KEY\` to 600, throws on a non-positive value at start, and the new test imports without the variable — 9 tests pass with it unset, the way CI runs. This is the customer-visible half of ${ISSUE}, so release-scribe has the note (their #${SEQ.scribeNote1421}), and intake has the status for the issue (their #${SEQ.intakeApproved}).`,
      },
    ],
  }).id;
  e10.envelope.runId = appendScriptedRun(store, pr493, {
    at: p(20.1),
    user: e10.prompt,
    steps: [
      {
        thinking: "**Reviewing metrics and docs**\n\nLabel cardinality is the risk in a metrics change: a raw key as a label is both a secret leak and unbounded growth. Read the diff first.",
        tools: [github("get_pull_request_diff", { ...REPO, pull_number: TASK.c.pr }, `#${TASK.c.pr} diff`, "(2 files, +61 −0; src/ratelimit/metrics.ts, docs/api/limits.md)")],
      },
    ],
    failure: "llm provider error: anthropic returned 529 overloaded_error (4 attempts over 96 s)",
  }).id;
  e11.envelope.runId = appendExchange(store, universe, pr493, {
    at: p(20.7),
    user: e11.prompt,
    turn: {
      text: `Deferring #${TASK.c.pr}: the provider is shedding load — my review on #${SEQ.reviewerPr493} failed on a 529 two minutes ago — and burning the run budget on retries helps nobody. The diff is already in this thread; the morning triage picks it up, or replay #${SEQ.reviewerPr493} once the provider recovers.`,
    },
  }).id;

  universe.bots.set(BOT.reviewer, {
    bot: record,
    triggers: new Map(triggers.map((entry) => [entry.triggerId, entry])),
    events: log.events,
    state: botState({
      bot: record,
      sessions: [botSession(main, "main"), botSession(pr491, "keyed", "pr-491"), botSession(pr492, "keyed", "pr-492"), botSession(pr493, "keyed", "pr-493")],
      recentEvents: [
        recent(e4.envelope, { inputTokens: 38_400, cachedInputTokens: 33_900 }),
        recent(e5.envelope, { inputTokens: 41_100, cachedInputTokens: 40_600 }),
        recent(e6.envelope, { inputTokens: 57_300, cachedInputTokens: 49_800 }),
        recent(e7.envelope, { inputTokens: 60_200, cachedInputTokens: 59_700 }),
        recent(e8.envelope),
        recent(e9.envelope, { inputTokens: 63_900, cachedInputTokens: 61_200 }),
        recent(e10.envelope, { inputTokens: 34_800, cachedInputTokens: 30_100 }),
        recent(e11.envelope, { inputTokens: 36_200, cachedInputTokens: 35_700 }),
      ],
      eventsProcessed: 163,
      duplicateEventCount: 3,
      appliedProfileRevision: REVIEWER_PROFILE.revision,
      runsToday: 8,
    }),
    descendants: [],
  });
}

// --- ci-watch --------------------------------------------------------------

function seedCiWatch(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.ci,
    displayName: "CI Watch",
    description: "Polls GitHub Actions for failed runs; steers branch failures into implementer's task and tells pr-reviewer; files issues for main.",
    profileId: PROFILE.ci,
    brief: [
      "You are CI Watch for acme/acme-web. Each poll event is one failed workflow run.",
      "",
      "Read the failing job's log and name the failing test or step. A failure on a task branch (lin-<issue>-<task>-…) goes to implementer as ci.failure with the task id from the branch name, and to pr-reviewer with the PR number — one event per run, never a repeat. A failure on main gets an issue with the log excerpt, assigned to the author of the breaking commit; two failures of the same job within minutes with an infrastructure cause are one issue. A run cancelled by a newer push is not a failure: ignore it and say so.",
    ].join("\n"),
    runsPerDay: 80,
    breaker: { fires: 12, windowMs: 10 * MINUTE_MS },
    emit: true,
    createdAtMs: ago(20 * DAY_MS),
    updatedAtMs: ago(5 * DAY_MS),
  });
  const triggers = [
    pollTrigger(
      BOT.ci,
      "ci-status-poll",
      {
        source: {
          kind: "http",
          url: "https://api.github.com/repos/acme/acme-web/actions/runs?status=completed&per_page=20",
          method: "GET",
          headers: { accept: "application/vnd.github+json" },
          auth: { grantId: GRANT.github, header: "authorization", scheme: "Bearer" },
        },
        intervalMs: 2 * MINUTE_MS,
        items: "$.workflow_runs[*]",
        cursor: { kind: "idSet", id: "id" },
      },
      {
        filter: 'data.conclusion == "failure"',
        route: { policy: "bot" },
        coalesce: { debounceMs: 60_000, maxWaitMs: 5 * MINUTE_MS, maxCount: 20 },
        deliver: { whenBusy: "queue" },
        cursorState: {
          ids: ["9140", "9151", "9160", "9172", "9181", "9185", String(CI_RUN_492), "9189", "9190", "9193", "9194", "9196"],
          consecutiveFailures: 0,
          baselinedAtMs: ago(20 * DAY_MS),
          lastPolledAtMs: ago(MINUTE_MS + 20_000),
        },
        createdAtMs: ago(20 * DAY_MS),
        updatedAtMs: ago(MINUTE_MS + 20_000),
      },
    ),
    inboxTrigger(BOT.ci, [BOT.implementer, BOT.reviewer], { route: { policy: "bot" }, deliver: { whenBusy: "queue" }, createdAtMs: ago(20 * DAY_MS) }),
  ];
  const main = managedSession(store, universe, { id: SESSION.ciMain, botId: BOT.ci, displayName: "CI Watch", profile: CI_PROFILE, tools: [...BOT_TOOLS, EMIT_TOOL], createdAtMs: ago(20 * DAY_MS) });
  introduce(
    store,
    universe,
    main,
    ago(20 * DAY_MS) + 40_000,
    "I'm CI Watch: every two minutes I poll GitHub Actions for acme/acme-web, and each failed run wakes me once. Task-branch failures go to implementer (with the task id) and pr-reviewer (with the PR number); main failures become issues. I have the GitHub App tools for logs and issues, no environment — I only read. Nothing missing.",
  );
  const mainRef = { sessionId: SESSION.ciMain, label: "Main" };
  const source = "poll:ci-status-poll";

  const log = eventLog(store, BOT.ci);
  const pollEvent = (
    run: { id: number; branch: string; pr: number | null; job: string; head: string; author: string },
    atMs: number,
    outcome: BotEventOutcome,
    detail: string,
    resolvedAfterMs: number,
  ): ScriptedEvent =>
    log.add({
      kind: "poll",
      source,
      at: atMs,
      summary: `ci-status-poll: run ${run.id} on ${run.branch}${run.pr === null ? "" : ` (#${run.pr})`} — failure in ${run.job}`,
      body: [`run_id: ${run.id}`, "workflow: ci.yml", `head_branch: ${run.branch}`, `head_sha: ${run.head}`, `actor: ${run.author}`, "conclusion: failure"],
      eventId: `poll:ci-status-poll:${run.id}`,
      session: mainRef,
      outcome,
      detail,
      resolvedAfterMs,
      data: {
        id: run.id,
        name: "ci.yml",
        head_branch: run.branch,
        head_sha: run.head,
        conclusion: "failure",
        actor: { login: run.author },
        pull_requests: run.pr === null ? [] : [{ number: run.pr }],
        html_url: `https://github.com/acme/acme-web/actions/runs/${run.id}`,
      },
    });
  const e1 = pollEvent(
    { id: 9140, branch: "main", pr: null, job: "lint", head: "3c7d9a1", author: "priya-n" },
    at(2, 11, 20),
    "handled",
    "main is red: lint failed on an unused import in src/auth/middleware.ts after #479 merged. Opened issue #489, assigned Priya.",
    90_000,
  );
  const e2 = pollEvent(
    { id: 9151, branch: "marco/ci-cache", pr: 486, job: "test (node 22)", head: "e1b7a04", author: "marco-r" },
    at(1, 15, 42),
    "ignored",
    "Run 9151 was cancelled by a newer push on the same branch; not a failure.",
    20_000,
  );
  const e3 = expectSeq(
    pollEvent(
      { id: CI_RUN_492, branch: TASK.b.branch, pr: TASK.b.pr, job: "test (node 22)", head: "1c40f7a", author: "lightspeed-factory[bot]" },
      p(10),
      "handled",
      `Task branch ${TASK.b.branch}: middleware.test.ts fails with RateLimit-Limit NaN. Sent ci.failure to implementer (#${SEQ.implCiFailure}) and pr-reviewer (#${SEQ.reviewerCiFailure}).`,
      span(10, 10.8),
    ),
    SEQ.ciFailure,
  );
  const e4 = expectSeq(
    receipt(log, {
      from: BOT.implementer,
      askedSeq: SEQ.implCiFailure,
      status: "handled",
      summary: `PR #${TASK.b.pr} opened; the CI failure steered in mid-run was the config default — fixed in 3f2a9c1 and pushed`,
      at: p(11.5),
      hops: 3,
      session: mainRef,
      outcome: "handled",
      detail: "implementer fixed run 9187's cause on the branch; nothing further.",
    }),
    SEQ.ciReceiptImpl,
  );
  const e5 = expectSeq(
    receipt(log, {
      from: BOT.reviewer,
      askedSeq: SEQ.reviewerCiFailure,
      status: "handled",
      summary: `Requested changes on #${TASK.b.pr}: default and validate RATE_LIMIT_PER_KEY (middleware.ts:13); same cause as CI run ${CI_RUN_492}`,
      at: p(12),
      hops: 4,
      session: mainRef,
      outcome: "handled",
      detail: "pr-reviewer folded the failure into its review; nothing further.",
    }),
    SEQ.ciReceiptReviewer,
  );
  const e6 = pollEvent(
    { id: 9193, branch: "main", pr: null, job: "e2e (checkout)", head: "e41b2c7", author: "priya-n" },
    p(23),
    "handled",
    "Runs 9193 and 9194 are the same e2e job on main timing out against the Stripe sandbox (no code change between them). One issue, #494.",
    span(23, 27),
  );
  const e7 = pollEvent(
    { id: 9194, branch: "main", pr: null, job: "e2e (checkout)", head: "e41b2c7", author: "priya-n" },
    p(26),
    "handled",
    "Runs 9193 and 9194 are the same e2e job on main timing out against the Stripe sandbox (no code change between them). One issue, #494.",
    span(26, 27),
  );

  e1.envelope.runId = appendScriptedRun(store, main, {
    at: at(2, 11, 21),
    user: e1.prompt,
    steps: [
      {
        tools: [
          github(
            "get_job_logs",
            { ...REPO, run_id: 9140, job: "lint", tail_lines: 20 },
            "run 9140 · lint",
            "src/auth/middleware.ts\n  3:10  error  'JWTPayload' is defined but never used  @typescript-eslint/no-unused-vars\n\n✖ 1 problem (1 error, 0 warnings)",
          ),
        ],
      },
      {
        tools: [
          github(
            "create_issue",
            { ...REPO, title: "main is red: lint fails on unused import in src/auth/middleware.ts (run 9140)", assignees: ["priya-n"], labels: ["ci"] },
            "issue for main",
            "#489 created, assigned priya-n",
          ),
        ],
        text: "main is red after #479: `lint` fails on an unused `JWTPayload` import in `src/auth/middleware.ts:3`. Opened #489 with the log and assigned Priya, who merged #479.",
      },
    ],
  }).id;
  e2.envelope.runId = appendExchange(store, universe, main, {
    at: at(1, 15, 43),
    user: e2.prompt,
    turn: { text: "Run 9151 on marco/ci-cache was cancelled by a newer push (run 9160 on the same branch passed). Not a failure; nothing to send." },
  }).id;
  e3.envelope.runId = appendScriptedRun(store, main, {
    at: p(10.1),
    user: e3.prompt,
    steps: [
      {
        thinking: "A task branch: lin-1421-b. Name the failing test from the log, then tell implementer (task id from the branch) and pr-reviewer (PR number).",
        tools: [github("get_job_logs", { ...REPO, run_id: CI_RUN_492, job: "test (node 22)", tail_lines: 40 }, `run ${CI_RUN_492} · test (node 22)`, CI_LOG_9187)],
      },
      {
        tools: [
          botEmit(
            {
              to: BOT.implementer,
              kind: "ci.failure",
              summary: `CI run ${CI_RUN_492} on ${TASK.b.branch} (#${TASK.b.pr}) failed: test (node 22) — middleware.test.ts, RateLimit-Limit is NaN`,
              data: {
                run: CI_RUN_492,
                pr: TASK.b.pr,
                branch: TASK.b.branch,
                task: { id: TASK.b.id },
                job: "test (node 22)",
                failing: "src/ratelimit/middleware.test.ts › passes requests under the limit",
              },
              reply: true,
            },
            SEQ.implCiFailure,
          ),
          botEmit(
            {
              to: BOT.reviewer,
              kind: "ci.failure",
              summary: `CI run ${CI_RUN_492} on ${TASK.b.branch} (#${TASK.b.pr}) failed: test (node 22) — middleware.test.ts, RateLimit-Limit is NaN`,
              data: {
                run: CI_RUN_492,
                pr: TASK.b.pr,
                branch: TASK.b.branch,
                task: { id: TASK.b.id },
                job: "test (node 22)",
                failing: "src/ratelimit/middleware.test.ts › passes requests under the limit",
              },
              reply: true,
            },
            SEQ.reviewerCiFailure,
          ),
        ],
        text: `Run ${CI_RUN_492} on \`${TASK.b.branch}\` (#${TASK.b.pr}) failed \`test (node 22)\`: three cases in \`src/ratelimit/middleware.test.ts\`, all with \`RateLimit-Limit: NaN\` — the limiter has no limit configured in the CI job. Sent ci.failure to implementer for task ${TASK.b.id} (their #${SEQ.implCiFailure}) and to pr-reviewer for #${TASK.b.pr} (their #${SEQ.reviewerCiFailure}).`,
      },
    ],
  }).id;
  e4.envelope.runId = appendExchange(store, universe, main, {
    at: p(11.55),
    user: e4.prompt,
    turn: { text: `implementer took run ${CI_RUN_492}'s failure as its own and pushed a fix on \`${TASK.b.branch}\`; the re-run shows up in my next poll. Nothing further.` },
  }).id;
  e5.envelope.runId = appendExchange(store, universe, main, {
    at: p(12.05),
    user: e5.prompt,
    turn: { text: `pr-reviewer folded run ${CI_RUN_492} into its review of #${TASK.b.pr}. Both receivers have acted; this run is closed on my side.` },
  }).id;
  const batchRun = appendScriptedRun(store, main, {
    at: p(26.2),
    user: `${e6.prompt}\n\n${e7.prompt}`,
    steps: [
      {
        thinking: "Two failures of the same e2e job on main, same commit, four minutes apart: infrastructure until proven otherwise. Read the second log.",
        tools: [
          github(
            "get_job_logs",
            { ...REPO, run_id: 9194, job: "e2e (checkout)", tail_lines: 30 },
            "run 9194 · e2e (checkout)",
            "  ✗ checkout › completes a card payment (30012ms)\n    TimeoutError: waiting for https://api.stripe.com/v1/payment_intents (test mode) exceeded 30000ms\n\n  1 failed, 11 passed",
          ),
        ],
      },
      {
        tools: [
          github(
            "create_issue",
            { ...REPO, title: "e2e (checkout) timing out against the Stripe sandbox on main (runs 9193, 9194)", labels: ["ci", "flaky"] },
            "one issue for both runs",
            "#494 created",
          ),
        ],
        text: "Runs 9193 and 9194 are the same `e2e (checkout)` job on the same main commit (`e41b2c7`), both timing out on Stripe's test-mode API after 30 s. No code changed between them, so this is the sandbox, not us. One issue, #494, unassigned; no one to blame for a third party's latency.",
      },
    ],
  });
  e6.envelope.runId = batchRun.id;
  e7.envelope.runId = batchRun.id;

  universe.bots.set(BOT.ci, {
    bot: record,
    triggers: new Map(triggers.map((entry) => [entry.triggerId, entry])),
    events: log.events,
    state: botState({
      bot: record,
      sessions: [botSession(main, "main")],
      recentEvents: [
        recent(e3.envelope, { inputTokens: 9_400, cachedInputTokens: 8_100 }),
        recent(e4.envelope, { inputTokens: 10_900, cachedInputTokens: 10_500 }),
        recent(e5.envelope, { inputTokens: 11_300, cachedInputTokens: 10_900 }),
        { ...recent(e6.envelope, { inputTokens: 12_800, cachedInputTokens: 11_600 }), seqs: [6, 7] },
      ],
      eventsProcessed: 212,
      appliedProfileRevision: CI_PROFILE.revision,
      runsToday: 4,
    }),
    descendants: [],
  });
}

// --- release-scribe --------------------------------------------------------

function seedReleaseScribe(store: DemoStore, universe: UniverseState): void {
  const record = bot(universe, {
    botId: BOT.scribe,
    displayName: "Release Scribe",
    description: "Keeps CHANGELOG.md's Unreleased section current from pr-reviewer's notes and drafts Friday's release notes as a docs PR.",
    profileId: PROFILE.scribe,
    brief: [
      "You are Release Scribe for acme/acme-web.",
      "",
      "A release.note from another bot is a customer-visible change: draft it into the Unreleased section of CHANGELOG.md in the acme-web workspace at once, in the changelog's voice, with the PR numbers. Internal changes are not customer notes: say so and do nothing. On the Friday schedule, list commits and merged PRs since the last tag, reconcile them with Unreleased, and open a PR under docs/releases/.",
    ].join("\n"),
    runsPerDay: 10,
    breaker: null,
    emit: false,
    createdAtMs: ago(30 * DAY_MS),
    updatedAtMs: ago(9 * DAY_MS),
  });
  const triggers = [
    scheduleTrigger(
      BOT.scribe,
      "friday-notes",
      { cron: "0 16 * * 5", summary: "Every Friday afternoon, draft release notes for everything merged since the last tag and open a docs PR." },
      { route: { policy: "bot" }, deliver: { whenBusy: "queue" }, createdAtMs: ago(30 * DAY_MS) },
    ),
    inboxTrigger(BOT.scribe, undefined, { route: { policy: "bot" }, deliver: { whenBusy: "queue" }, createdAtMs: ago(30 * DAY_MS) }),
  ];
  const main = managedSession(store, universe, { id: SESSION.scribeMain, botId: BOT.scribe, displayName: "Release Scribe", profile: SCRIBE_PROFILE, tools: BOT_TOOLS, createdAtMs: ago(30 * DAY_MS) });
  introduce(
    store,
    universe,
    main,
    ago(30 * DAY_MS) + 50_000,
    "I'm Release Scribe: I keep the Unreleased section of CHANGELOG.md current from the notes other bots send me, and every Friday afternoon I draft the release notes from everything merged since the last tag and open them as a docs PR. I wake on the Friday schedule and on my inbox, which is open to every bot here; I have the GitHub tools and read-write access to the acme-web workspace. No environment, which is right — I only write prose.",
  );
  const mainRef = { sessionId: SESSION.scribeMain, label: "Main" };

  const log = eventLog(store, BOT.scribe);
  const friday = (atMs: number, detail: string): ScriptedEvent =>
    log.add({
      kind: "schedule",
      source: "schedule:friday-notes",
      at: atMs,
      summary: "Friday release notes.",
      body: ["cron: 0 16 * * 5 (Europe/Berlin)"],
      eventId: `schedule:friday-notes:${atIso(atMs).slice(0, 10)}`,
      session: mainRef,
      outcome: "handled",
      detail,
      resolvedAfterMs: 150_000,
    });
  const note = (atMs: number, summary: string, data: Record<string, unknown>, outcome: BotEventOutcome, detail: string): ScriptedEvent =>
    log.add({
      kind: "release.note",
      source: `bot:${BOT.reviewer}`,
      at: atMs,
      summary,
      eventId: `bot:${BOT.reviewer}:${hex(`release.note:${String(data.pr)}`, 12)}`,
      session: mainRef,
      sender: BOT.reviewer,
      hops: 1,
      outcome,
      detail,
      resolvedAfterMs: 45_000,
      data,
    });
  const e1 = friday(at(21, 16, 0), "v2.12 notes: 14 changes, the billing ledger rewrite called out. Docs PR #455.");
  const e2 = friday(at(14, 16, 0), "v2.13 notes: 9 changes, 2 breaking (config keys renamed). Docs PR #466.");
  const e3 = log.add({
    kind: "operator.requested",
    source: "manual",
    at: at(10, 14, 12),
    summary: "Test event from Marco: is this thing on?",
    eventId: `manual:${hex("scribe-test-event", 12)}`,
    session: mainRef,
    outcome: "ignored",
    detail: "Test event; replied with the schedule and inbox. Nothing to write.",
    resolvedAfterMs: 20_000,
    data: { text: "is this thing on?" },
  });
  const e4 = friday(at(7, 16, 0), "v2.14 notes: 8 changes, bounded scheduler retries marked breaking. Docs PR #484.");
  const e5 = expectSeq(
    note(
      at(3, 10, 8),
      "#486 restores the npm cache in CI; internal, not customer-facing.",
      { pr: 486, area: "internal", breaking: false, note: "CI restores the npm cache dropped in #483; install is back under a minute." },
      "ignored",
      "Internal change; not customer notes. Nothing written.",
    ),
    SEQ.scribeNote486,
  );
  const e6 = expectSeq(
    note(
      p(13.5),
      `${ISSUE}: per-API-key rate limits on /api — 600/min default, 429 with Retry-After; customer-visible.`,
      {
        pr: [491, 492],
        issue: ISSUE,
        area: "api",
        breaking: false,
        note: "Per-API-key rate limits on /api: 600 requests per minute per key by default, browser sessions bucketed per org; 429 with Retry-After and RateLimit-* headers.",
      },
      "handled",
      "Drafted the LIN-1421 entry under Unreleased › API in CHANGELOG.md (#491, #492).",
    ),
    SEQ.scribeNote1421,
  );

  const fridayRun = (session: ScriptedEvent, atMs: number, since: string, commits: string, pr: number, version: string, text: string) =>
    appendScriptedRun(store, main, {
      at: atMs,
      user: session.prompt,
      steps: [
        { tools: [github("list_commits", { ...REPO, sha: "main", since }, `main since ${since}`, commits)] },
        {
          tools: [
            github(
              "create_pull_request",
              { ...REPO, title: `docs: release notes ${version}`, head: `release-scribe/${version}`, base: "main" },
              "docs PR",
              `#${pr} created: docs/releases/${version}.md`,
            ),
          ],
          text,
        },
      ],
    }).id;
  e1.envelope.runId = fridayRun(
    e1,
    at(21, 16, 0),
    "v2.11",
    "19 commits (14 PRs, 5 dependency bumps)",
    455,
    "v2.12",
    "Opened #455 with the v2.12 notes: 14 changes; the billing ledger rewrite (#448) gets its own paragraph because it changes invoice numbering. Dependency bumps omitted.",
  );
  e2.envelope.runId = fridayRun(
    e2,
    at(14, 16, 0),
    "v2.12",
    "12 commits (9 PRs, 3 dependency bumps)",
    466,
    "v2.13",
    "Opened #466 with the v2.13 notes: 9 changes, two breaking (the `SCHEDULER_*` config keys were renamed in #459 and #461) listed first with the old and new names.",
  );
  e3.envelope.runId = appendExchange(store, universe, main, {
    at: at(10, 14, 12),
    user: e3.prompt,
    turn: { text: "On. I write on Fridays at 16:00 (Europe/Berlin) and whenever a bot sends me a release.note; the inbox is open to every bot here. Nothing to write from a test event." },
  }).id;
  e4.envelope.runId = fridayRun(
    e4,
    at(7, 16, 0),
    "v2.13",
    "11 commits (8 PRs, 3 dependency bumps): #467 #469 #471 #472 #473 #475 #477 #478",
    484,
    "v2.14",
    "Opened #484 with the v2.14 notes: 8 changes across scheduler (bounded retries — marked breaking, with the requeue endpoint), API, auth, and billing; the three dependency bumps are omitted. @-mentioned Marco on #469 and Priya on #473 for wording.",
  );
  e5.envelope.runId = appendExchange(store, universe, main, {
    at: at(3, 10, 9),
    user: e5.prompt,
    turn: {
      text: "#486 restores the npm cache in CI — internal, no customer-visible behaviour. Not for the changelog; it can go in the v2.14.1 internal line if Friday's diff makes one. Nothing written.",
    },
  }).id;
  e6.envelope.runId = appendScriptedRun(store, main, {
    at: p(13.6),
    user: e6.prompt,
    steps: [
      {
        thinking: "Customer-visible: a new 429 that integrations will hit. Read Unreleased so the entry goes under the right heading, then draft it in the changelog's voice with both PR numbers.",
        tools: [vfsReadFile("/acme-web/CHANGELOG.md", CHANGELOG_BASE)],
      },
      {
        tools: [vfsWriteFile("/acme-web/CHANGELOG.md", CHANGELOG_WITH_ENTRY, "Unreleased › API: LIN-1421")],
        text: [
          "Drafted into `CHANGELOG.md` under **Unreleased › API**:",
          "",
          "> **Per-API-key rate limits on `/api`** (LIN-1421). Each API key gets 600 requests per minute by default (`RATE_LIMIT_PER_KEY`), with a burst of one minute's worth; browser sessions are bucketed per org. Over the limit, requests get `429` with `Retry-After` in seconds and `RateLimit-Limit` / `RateLimit-Remaining` headers. Integrations that retry on 5xx only should also honour 429. (#491, #492)",
          "",
          "Not marked breaking — 600/min is above any integration's observed rate — but the last sentence is there because a client that treats 429 as fatal will notice. Friday's notes pick this up; #493 (metrics) is internal and will not get a line.",
        ].join("\n"),
      },
    ],
  }).id;

  universe.bots.set(BOT.scribe, {
    bot: record,
    triggers: new Map(triggers.map((entry) => [entry.triggerId, entry])),
    events: log.events,
    state: botState({
      bot: record,
      sessions: [botSession(main, "main")],
      recentEvents: [
        recent(e4.envelope, { inputTokens: 38_600, cachedInputTokens: 12_100 }),
        recent(e5.envelope, { inputTokens: 12_400, cachedInputTokens: 11_700 }),
        recent(e6.envelope, { inputTokens: 15_900, cachedInputTokens: 12_300 }),
      ],
      eventsProcessed: 23,
      appliedProfileRevision: SCRIBE_PROFILE.revision,
      runsToday: 1,
    }),
    descendants: [],
  });
}

// ---------------------------------------------------------------------------
// Scripted replies for live interaction
// ---------------------------------------------------------------------------

function implementReply(turn: number): DemoTurn {
  if (turn === 1) {
    return {
      thinking: "**Same way the pipeline does it**\n\nRead the spec for what exists, delegate the repository question, run the tests that matter, then say what the tasks are.",
      tools: [
        vfsReadFile(SPEC_PATH, SPEC_LIN_1421),
        agentRun(PROFILE.explorer, EXPLORE_BUCKET_TASK, EXPLORE_BUCKET_REPORT),
        runCommand(["npx", "vitest", "run", "src/ratelimit"], VITEST_BUCKET_OUTPUT),
      ],
      text: [
        `Here is how I would build it, the way the factory did for ${ISSUE}:`,
        "",
        "1. **Token bucket** — `src/ratelimit/bucket.ts`, refill credited from whole intervals so a hot caller cannot refill itself (the #482 regression), with tests on an injected clock. *Done in #491.*",
        "2. **Middleware and config** — mount after `requireSession` on `/api`, `X-Api-Key` or an org bucket, 429 with `Retry-After`. *Done in #492, after one CI round-trip on the config default.*",
        "3. **Metrics and docs** — hashed-key counter and a `Retry-After` histogram on the existing registry, the customer page. *Open as #493, awaiting review.*",
        "",
        "The spec has one open question (whether `/api` only), answered by assumption. Tell me which task to start on, or say *open a PR* and I take the next one on the plan.",
      ].join("\n"),
    };
  }
  if (turn === 2) {
    return {
      tools: [
        commit("lin-1421-d-wiring", "d41f0a9", `ratelimit: observe decisions from the middleware (${ISSUE})`, "1 file changed, 3 insertions(+)"),
        github(
          "create_pull_request",
          { ...REPO, title: `ratelimit: observe decisions from the middleware (${ISSUE})`, head: "lin-1421-d-wiring", base: "main" },
          "#495",
          "#495 created: ratelimit: observe decisions from the middleware (LIN-1421)\nlin-1421-d-wiring → main · +3 −0 · 1 file · checks: queued",
        ),
      ],
      text: "Opened #495: the two-line wiring of `observe(key, result)` into the middleware that the plan left for after #492 and #493 merge — it is stacked on both, so it turns green once they land. PR Reviewer picks it up from the webhook.",
    };
  }
  return {
    text: "That is the LIN-1421 work covered. I can start the `#472` limiter deletion the plan mentioned, replay the failed review of #493 now that the provider is back, or take the next ready-for-build issue from the backlog — say which.",
  };
}

function flakyTestReply(turn: number): DemoTurn {
  if (turn === 1) {
    return {
      thinking:
        "**Narrowing down a timing flake**\n\nIntermittent failures that never reproduce locally usually mean the test waits on real time. Find the test, then read it next to the code it exercises.",
      tools: [
        runCommand(["rg", "-n", "retries after a transient failure", "src"], 'src/scheduler/retry.test.ts:11:  it("retries after a transient failure", async () => {'),
        readFile("src/scheduler/retry.test.ts", RETRY_TEST_BEFORE),
      ],
      text: [
        "The flaky one is `retries after a transient failure` in `src/scheduler/retry.test.ts:11`. It asserts that the whole call finishes in under 100 ms, but the retry policy sleeps a *random* `[0, 100)` ms before the second attempt — add a slow runner and the assertion loses a coin toss.",
        "",
        "`withRetry` already accepts injectable `random` and `sleep`, so the fix is mechanical: fake timers, `random: () => 0.5`, and assert on the sleep the policy asked for instead of `Date.now()`.",
        "",
        "Shall I apply it and run the scheduler suite?",
      ].join("\n"),
    };
  }
  if (turn === 2) {
    return {
      thinking: "**Applying the fix**\n\nRewrite the test with fake timers and a pinned random, then run the scheduler tests.",
      tools: [writeFile("src/scheduler/retry.test.ts", RETRY_TEST_AFTER, "fake timers, pinned jitter"), runCommand(["npx", "vitest", "run", "src/scheduler"], VITEST_SCHEDULER_OUTPUT)],
      text: "Applied and green — 15 tests in 1.4 s. The test now drives the retry with `vi.runAllTimersAsync()` and asserts `sleep` was asked for 25 ms (base 50 × 2⁰ × 0.5), so wall time never enters. I also added a case for the dead-letter path, which had no coverage. The change is uncommitted in the sandbox; I can open a PR if you want.",
    };
  }
  return {
    text: "That's the scheduler test done. From here I could open the PR with a short description, run the full suite once more under `--repeat=20` to be sure nothing else flakes, or look at the `lockedAt` wall-clock read in `scheduler.ts:88` that the audit flagged — which would you like?",
  };
}

function reviewReply(turn: number): DemoTurn {
  if (turn === 1) {
    return {
      thinking:
        "**Reading the PR before opining**\n\nThe open one is #493 — the review the bot deferred after a provider error. Read its description and diff; label cardinality is the risk in a metrics change.",
      tools: [
        github(
          "get_pull_request",
          { ...REPO, pull_number: TASK.c.pr },
          `#${TASK.c.pr}`,
          `#${TASK.c.pr} ${TASK.c.prTitle} · lightspeed-factory[bot] → main · open · review pending\n2 files changed · +61 −0 · checks: passing`,
        ),
        github("get_pull_request_diff", { ...REPO, pull_number: TASK.c.pr }, `#${TASK.c.pr} diff`, "(2 files; src/ratelimit/metrics.ts, docs/api/limits.md)"),
      ],
      text: [
        `**#${TASK.c.pr} — ${TASK.c.prTitle}** (implementer, 2 files, +61 −0)`,
        "",
        "**Blocking (0)** — nothing. The key label is hashed to eight hex digits before it reaches Prometheus, so cardinality is bounded by distinct callers and no secret is exposed; the histogram is observed only on refusals, which is what the spec asks for.",
        "",
        "**Comments (2)**",
        "- `src/ratelimit/metrics.ts:26` — FNV-1a over the key is fine for a label, but say so in the comment: someone will want to reverse it to a customer and cannot.",
        "- `docs/api/limits.md` — the burst sentence should name `RATE_LIMIT_BURST` for operators, or move to `docs/config.md`.",
        "",
        "Verdict: approve with comments. Post it?",
      ].join("\n"),
    };
  }
  if (turn === 2) {
    return {
      tools: [review(TASK.c.pr, "APPROVE", "approve with 2 comments", 2248011)],
      text: `Posted the review on #${TASK.c.pr} as *approve* with the two comments inline. All three LIN-1421 pull requests are now approved; a person merges them, and the implementer's #495 wiring turns green once they land.`,
    };
  }
  return {
    text: "Nothing else is waiting on a review. I could re-check #487 (still waiting on a merge), look at whether the org bucket for browser sessions needs its own limit, or summarise this week's reviews for the Friday notes — say which.",
  };
}

function statusReply(turn: number): DemoTurn {
  if (turn === 1) {
    return {
      thinking: "**One issue, six bots**\n\nWalk the chain by event number rather than by bot, so the reader can follow the handoffs.",
      text: [
        `## ${ISSUE} — Per-API-key rate limits`,
        "",
        "| step | bot | event | outcome |",
        "|---|---|---|---|",
        `| 1 | intake | #4 issue.labeled (Linear webhook) | spec written to \`${SPEC_PATH}\`; Linear 401 kept the clarifying question in the spec |`,
        `| 2 | planner | #${SEQ.plannerSpec} feature.spec | three tasks in \`${PLAN_PATH}\`, task.ready ×3 |`,
        `| 3 | implementer | #${SEQ.implTaskA} / #${SEQ.implTaskB} / #${SEQ.implTaskC} task.ready | PRs #491, #492, #493 from three sandboxes; 5 sub-agents |`,
        `| 4 | ci-watch | #${SEQ.ciFailure} poll (run ${CI_RUN_492}) | ci.failure steered into implementer #${SEQ.implCiFailure} and pr-reviewer #${SEQ.reviewerCiFailure} |`,
        `| 5 | pr-reviewer | #${SEQ.reviewerPr491}, #${SEQ.reviewerPr492}, #${SEQ.reviewerSync492} | #491 approved; #492 changes requested → approved on synchronize |`,
        `| 6 | release-scribe | #${SEQ.scribeNote1421} release.note | CHANGELOG.md Unreleased entry drafted |`,
        "",
        `**Open:** #493 awaits review (pr-reviewer's #${SEQ.reviewerPr493} failed on a provider 529 and #11 deferred it to the morning triage); test-writer's metrics tests are still a pending promise on task c. **Waiting on people:** merges of #491 and #492, and an answer to the spec's open question on Linear.`,
      ].join("\n"),
    };
  }
  if (turn === 2) {
    return {
      text: `Two things would move it: replay pr-reviewer's #${SEQ.reviewerPr493} from its Activity tab now that the provider is back, and reconnect the Linear server on the Integrations page so intake can post the open question. Everything else is a merge button.`,
    };
  }
  return { text: "Status unchanged since the last summary. Ask for a specific bot or thread and I'll go into its events." };
}

function defaultReply(turn: number): DemoTurn {
  if (turn === 1) {
    return {
      text: [
        "Happy to help. I'm working in a fresh sandbox with acme-web checked out, with file and process tools, GitHub access through the App installation, sub-agents, and the specs workspace. Three concrete things I can do right now:",
        "",
        "1. **Build a feature the way the pipeline does** — read the spec, delegate exploration, write, test, open the PR.",
        "2. **Review a pull request** — read the diff, run the affected tests, post one clear verdict (#493 is waiting).",
        "3. **Report the status of LIN-1421** across the six bots, by event number.",
        "",
        "Or point me at a file, a PR, or a failing test and I'll start from there.",
      ].join("\n"),
    };
  }
  if (turn === 2) {
    return {
      text: "Understood. To be useful rather than general: give me a file path, a PR number, an issue key, or an error message and I'll go look. If you'd rather I pick, the open item with the most leverage is #493 — one deferred review away from LIN-1421 being fully approved.",
    };
  }
  return {
    text: "Still here. I can implement (say an issue key), review (say a PR number), chase a failing test (say its name), or summarise the pipeline (say *status*). Anything in this repository is fair game; I won't push to main or merge.",
  };
}

const responder: DemoResponder = (input, context) => {
  const text = input.toLowerCase();
  if (/\b(status|pipeline|progress|where are we)\b/.test(text)) return statusReply(context.turn);
  if (/\b(implement|feature|build|ship)\b/.test(text)) return implementReply(context.turn);
  if (/\b(tests?|flaky|failing|vitest)\b/.test(text)) return flakyTestReply(context.turn);
  if (/\b(review|pr|pull request|#\d+)\b/.test(text)) return reviewReply(context.turn);
  return defaultReply(context.turn);
};

// ---------------------------------------------------------------------------
// Seed
// ---------------------------------------------------------------------------

export function seedSoftwareFactory(store: DemoStore): void {
  const universe = store.addUniverse({
    id: SOFTWARE_FACTORY_UNIVERSE_ID,
    slug: SOFTWARE_FACTORY_SLUG,
    name: "Software Factory",
    lightspeedUniverseId: ENGINE_UNIVERSE_ID,
    role: "owner",
    createdAt: agoIso(70 * DAY_MS),
    responder,
  });
  seedMembers(store, universe);
  seedProfiles(universe);
  seedWorkspaces(store, universe);
  seedEnvironments(universe);
  seedIntegrations(universe);
  seedSessions(store, universe);
  seedBots(store, universe);
}

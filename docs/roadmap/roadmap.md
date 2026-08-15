# Lightspeed Roadmap

## Work
- [ ] [P124](p124-first-party-platform-monorepo.md) — import the complete
  first-party TypeScript platform into Lightspeed, neutralize product branding
  with a greenfield durable-identity reset, enforce atomic cross-language contract
  checks, extend P123 to publish every selected component in one coherent
  release, and reduce the private repository to deployment operations.
- [x] [P123](p123-build-and-release.md) — Lightspeed-owned release artifacts,
  embedded ledgered migrations, one-build packaging, automatic coherent
  snapshots after successful `main` CI, and independent exact-commit SemVer
  releases. Lightspeed side complete 2026-08-14 except for the intentionally
  deferred deployment-repository dispatch; isolated-runner provisioning and consumer cutover
  remain external.
- [x] [P100](p100-workflow-tool-ports.md) — the workflow emission substrate:
  one envelope and one fixed `deliver_emission` signal for all cross-workflow
  facts (run-terminal notifications and env-job source resolutions folded in
  a single push, deleting both promise-specific signals), plus workflow-bound
  tools — schema-validated function tools whose calls become log-backed
  emissions for one fixed opaque receiver per binding, declared by trusted
  workflow plugins at managed-session creation (no built-in endpoint
  registry, no plugin-specific session-worker code); Accepted tools are
  pull-consumed at run boundaries, with promise-bearing push delivery owned by
  P100b. Complete 2026-07-28: generic `WorkflowTool*` vocabulary, immutable
  managed bindings, atomic emissions, receiver-authorized paginated pull,
  retry/restart/continue-as-new coverage, and compatibility gates are closed
- [x] [P100b](p100b-workflow-backed-tools.md) — workflow-backed tool
  interactions over P100/P92: push to bound workflow executions (derived from
  completion, no per-binding delivery mode), keyed promise-set completion
  (request/reply as the single-key case), deterministic start-on-call
  workflow tools, one unified `PromiseSource::Workflow`, and plugin-owned
  Temporal workers/activities with no second executor protocol;
  primitive-only, with no production feature migrations. B1-B6 completed
  2026-07-26 incl. the live plugin-worker suite (workflow_tool_plugins_live,
  11 scenarios), hard-promise-deadline enforcement, and concurrency-toolset
  derivation for promise-bearing bindings. B7 completed 2026-07-28: one
  workflow may be both lifecycle controller and promise-bearing receiver only
  with a non-zero hard deadline; the 13-scenario live suite proves the
  Channels-shaped enqueue/reply-before-terminal flow, deduplication,
  controller continue-as-new, stalled-controller deadline, and absent receiver
- [ ] [P101](p101-durable-work-workflow.md) — durable Work as a Temporal-owned
  goal loop over one managed session and many execution runs; explicit
  completion/blockage reports over P100 workflow tools, automatic
  continuation, caller
  input, and reuse of Fleet promises/run notifications without Work-specific
  transport
- [x] [P102](p102-workflow-plugin-extractions.md) — completed 2026-07-29:
  environment-job dispatch adopted P100b internally while remaining core;
  channel integration moved to the external Channels application, allowing
  the built-in messaging feature, tools, outbox store, and delivery APIs to be
  deleted from Lightspeed
- [x] [P103](p103-managed-session-api.md) — expose the existing trusted
  managed-session admission through the universe-authenticated main API by
  adding `session/managed/start` with the complete bound/start and
  Accepted/keyed-Promise workflow-tool vocabulary, and let
  `session/runs/start` optionally notify the session's immutable lifecycle
  controller; keep ordinary session creation unchanged and reuse the existing
  emission, Promise, event, and blob protocols
- [x] [P104](p104-provider-owned-environment-jobs.md) — completed 2026-07-29:
  remove the PostgreSQL environment-job registry and job listing surfaces;
  keep provider-direct
  read/cancel, let providers reject close or interrupt active jobs, and rely
  on Temporal plus provider idempotency instead of stored job/group rows
- [x] [P106](p106-joined-workflow-tools.md) — completed 2026-07-30: add Joined as the ordinary
  single-result workflow-tool form: durably park and resume the original tool
  call without exposing a Promise or requiring model-authored `await`; make
  bound pull/push dispatch independent of Accepted/Joined/Promises completion,
  motivated by the first production Channels sessions where every provider
  receipt currently costs an otherwise unnecessary await tool round. Advanced
  2026-07-30 through the greenfield v4 dispatch foundation, pushed Accepted,
  engine-native Joined, shared declaration readback, durable event diagnostics,
  generated contracts, and passing serial live proofs
- [x] [P111](p111-promise-result-materialization.md) — completed 2026-08-01:
  make explicit `await` return one structured tool result containing every observed Promise value,
  remove Promise-derived synthetic user messages, retain one root ref per
  Promise with structured child refs, and normalize environment-job byte
  chunks into readable text or typed CAS references before Promise resolution
- [x] [P112](p112-joined-environment-job-run.md) — add `job_run` as the
  single-job `Start + Joined` environment surface, returning P111's normalized
  terminal result directly while retaining `job_submit` as the asynchronous
  dependency-group and keyed-Promise form

## Core
- [x] [P91](p91-core-agent-structure-cleanup.md) — cleanup of CoreAgent structures: delete the SDK-era open-kernel layer, commit to a closed event vocabulary and core FSM
- [x] [P95](p95-config-redesign.md) — config redesign: full-document puts with expected revisions, feature-oriented capability config (secure by default), feature versioning, derived toolset; removes patch semantics and the unused `session/messages/submit` RPC surface
- [x] [P98](p98-context-revisions-and-instruction-reconciliation.md) — optional context-edit revision guards and atomic effective-instruction reconciliation, with the product default active only as a true fallback
- [x] [P107](p107-session-workspace-links.md) — move session VFS bindings into
  `features.vfs.workspaceLinks`, derive filesystem/runtime projection from
  config plus the VFS catalog, remove the `vfs_mounts` table and mount APIs,
  and preserve dangling links when referenced workspaces are deleted

## Hosted Runtime
- [ ] [P105](p105-unbounded-hosted-runs.md) — remove the hosted
  `max_steps_per_input = 128` ceiling; let one logical run execute for hours
  across history-driven Temporal continue-as-new boundaries, preserving
  durable progress and transient transport state without fixed-step rollover
  or workflow failure
- [ ] [P109](p109-runtime-state-handoff.md) — remove owning-session log replay
  from ordinary tool, Promise, environment, environment-job, and Fleet runtime
  paths by carrying bounded facts from the Temporal workflow's current
  `CoreAgentState`; retain replay for bootstrap, continue-as-new, recovery, and
  explicit API/history reads

## Fleet (sub-agents)
- [x] [P82](p82-session-graph-fork-clone.md) — session graph foundation: clone, fork (by-reference), and links in the store
- [x] [P83](p83-fleet-subagent-control-plane.md) — agent-facing Fleet control plane (spawn/task/read/list/cancel) on top of P82
- [x] [P84](p84-fleet-wait-and-callbacks.md) — first cut complete: `agent_send`, generic deferred tool batches, `RunSubscription` workflow primitives, `agent_wait` DTO/preflight/parking/resume, and live Mode I/Mode W coverage
- [x] [Appendix: Fleet one-off child lifecycle](appendix-fleet-one-off-lifecycle.md) — `agent_spawn.lifecycle.close_on_terminal` for ephemeral delegation sessions
- [x] agent profiles
- [x] start new sessions with profiles and ad-hoc profiles
- [x] [P92](p92-unified-suspension.md) — unified suspension: promises + one `await`, cancellation-as-resolution, watchdogs, force-close, mailbox unification; motivated by the 2026-07-06 stuck-`cancelling` incident

## Provider Integrations
- [x] [P97](p97-model-discovery.md) — direct provider model discovery for
  `models/list` (OpenAI Responses and Anthropic Messages)
- [ ] support and test completions api
   - test with OAI
   - test with open router
   - test with self-hosted model
- [ ] incremental tool discovery support (at least OAI)

## Environments & Sandboxes
- [x] [P117](p117-environment-compute-plan.md) — agreed target architecture for
  operator-registered reachable compute providers, lightweight universe
  bindings, provider-wide offering/ingress policy, stable environment
  identity, reachable external environments, on-demand data connections,
  managed Incus VMs, public ingress, and standalone/native-cluster Incus modes;
  delivery is split across P118-P122
- [ ] [P118](p118-environment-domain-and-lifecycle.md) — replace the
  provider-as-singleton model with operator providers, revisioned universe
  routing/admission bindings, transient controller initialization,
  incarnation-scoped physical facts, idempotent asynchronous lifecycle, and a
  Lightspeed-owned reconciler over a provider-owned policy boundary
- [ ] [P119](p119-environment-daemon-gateway-enrollment.md) — core passive
  `lightspeed-envd` and on-demand gateway data plane implemented; packaging,
  deployment authentication, and product UX remain follow-ups
- [ ] [P120](p120-incus-environment-provider.md) — core standalone stateless
  Incus controller, passive on-demand data endpoint, immutable image recipe, provider-wide policy,
  and durable VM provisioning are implemented; hz01/hz02 deployment, image
  publication, platform UX, and live isolation/acceptance proofs remain
- [ ] [P121](p121-environment-public-ingress.md) — add provider-authorized
  per-environment HTTPS ingress through a shared node-edge proxy with wildcard
  DNS/TLS and no agent or platform credential inside the VM; core API,
  protocol, Incus-provider, and stateless proxy are implemented, while
  deployment and live acceptance remain
- [ ] [P122](p122-incus-multi-node-pool.md) — run the stateless Incus provider
  against either one standalone server or one native Incus cluster, with API
  endpoint failover, cluster-group placement, bounded topology health, and
  explicit non-destructive member-loss behavior; live cluster acceptance and
  deployment remain
- [x] [P113](p113-explicit-vfs-and-environment-tool-domains.md) — separate
  dedicated VFS tools from ordinary active-environment file/process tools,
  remove generic execution targets and the fused filesystem, and make prompts
  and the existing skill catalog explicitly VFS-owned
- [x] [P108](p108-universe-environments.md) — make environments and their
  credentials universe resources, replace session attachment/catalog state
  with one event-sourced active environment, add focused model discovery and
  selection tools, and remove generic default-target routing
- [ ] [P96](p96-environment-api.md) — environment API review: machines as universe resources vs session bindings, real presence leases, machine-keyed durable jobs, occupancy-checked teardown
- [x] Stop externally re-scoping the host-bridge filesystem by its `fsRoot`;
      ordinary environment file tools now pass environment-native absolute
      paths directly to the bridge, which enforces its own root boundary
- [ ] Finalize sandbox protocol (look at Codex's protocol)
- [ ] Write first sandbox integration
- [ ] Allow agent to request new sandbox/env
- [ ] [P86](p86-durable-environment-jobs.md) — durable environment jobs for long VM/sandbox work, including parallel jobs, serial lanes, dependency DAGs, and wait/cancel/read primitives
- [ ] run coding agent (CC or Codex) on sandbox wrappers

## External Channels Integration
- [x] [P88](p88-media-aware-context-append-and-activation.md) — media-aware
  `context/append`, context-triggered runs, and eager bridge ingest/activation
  for current supported media types
- [x] [P89](p89-room-context-retention.md) — room context retention and
  compaction: watermarked drop-oldest pruning via `context/remove`, then
  summarize-and-replace, so always-on group sessions stay bounded
- [x] Channel transport, account sessions, delivery state, media handling,
  authentication, and provider-specific behavior now live in the external
  Channels application; Lightspeed exposes only generic P100/P100b managed
  session and workflow-tool primitives

## Security Auth
- [ ] Provider OAuth login
- [ ] Send secrets to sandbox/VM/env
- [ ] Design capability based model for agents

## MCP
- [x] [P110](p110-universe-owned-mcp-auth.md) — make authentication part of
  each universe-scoped MCP server configuration, remove grant selection and
  grant references from sessions, and resolve the server's current credential
  immediately before provider I/O; edit the greenfield tables in place and
  reset infrastructure
- [x] [P99](p99-configurator-mcp.md) — multi-universe Configurator MCP over
  stateless Streamable HTTP, generated from a configurable subset of the
  universe-scoped TypeScript client contract with request-scoped gateway
  authentication and no operator methods
- [ ] Support MCP tunnels to model providers
- [ ] MCP orchestration by Lightspeed

## Framework/SDK
- [ ] External Temporal workflow SDK: authenticated access to P100's generic
  trusted managed-session creation capability (declare workflow tools +
  opaque receivers per binding; authorization at the control-plane boundary,
  no endpoint registry)
- [x] P100b promise-bearing (keyed promise set) and workflow-as-tool
  implementation (see [P100b](p100b-workflow-backed-tools.md)); complete
  2026-07-26 incl. live plugin-worker suite; discovery/catalog remains
  outside the session worker
- [x] [P90](p90-multi-tenancy.md) — multi-tenant worker: multiple universes
      per deployment, composed workflow ids, per-request universe resolution
      (`single` / `trusted-header` / `api-key` modes), principal pass-through,
      universe/api-key admin subcommands, per-binding bridge credentials
- [ ] Python SDK
     - [ ] API Client
     - [ ] Workflow helpers
- [ ] TypeScript SDK
     - [x] API Client
     - [ ] Workflow helpers

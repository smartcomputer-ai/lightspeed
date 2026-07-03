# Lightspeed Roadmap

## Core
- [ ] [P91](p91-core-agent-structure-cleanup.md) — cleanup of CoreAgent structures: delete the SDK-era open-kernel layer, commit to a closed event vocabulary and core FSM
- [ ] optimize: we're rading all session events to get latest state, this will get expensve in the future

## Fleet (sub-agents)
- [x] [P82](p82-session-graph-fork-clone.md) — session graph foundation: clone, fork (by-reference), and links in the store
- [x] [P83](p83-fleet-subagent-control-plane.md) — agent-facing Fleet control plane (spawn/task/read/list/cancel) on top of P82
- [x] [P84](p84-fleet-wait-and-callbacks.md) — first cut complete: `agent_send`, generic deferred tool batches, `RunSubscription` workflow primitives, `agent_wait` DTO/preflight/parking/resume, and live Mode I/Mode W coverage
- [x] [Appendix: Fleet one-off child lifecycle](appendix-fleet-one-off-lifecycle.md) — `agent_spawn.lifecycle.close_on_terminal` for ephemeral delegation sessions
- [x] agent profiles
- [x] start new sessions with profiles and ad-hoc profiles

## Provider Integrations
- [ ] support and test completions api
   - test with OAI
   - test with open router
   - test with self-hosted model
- [ ] incremental tool discovery support (at least OAI)

## Environmnets & Sandboxes
- [ ] Fix host-bridge fs routing doubled path: absolute guest paths get
      re-prefixed with the bridge root, so file-tool reads of shell-written
      absolute paths fail (`environment_provider_live` host-bridge agent
      test; pre-dates P90)
- [ ] Finalize sandbox protocol (look at Codex's protocol)
- [ ] Write first sandbox integration
- [ ] Allow agent to request new sandbox/env
- [ ] [P86](p86-durable-environment-jobs.md) — durable environment jobs for long VM/sandbox work, including parallel jobs, serial lanes, dependency DAGs, and wait/cancel/read primitives
- [ ] run coding agent (CC or Codex) on sandbox wrappers

## Message Bridge
- [x] [P88](p88-media-aware-context-append-and-activation.md) — media-aware
  `context/append`, context-triggered runs, and eager bridge ingest/activation
  for current supported media types
- [x] [P89](p89-room-context-retention.md) — room context retention and
  compaction: watermarked drop-oldest pruning via `context/remove`, then
  summarize-and-replace, so always-on group sessions stay bounded
- [x] Password/code-based login in channel (instead of whitelisting)
- [ ] Support Slack

## Security Auth
- [ ] Provider OAuth login
- [ ] Send secrets to sandbox/VM/env
- [ ] Design capability based model for agents

## MCP
- [ ] Support MCP tunnels to model providers

## Framework/SDK
- [ ] Temporal service design: ensure ls can be used as a Temporal service by other workflows
- [ ] Workflows as tools (register a workflow as a tool, route to workflow)
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

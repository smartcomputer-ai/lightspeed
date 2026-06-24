# Lightspeed Roadmap

## Core
- [ ] cleanup of CoreAgent structures (which were designed to expand the event types because we wanted Lightspeed to be a library)
- [ ] do we need to keep agent_id in session meta data (in tables)?
- [ ] optimize: we're rading all session events to get latest state, this will get expensve in the future

## Fleet (sub-agents)
- [x] [P82](p82-session-graph-fork-clone.md) — session graph foundation: clone, fork (by-reference), and links in the store
- [x] [P83](p83-fleet-subagent-control-plane.md) — agent-facing Fleet control plane (spawn/task/read/list/cancel) on top of P82
- [ ] [P84](p84-fleet-wait-and-callbacks.md) — implementation landed: `agent_send`, generic deferred tool batches, `RunSubscription` workflow primitives, and `agent_wait` DTO/preflight/parking/resume; ignored live/replay coverage for parked waits remains pending
- [ ] agent profiles
- [ ] start new sessions with profiles and ad-hoc profiles
- [ ] run coding agent (CC or Codex) on sandbox

## Provider Integrations
- [ ] support and test completions api
   - test with OAI
   - test with open router
   - test with self-hosted model
- [ ] incremental tool discovery support (at least OAI)

## Environmnets & Sandboxes
- [ ] Finalize sandbox protocol (look at Codex's protocol)
- [ ] Write first sandbox integration
- [ ] Allow agent to request new sandbox/env

## Message Bridge
- [ ] Password/code-based login in channel (instead of whitelisting)
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
- [ ] Multi-tenant support in worker
- [ ] Python SDK
     - [ ] API Client
     - [ ] Workflow helpers
- [ ] TypeScript SDK
     - [x] API Client
     - [ ] Workflow helpers

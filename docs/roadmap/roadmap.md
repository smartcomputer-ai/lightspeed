# Lightspeed Roadmap

## Core
- [ ] cleanup of CoreAgent structures (which were designed to expand the event types because we wanted Lightspeed to be a library)
- [ ] do we need to keep agent_id in session meta data (in tables)?

## Fleet (sub-agents)
- [ ] [P82](p82-session-graph-fork-clone.md) — session graph foundation: clone, fork (by-reference), and links in the store
- [ ] [P83](p83-fleet-subagent-control-plane.md) — agent-facing Fleet control plane (spawn/read/list/cancel) on top of P82
- [ ] add apis needed to control fleet (fix re-entrancy issue)
- [ ] decide on first design
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

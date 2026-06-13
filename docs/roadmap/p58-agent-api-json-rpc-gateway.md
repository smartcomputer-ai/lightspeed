# P58: Agent API JSON-RPC Gateway

**Status**
- Completed

**Progress**
- Added transport-neutral JSON-RPC dispatch helpers to `agent-api`.
- Added `claw-gateway`, an HTTP JSON-RPC gateway over `ClawAgentApi`.
- Added `--api-url` / `LIGHTSPEED_AGENT_API_URL` to the CLI chat command so the
  existing CLI can target a Temporal-backed gateway.

## Goal

Let clients use the same `agent-api` contract across local and Temporal-backed
agents through a process boundary.

The first transport is JSON-RPC over HTTP:

```text
POST /rpc
method: initialize | session/start | session/read | session/events/read | run/start
```

This keeps `agent-api` as the stable method/DTO contract while allowing:

- `agent-local` to remain in-process for local SDK use
- `claw-gateway` to expose Temporal/Pg-backed Claw sessions
- `cli` to switch between local and remote execution with `--api-url`

## Runtime Shape

```text
cli --api-url http://127.0.0.1:18080/rpc
  -> JSON-RPC HTTP
  -> claw-gateway
  -> ClawAgentApi
  -> Temporal ClawSessionWorkflow
  -> Pg session log + CAS
```

`claw-worker` and `claw-gateway` remain separate processes. The worker owns
Temporal workflow/activity execution. The gateway owns client transport and API
projection.

## Non-Goals

- Streaming notifications
- WebSocket transport
- SSE transport
- Auth/multi-tenant gateway policy
- HTTP REST resource design

## Verification

- `cargo test -p agent-api -p cli --tests -p claw`


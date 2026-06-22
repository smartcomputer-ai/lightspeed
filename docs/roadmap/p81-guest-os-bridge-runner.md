# P81: Guest OS Host Bridge Runner

**Status**
- Proposed 2026-06-17.
- G1-G7 implemented 2026-06-17.
- Builds on P75-P80 and `docs/spec/04-environments.md`.
- Breaking changes remain allowed. Lightspeed has not shipped a stable
  environment API.

**Progress**
- Added standalone workspace crate `crates/host-bridge` with binary
  `host-bridge`.
- Implemented config/env parsing, private advertised endpoint calculation,
  gateway provider registration, heartbeat, and best-effort unregister.
- Implemented a WebSocket `host-protocol` controller/data-plane server with
  attach-only target lifecycle.
- Implemented POSIX-first local process execution with stdout/stderr polling,
  stdin writes, termination, and timeout handling.
- Implemented rooted host filesystem operations with read/write, directory,
  metadata, remove, and copy support.
- Added focused process/filesystem tests plus a host-client protocol smoke test
  that initializes the controller, attaches the target, connects to the returned
  data plane, and executes a process.
- Added `lightspeed env` CLI helpers for list/read/attach/activate/deactivate/
  close so operators can bind a running bridge provider to a session without
  hand-writing JSON-RPC.
- Added ignored live test
  `temporal_live_host_bridge_agent_reads_local_filesystem`, which starts a
  Temporal worker, starts an HTTP gateway, spawns the real `host-bridge` binary,
  attaches/activates it, has the agent write a local file with `exec_command`,
  reads the same file through `read_file`, and verifies the file on the local
  filesystem.
- Verified with `cargo test -p host-bridge`, `cargo test -p cli --tests`, and
  the ignored host-bridge live test.

## Goal

Ship the first real environment provider: a standalone bridge runner binary that
can be started inside a guest OS to give Lightspeed access to that OS as an
attached execution environment.

The runner is not part of the Lightspeed CLI. It is a separate binary with a
separate operational lifecycle:

```text
host-bridge --gateway-url http://127.0.0.1:18080/rpc \
  --provider-id local-dev-mac \
  --listen 127.0.0.1:0 \
  --advertise-url ws://127.0.0.1:19090 \
  --cwd /Users/lukas/dev/lightspeed
```

The network assumption is private reachability, not public internet exposure.
The bridge may run on another machine or inside a guest OS, but the Temporal
server/gateway/worker deployment must be able to reach the bridge's advertised
`host-protocol` WebSocket endpoint through deployment plumbing such as a LAN,
VPN, Tailscale tailnet, port forward, or local development loopback. Provider
registration does not create a tunnel; it only tells Lightspeed where the
controller is.

After startup it:

1. serves a `host-protocol` controller/data-plane WebSocket endpoint;
2. registers itself with the gateway through `environmentProviders/register`;
3. heartbeats while reachable;
4. advertises one attached-host target representing the OS it is running in;
5. allows a session to attach that target and execute process/file operations
   through the existing `env:<id>` path.

This is the concrete successor to the fake provider live test in P80.

## Product Boundary

The bridge runner is a guest-side access agent, not the user-facing CLI and not a
Temporal worker.

It may live in this repository as a workspace crate, but it must remain
deployable as one standalone binary:

```text
crates/host-bridge/
  src/main.rs
```

Working binary name: `host-bridge`.

The runner may depend on `api`, `host-protocol`, and small transport helpers. It
must not depend on `cli`, `temporal-server`, `store-pg`, `engine`, or any
workflow/runtime crate. The dependency direction should be:

```text
host-bridge -> api
host-bridge -> host-protocol
host-bridge -> transport/json-rpc helper code
```

If the existing JSON-RPC/WebSocket mechanics are too client-oriented, extract a
small shared helper crate instead of making the runner depend on
`temporal-server`.

## Runner Model

One bridge process registers one environment provider.

One provider initially advertises one target:

```text
provider_id   configured or generated bridge provider id
target_id     configured target id, default "local"
kind          bridge / attached_host
scope         default
status        ready while the runner is online
cwd           configured cwd, default process cwd
capabilities  fs read/write, process start/stdin/terminate/output polling
```

The runner does not provision anything. It represents the OS where it is already
running. Therefore it should support:

- `controller/listTargets`
- `controller/attachTarget`
- `controller/getTarget`
- `controller/closeTarget` as a detach/no-op transition

It should not advertise `controller/createTarget` in the first implementation.
Sandbox creation belongs to a sandbox provider, not the bridge runner.

## Registration Flow

Startup sequence:

1. Load config and compute the advertised controller endpoint.
2. Start the WebSocket server.
3. Call `environmentProviders/register` with:
   - `providerKind = bridge`
   - `controllerConnection.transport = webSocket`
   - `controllerConnection.endpoint = {advertise-url}/control`
   - capabilities from the controller handshake
4. Start a heartbeat loop using `environmentProviders/heartbeat`.
5. Include the current target summary in heartbeat payloads so the gateway does
   not have to poll `controller/listTargets` on every heartbeat.

Attach sequence:

```text
session/environments/attach
  provider_id = "local-dev-mac"
  request = { type: "target", targetId: "local" }
  env_id = "local-dev"
  activate = true
```

Gateway calls `controller/attachTarget`; the runner returns a
`HostConnectionSpec` with a data-plane endpoint such as:

```text
ws://127.0.0.1:19090/data?target=local
```

For non-local deployments this endpoint should be a private routable address,
for example a Tailscale DNS name or tailnet IP. It must be reachable from both
gateway controller calls and worker data-plane calls.

The worker already knows how to connect to WebSocket `HostConnectionSpec`
records, handshake, and build `RemoteHostConnection`.

## Data Plane

The runner implements the data-plane methods that `RemoteHostConnection` already
calls.

### Process Methods

Implement:

- `data/initialize`
- `data/initialized`
- `process/start`
- `process/read`
- `process/write`
- `process/terminate`

Mapping:

- `process/start` spawns `tokio::process::Command` with the requested `argv`,
  `cwd`, environment overrides, optional initial stdin, and optional timeout.
- `process/read` returns buffered stdout/stderr chunks in increasing `seq`
  order, respects `afterSeq`, `maxBytes`, and `waitMs`, and includes exit status
  once known.
- `process/write` writes to the child's stdin when `pipeStdin` was requested.
- `process/terminate` kills the child process and returns whether it was still
  running.

Initial limitations:

- `tty=false` only.
- No PTY resize support.
- No output notifications; use polling first.

The runner should advertise only the capabilities it actually supports.

### Filesystem Methods

Implement the host filesystem methods needed by `RemoteHostFileSystem`:

- `fs/readFile`
- `fs/writeFile`
- `fs/createDirectory`
- `fs/getMetadata`
- `fs/readDirectory`
- `fs/remove`
- `fs/copy`

By default, file methods are rooted at `--fs-root` if set, otherwise at `--cwd`.
Paths outside the allowed root should fail with a typed host-protocol error.
This is an accidental-write/read guard, not a sandbox: commands executed through
`process/start` still run with the OS permissions of the bridge process.

Advertise `filesystemRead` / `filesystemWrite` only when the filesystem server is
enabled. When enabled, the gateway will project the cwd route with
`same_state_as_active_env = Some(env_id)`, which is the right story for this
bridge: file-tool edits and shell edits touch the same guest filesystem.

## Security Posture

This runner grants Lightspeed access to the OS account running the binary. It is
not isolation.

Required P81 defaults:

- bind to loopback by default;
- require an explicit `--advertise-url` when the gateway cannot reach the bind
  address directly;
- require a provider registration token when the gateway is not configured for a
  local trust channel;
- clearly log the provider id, target id, advertised endpoint, cwd, and fs root;
- never default to a public listen address.

Host-protocol connection authentication is intentionally not solved in P81
unless the implementation can do it without leaking secrets into session/runtime
records. For P81, direct WebSocket endpoints should be treated as local/private
deployment plumbing. Tailscale/private-network reachability is in scope; public
internet exposure is a non-goal.

## Configuration

Minimum flags/env:

```text
--gateway-url / LIGHTSPEED_GATEWAY_URL
--provider-id / LIGHTSPEED_HOST_BRIDGE_PROVIDER_ID
--provider-token / LIGHTSPEED_PROVIDER_TOKEN
--target-id / LIGHTSPEED_HOST_BRIDGE_TARGET_ID         default "local"
--listen / LIGHTSPEED_HOST_BRIDGE_LISTEN               default 127.0.0.1:0
--advertise-url / LIGHTSPEED_HOST_BRIDGE_ADVERTISE_URL
--cwd / LIGHTSPEED_HOST_BRIDGE_CWD                     default current dir
--fs-root / LIGHTSPEED_HOST_BRIDGE_FS_ROOT             default cwd
--heartbeat-interval-ms                                default 10_000
--lease-ttl-ms                                         default 30_000
--read-only-fs                                         default false
```

If `--provider-id` is omitted, the runner may generate an ephemeral provider id
and print it. Daemon/service deployments should configure a stable provider id
so operators and attach commands can target it predictably.

When the runner is reached over Tailscale or another private overlay network,
`--listen` should bind an address on that network and `--advertise-url` should be
the address the Temporal server can dial, for example
`ws://devbox.tailnet-name.ts.net:19090`.

## Guest OS Support

Start POSIX-first.

The current `HostPath` model is normalized around `/` paths, and the agent-facing
environment model assumes POSIX-like paths in examples. Linux and macOS are the
first target. Windows support should be explicit later rather than accidentally
wrong, because path normalization, drive letters, executable lookup, and process
termination semantics differ.

## Implementation Plan

### G1: Binary Crate And Config

Add `crates/host-bridge` with a standalone binary named `host-bridge`.

Implement config loading, flag/env precedence, startup logging, graceful shutdown,
and a small JSON-RPC client for the gateway provider APIs.

### G2: Host-Protocol WebSocket Server

Serve controller and data-plane JSON-RPC over WebSocket.

The first server can use two paths on one listener:

- `/control`
- `/data`

Keep request dispatch typed at the boundary: parse JSON-RPC, match method names,
deserialize into `host-protocol` DTOs, call local handlers, serialize typed
responses.

### G3: Registration And Heartbeat

Register the provider on startup, verify the gateway accepts the controller
handshake, then heartbeat until shutdown.

On graceful shutdown call `environmentProviders/unregister`. On crash, rely on
lease expiry.

### G4: Attached-Host Controller

Implement a one-target controller:

- `initialize` returns bridge implementation info and attach/list/get/close
  capabilities;
- `listTargets` returns the local target summary;
- `attachTarget(Target { targetId })` returns a data-plane `HostConnectionSpec`;
- `getTarget` returns the current summary;
- `closeTarget` returns `closed` for the request but does not stop the bridge
  process.

Prefer treating close as "detach this session binding" rather than shutting down
the bridge process. The process remains online until the operator stops it.

### G5: Local Process Data Plane

Implement the process server with `tokio::process`.

Add focused tests for:

- stdout/stderr chunk ordering;
- exit-code reporting;
- stdin write/close;
- timeout/terminate behavior;
- unknown process id behavior.

### G6: Local Filesystem Data Plane

Implement rooted filesystem operations and map local I/O errors to
host-protocol error codes.

Add focused tests for:

- root escape rejection;
- read/write round trip;
- directory listing;
- metadata mapping;
- recursive directory create/remove;
- copy behavior.

### G7: Live End-To-End Test

Replace the P80 in-process fake-provider-only confidence with a real spawned
bridge binary in an ignored live test.

Test shape:

1. start local Temporal/Postgres stack;
2. spawn `host-bridge` with a temp cwd/fs root;
3. wait for provider registration/heartbeat;
4. start a session;
5. attach target `local`, activate it;
6. run a process tool call that writes a file;
7. read that same file through fs tools;
8. assert the final assistant answer contains the marker read from the file;
9. close the session environment without stopping the bridge;
10. stop the bridge process during test cleanup.

This proves the real bridge has the "same guest filesystem" property that P80's
fake provider only simulated.

## Non-Goals

- No sandbox creation.
- No container/VM lifecycle.
- No reverse tunnel. Private network and VPN reachability are allowed, but the
  bridge does not establish a relay itself in P81.
- No public internet host-protocol authentication.
- No PTY/computer-use implementation.
- No Windows support.
- No multi-target provider inventory.
- No VFS workspace sync/fusion beyond exposing the guest filesystem as an
  environment filesystem route.
- No bridge runner implementation inside the Lightspeed CLI binary. The CLI may
  contain small API helpers for session environment lifecycle.

## Done When

- A standalone `host-bridge` binary can run outside `temporal-server` and
  outside the Lightspeed CLI.
- The runner registers and heartbeats as a bridge provider.
- The gateway can attach its `local` target to a session and activate it.
- Process tools execute against the OS account running the bridge.
- File tools can read/write the configured guest filesystem root and observe the
  same state as shell commands in the active environment.
- Close/detach does not kill the bridge daemon.
- Focused process/fs tests pass.
- An ignored live test spawns the real bridge binary and completes a
  write/edit/run round trip through the gateway.

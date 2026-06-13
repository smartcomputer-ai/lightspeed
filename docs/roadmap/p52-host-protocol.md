# P52: Host Protocol

**Status**
- Draft

**Progress**
- Added the `host-protocol` workspace crate for pure protocol data types.
- Implemented G1 data-plane types: shared ids, protocol version, `HostScope`,
  `HostCapabilities`, `ByteChunk`, `HostPath`, common error payloads,
  handshake records, filesystem method records, process method records, and
  process notifications.
- Added data-plane serde fixtures and round-trip tests.
- Implemented G2 controller-plane types: controller handshake, lifecycle method
  constants, extensible create/attach requests, target summaries/statuses, and
  the shared `HostTransport`/`HostConnectionSpec` handoff.
- Added controller-plane serde fixtures and round-trip tests.
- Added the `host-client` workspace crate with a small JSON-RPC core, WebSocket
  transport, typed data-plane methods, typed controller-plane methods, and
  mock-transport tests.
- Integrated the data plane into `agent-tools` with remote filesystem/process
  adapters, a remote `HostToolContext` helper, and tests covering existing file
  tools through a protocol-backed context.

## Goal

Define the substrate protocol used to talk to a host execution target.

P52 should get the protocol boundary right before we build rich sandbox
provisioning, host inventory, SSH attachment, or hosted controller services.
The first implementation should be enough to back Forge's existing host
filesystem and process tools through a remote target, and to obtain the first
externally hosted target through a minimal controller protocol.

The crate names should be neutral:

- `host-protocol`: pure protocol types, method names, serde fixtures, and
  compatibility rules for both the host data plane and the minimal controller
  plane
- `host-client`: one reusable client crate for both data-plane and controller
  calls, even if a consumer only uses one side
- `host-bridge`: later optional server/binary implementation of the protocol
- `sandbox-controller`: later production service implementation of the
  controller protocol

P52 focuses on `host-protocol`, one `host-client`, the minimal controller
protocol needed for the first externally hosted host, and the adapter needed to
use data-plane host connections from `agent-tools`.

## Design Position

Do not make "host bridge" mean "a binary inside the sandbox".

The required concept is the protocol. A concrete provider may implement it in
several ways:

- an in-guest daemon inside a VM/container
- a host-side daemon that controls VMs externally
- a provider API client that never starts a Forge binary in the guest
- an SSH adapter
- a virtual filesystem adapter with no process capability

Forge host tools should continue to depend on capabilities:

```rust
pub struct HostToolContext {
    pub fs: Arc<dyn FileSystem>,
    pub process: Option<Arc<dyn ProcessExecutor>>,
    // blobs, limits, cwd...
}
```

The protocol and client are one way to supply those capabilities.

`agent-core` should not change for P52. P51 already records the semantic
`ToolExecutionTarget` on tool effects. Runtime/tool execution maps
`host:<id>` to a concrete `HostToolContext`; the context may be local or backed
by the host protocol.

## Prior Art

Codex has the closest data-plane protocol:

- `/Users/lukas/dev/tmp/codex/codex-rs/exec-server/src/protocol.rs`
- `/Users/lukas/dev/tmp/codex/codex-rs/exec-server/src/client.rs`
- `/Users/lukas/dev/tmp/codex/codex-rs/exec-server/src/environment.rs`
- `/Users/lukas/dev/tmp/codex/codex-rs/exec-server/src/remote_file_system.rs`
- `/Users/lukas/dev/tmp/codex/codex-rs/exec-server/src/remote_process.rs`

The pieces worth taking are JSON-RPC transport, initialize/initialized
handshake, client-chosen process handles, retained process output reads,
streamed output notifications, base64 byte chunks, and remote fs/process
adapters hidden behind local capability traits.

Concretely, `host-protocol` should borrow these Codex protocol parts:

- method families: `initialize`, `initialized`, `process/*`, and `fs/*`
- a typed byte wrapper serialized as base64
- client-chosen logical process handles instead of OS pids
- `process/start` returning once the process is registered
- `process/read` as the source of truth for retained output
- output notifications as an optimization, not the only delivery path
- monotonic output sequence numbers for read-after-notification recovery
- stdin write and process termination requests
- filesystem methods that map directly to a provider-neutral filesystem trait
- typed JSON-RPC errors that can be mapped to capability errors
- remote client adapters that implement local fs/process traits

Do not copy Codex-specific thread/app-server concepts into P52:

- thread/turn/session API shape
- user approval and guardian review protocol
- Codex sandbox policy and permission profile types
- model-facing `command/exec` details
- plugin/MCP/app-server protocol concerns
- environment defaulting through `CODEX_EXEC_SERVER_URL`

AOS Fabric has the more relevant host/session control-plane vocabulary:

- `/Users/lukas/dev/aos/crates/fabric-protocol/src/lib.rs`
- `/Users/lukas/dev/aos/crates/fabric-host/src/runtime.rs`
- `/Users/lukas/dev/aos/crates/fabric-host/src/service.rs`
- `/Users/lukas/dev/aos/crates/fabric-host/src/smolvm.rs`

The Fabric/smolvm shape is important because it shows the protocol must not
require an in-guest binary. A host daemon can own workspaces, inventory,
session lifecycle, and exec routing from outside the VM.

## P52 Scope

Implement the host data plane:

- protocol handshake and capability discovery
- filesystem operations needed by `agent-tools::host::fs::FileSystem`
- process operations needed by `agent-tools::host::process::ProcessExecutor`
- protocol errors and serde fixtures

Implement the minimal controller plane needed to create or attach one externally
hosted target:

- controller handshake and capability discovery
- list known host targets
- create a host target from an extensible request
- attach to an existing host target when the provider supports it
- get target status
- close/release a target
- return a `HostConnectionSpec` that the data-plane client can connect with

Leave these out of P52:

- rich sandbox inventory and scheduling
- connected in-OS bridge daemon inventory beyond provider metadata
- full SSH bootstrap UX
- detailed image/runtime-class/resource scheduling
- long-lived host leases
- auth policy design beyond client transport credentials
- MCP surface
- high-level agent tools such as grep, glob, edit, or apply_patch

The existing Forge host tools already implement grep, glob, edit, and
apply_patch over `FileSystem`. The protocol should expose the substrate, not
duplicate every agent-facing tool.

## `host-protocol` Crate Shape

`host-protocol` should be a pure types crate. It should not open sockets, own
retry policy, run commands, access filesystems, or depend on Forge agent crates.

Target shape:

```text
crates/host-protocol/
  Cargo.toml
  src/lib.rs
  src/shared.rs
  src/error.rs
  src/data/mod.rs
  src/data/methods.rs
  src/data/handshake.rs
  src/data/fs.rs
  src/data/process.rs
  src/data/events.rs
  src/control/mod.rs
  src/control/methods.rs
  src/control/handshake.rs
  src/control/targets.rs
  tests/serde.rs
  fixtures/
```

`shared` owns types used by both protocol planes:

- protocol version
- ids such as `HostTargetId`, `HostConnectionId`, and `ProcessId`
- `HostScope`
- `HostTransport`
- `HostCapabilities`
- `HostConnectionSpec`
- implementation metadata
- `ByteChunk`
- common error code/type shapes

`lib.rs` should expose the module boundaries directly:

```rust
pub mod control;
pub mod data;
pub mod error;
pub mod shared;
```

Prefer plane-scoped imports such as `host_protocol::data::fs::ReadFileParams`
or `host_protocol::control::targets::CreateTargetParams`. Keep top-level
re-exports limited to stable shared primitives if they materially improve
ergonomics.

`data` owns the selected-target capability protocol:

- data-plane handshake params/results
- filesystem method constants and params/results
- process method constants and params/results
- process output and lifecycle notifications

`control` owns target lifecycle and connection discovery:

- controller handshake params/results
- target list/create/attach/get/close method constants
- target specs and summaries
- target status values
- create/attach responses that return `HostConnectionSpec`

Keep JSON-RPC transport envelopes out of `host-protocol` unless a typed
request/response/error envelope is needed for fixtures. The client crate can
own transport machinery while reusing method constants and params/results from
`host-protocol`.

## Protocol Shape

Use JSON-RPC 2.0 over WebSocket for the first transport. Keep the protocol
types transport-neutral enough that stdio or HTTP can be added later.

Authentication should be transport metadata, for example bearer token or mTLS
configuration in `host-client` connect options. Do not put credentials in
normal method params.

A connection should be bound to one execution scope during initialization:

```rust
pub struct InitializeParams {
    pub protocol_version: u32,
    pub client_name: String,
    pub scope: HostScope,
    pub resume_connection_id: Option<String>,
}

pub enum HostScope {
    Default,
    Session { session_id: String },
}
```

The exact names can change during implementation, but the invariant should
hold: after initialization, filesystem and process calls run against the
selected host scope. This lets one implementation expose a session-specific
endpoint, while another exposes a multi-session host daemon and selects the VM
session from `HostScope`.

Initialization should return:

- accepted protocol version
- connection/session id usable for short reconnects when supported
- capability flags
- optional default cwd
- implementation metadata suitable for diagnostics

Capabilities should be explicit:

- filesystem read
- filesystem write
- process start
- process stdin
- process terminate
- process output polling
- process output notifications
- PTY support, if implemented
- HTTP/network proxy support, later

## Controller Shape

The controller plane should live in `host-protocol` beside the data plane, but
under a separate module namespace. The controller is how an authenticated SDK
user or runtime gets from "I need a host" to a concrete data-plane connection.

Keep the first controller protocol intentionally small and extensible:

```text
controller/initialize
controller/listTargets
controller/createTarget
controller/attachTarget
controller/getTarget
controller/closeTarget
```

Target creation should use a tagged request so providers can support different
backends without changing the top-level protocol:

```rust
pub enum HostTargetCreateRequest {
    Sandbox(SandboxTargetSpec),
    AttachedHost(AttachedHostSpec),
    Provider {
        provider_type: String,
        spec: serde_json::Value,
    },
}
```

The first cut can implement only the provider shape or only one concrete shape.
The important part is the response:

```rust
pub struct HostConnectionSpec {
    pub target_id: String,
    pub endpoint: String,
    pub transport: HostTransport,
    pub scope: HostScope,
    pub default_cwd: Option<String>,
    pub capabilities: HostCapabilities,
}
```

`target_id` becomes the id in `host:<target_id>`. The runtime uses the
connection spec to create a `host-client` data-plane connection, builds a
`HostToolContext`, and registers it in `HostToolTargets`.

Controller metadata should be allowed to say whether the target is backed by an
in-guest bridge, a host-side daemon, SSH, a VM API, or a virtual filesystem, but
normal host tool execution should not care.

## Filesystem Methods

P52 should cover the current `FileSystem` trait:

```text
fs/readFile
fs/writeFile
fs/createDirectory
fs/getMetadata
fs/readDirectory
fs/remove
fs/copy
```

Paths should be serialized as Forge logical filesystem paths, using the same
slash-normalized semantics as `FsPath`. The remote provider decides how those
logical paths map to its backing storage. A local path on the runtime host must
not leak into the protocol unless that path is intentionally part of the target
filesystem namespace.

Bytes should be base64 encoded in protocol JSON. Use a transparent byte wrapper
like Codex's `ByteChunk` rather than ad hoc string fields everywhere.

## Process Methods

P52 should cover the current `ProcessExecutor` trait and leave room for richer
interactive execution:

```text
process/start
process/read
process/write
process/terminate
process/resize
process/output
process/exited
process/closed
```

The first adapter can implement Forge's `run_process` by starting a process and
reading until exit, timeout, or `yield_time_ms`. `write_process_stdin` maps to
`process/write` plus a follow-up read.

Process ids are protocol handles chosen by the client. They are not OS pids.
Output chunks should carry monotonic sequence numbers so clients can recover
from missed notifications by polling `process/read`.

PTY support can be part of the protocol shape but does not need to be exposed
through the first Forge `ProcessExecutor` adapter unless the host tool surface
adds PTY parameters.

## Client Shape

`host-client` should be one reusable client crate:

- depends on `host-protocol`
- owns JSON-RPC request/response plumbing
- owns WebSocket connection handling
- supports optional auth headers/tokens
- exposes typed async methods for fs/process data-plane calls
- exposes typed async methods for minimal controller calls
- does not depend on `agent-core` or `agent-tools`

It is acceptable for consumers to use only one side of the client. Keeping one
crate avoids splitting shared transport, auth, error mapping, version
negotiation, and connection-spec handling too early.

The Forge-specific adapter should live in `agent-tools`:

- `RemoteHostFileSystem` implements `FileSystem` over `host-client`
- `RemoteProcessExecutor` implements `ProcessExecutor` over `host-client`
- a helper builds a `HostToolContext` from a remote connection and blob store
- `HostToolTargets` can then map `host:<id>` to that context

This keeps the reusable protocol client separate from Forge's current host tool
trait names, while still making integration straightforward.

## Relationship to P51

P51 answers "which host target should this tool call use?"

P52 answers "once a runtime selected a live host target, how do filesystem and
process capabilities talk to it?"

The minimal controller protocol answers "how does a runtime obtain a live host
target and a data-plane connection spec?"

The `ToolExecutionTarget` id is not necessarily sent on every protocol request.
The runtime maps `host:<id>` to a configured or provisioned connection:

```text
host:sandbox_123
  -> endpoint: wss://host.example/v1/host-protocol
  -> scope: Session { session_id: "sandbox_123" }
  -> cwd: /workspace
  -> credentials: runtime-owned
```

After that mapping, normal host tools invoke `FileSystem` and
`ProcessExecutor`; they should not know whether the target is local, Codex-like
remote, Fabric/smolvm, SSH-backed, or virtual.

## MCP Position

Do not implement the host data plane as MCP.

MCP is a reasonable later facade for agent-visible management tools:

- `sandbox.list`
- `sandbox.create`
- `sandbox.select`
- `sandbox.close`
- `host.list`
- `host.status`

The fs/process substrate needs lower-level behavior: binary chunks, retained
output reads, notifications, stdin, termination, reconnects, and precise error
classification. That belongs in `host-protocol`. MCP tools can call a
controller or SDK layer later.

## Implementation Order

### [x] G1. Add Data-Plane `host-protocol` Types

- Add a workspace crate for pure protocol types.
- Define shared ids, protocol version, `HostScope`, capability types, common
  errors, and `ByteChunk`.
- Define data-plane method constants and handshake types.
- Define filesystem params/results.
- Define process params/results and output/lifecycle notification types.
- Add serde round-trip tests and JSON fixtures for the data plane.
- Keep the crate independent from `agent-core`, `agent-local`, and
  `agent-tools`.

### [x] G2. Add Controller-Plane `host-protocol` Types

- Add controller method constants and handshake types.
- Define extensible target specs for create/attach.
- Define target summaries, status values, and close/release results.
- Define `HostTransport` and `HostConnectionSpec` as the handoff from
  controller plane to data plane.
- Add serde round-trip tests and JSON fixtures for controller requests and
  responses.
- Keep the first controller plane minimal enough to create or attach one
  externally hosted target.

### [x] G3. Add `host-client`

- Add one reusable client crate for data-plane and controller calls.
- Implement JSON-RPC request/response handling.
- Implement WebSocket transport first.
- Support data-plane initialize/initialized, typed fs methods, typed process
  methods, and output notification routing.
- Support minimal controller methods and returning `HostConnectionSpec`.
- Add tests against an in-memory or mock transport.

### [x] G4. Integrate Data Plane With `agent-tools`

- Add remote filesystem and process adapters in `agent-tools`.
- Build `HostToolContext` from the remote adapters and an existing `BlobStore`.
- Register the context through `HostToolTargets`.
- Add a local-loop style test proving existing host tools work through the
  protocol-backed context.

### [ ] G5. Add Minimal Externally Hosted Flow

- Use `host-client` controller calls to obtain a `HostConnectionSpec`.
- Use the returned spec to create the data-plane client.
- Register the data-plane backed `HostToolContext` under the returned
  `host:<target_id>`.
- Start a session with that target as the default host target.
- Add an integration-style test with a fake controller/data-plane service.

### [ ] G6. Document Provider Implementations

- Document how a Codex-style exec server maps to `host-protocol`.
- Document how Fabric/smolvm maps to `host-protocol` without an in-guest Forge
  binary.
- Document how future control-plane provisioning returns a connection spec that
  can become a `host:<id>` target.

## Later Work

- `host-bridge` reference server
- `sandbox-controller` production service for authenticated host/sandbox
  listing and provisioning
- SSH bootstrap/tunnel helper
- HTTP/network-origin capability
- PTY-first process tools
- durable host leases and reconnect policy
- multi-target agent-visible host management tools

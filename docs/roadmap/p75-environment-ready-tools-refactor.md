# P75: Environment-Ready Tools Refactor

**Status**
- Proposed 2026-06-17.
- G1-G3 implemented 2026-06-17.
- G4-G6 implemented 2026-06-17.
- G7 implemented 2026-06-17, including the API/config rename from
  `HostToolMode` / `tools.host` to `FilesystemToolMode` / `tools.filesystem`.
- Breaking changes are allowed. Lightspeed has not shipped a stable tool/runtime
  compatibility boundary.
- Preparatory work for `docs/spec/04-environments.md`.

**Progress**
- Added canonical target namespace constants for `fs` and `env`, with
  `fs:session` as the default filesystem target and `env:local` as the local
  environment compatibility target.
- Changed built-in file tool specs to require `fs`; process/stdin tools now
  require `env`.
- Added runtime namespace validation so file tools cannot be invoked against an
  environment target and process tools cannot be invoked against the session
  filesystem target.
- Added a top-level `tools::fs` module as the canonical filesystem API.
- Physically moved filesystem implementations, VFS filesystem code, apply-patch
  internals, and file-tool operation modules under `tools::fs`.
- Added `FsToolContext` and moved generic file tool implementations to depend on
  that context instead of the former combined host tool context.
- Updated hosted VFS/session tool setup and affected tests to set/wait for
  `fs:session` rather than `host:local`.
- Added `EnvironmentToolContext`; process/stdin tools now consume environment
  context, while file tools consume filesystem context.
- Moved process execution traits/adapters and process/stdin tool operations
  under `tools::environment`.
- Moved runtime target resolution to top-level `ToolTargets`, with `fs` and
  `env` maps stored separately.
- Added `SessionFileSystem`, a generic deepest-prefix router over arbitrary
  `FileSystem` backends, with route metadata for future VFS/environment
  projection.
- Updated VFS-only runtime composition to build from `FsToolContext` and
  `fs:session`, without manufacturing a process-capable host context.
- Changed built-in tool catalog logical IDs and eval tool IDs from `host.*` to
  `fs.*` / `env.*`; legacy `host.*` IDs remain accepted as aliases.
- Moved built-in tool definitions under `tools::builtin` and inline execution
  under `tools::runtime::InlineToolRuntime`.
- Renamed the remaining host-protocol adapter module to `tools::host_protocol`
  (`RemoteHostConnection`, `RemoteHostFileSystem`, `RemoteProcessExecutor`).

## Goal

Refactor the tools package so the code shape matches the environment model:

- filesystem tools operate on a generic session filesystem;
- VFS is one filesystem implementation, not a host;
- environment tools are actions against a real VM/OS/sandbox/attached host;
- the low-level host protocol remains the implementation mechanism for
  environment-backed capabilities.

The immediate goal is not to implement full environment activation. The goal is
to remove the coupling where one combined host tool context meant both "the
filesystem used by file tools" and "the process namespace used by exec". That
coupling is what made a VFS-only session look like `host:local` even though it
had no shell.

## Problem

`crates/tools/src/fs/mod.rs` has the right core abstraction:

```rust
pub trait FileSystem: Send + Sync { ... }
```

`LocalFileSystem`, `ScopedFileSystem`, `ReadOnlyFileSystem`,
`MountedVfsFileSystem`, `VfsSnapshotFileSystem`, and `VfsWorkspaceFileSystem`
are implementations of that trait. Before this refactor, the file tool
implementations mostly operated through the old combined context's filesystem;
the important
property is that they can be shared across local filesystems, VFS, and future
environment-backed filesystems.

The problem is the surrounding ownership model:

```rust
pub struct HostToolContext {
    pub fs: Arc<dyn FileSystem>,
    pub process: Option<Arc<dyn ProcessExecutor>>,
    pub blobs: Arc<dyn BlobStore>,
    pub limits: HostToolLimits,
    pub cwd: Option<FsPath>,
}
```

This encodes the guest-OS assumption that one target has both a filesystem and a
process namespace. In the environment model that is no longer true:

- the session filesystem is a fused file-tool view over VFS routes and exposed
  environment filesystem routes;
- the shell/process/computer-use layer belongs to one active environment;
- VFS has no process model and should not be represented as a host target.

Before this refactor, all built-in filesystem and process tools also shared one
target namespace:

```text
read_file / write_file / grep / glob / run_process -> host:<id>
```

That makes it impossible to say "file tools go to the session filesystem, but
`run_process` goes to the active environment" without overloading `host` even
more.

## Decision

Make filesystem tooling a first-class generic tool package, and reserve host /
environment tooling for actions performed against real environments.

Use this conceptual split:

```text
fs:session
  The session fused filesystem service.
  File tools target this.

env:<env_id>
  A concrete execution/action environment.
  Process, pty, stdin, computer-use, and future environment-native actions target this.
```

The agent should not choose whether a file path is VFS-backed or environment-
backed. It calls file tools against `fs:session`, and the session filesystem
routes paths internally:

```text
/skills      -> VFS snapshot, read-only
/prompts     -> VFS snapshot, read-only
/workspace   -> VFS workspace or environment-backed workspace
/repo         -> active environment filesystem route
```

The agent does choose actions that require an environment, such as running a
process or controlling a computer-use surface. Those route to `env:<env_id>`,
normally through the active environment default.

## Naming

Use the words consistently:

- **filesystem**: generic `FileSystem` trait plus path-oriented operations.
- **VFS**: CAS-backed implementation of `FileSystem`.
- **session filesystem**: fused router over VFS and environment filesystem
  routes.
- **environment**: model/product concept: sandbox, VM, attached host, devbox, or
  future action surface with capabilities.
- **host protocol**: low-level transport/control implementation used to reach
  environment-backed capabilities.
- **built-in tools**: Lightspeed-provided fs/env/web/messaging tool
  definitions and inline runtime bindings.

`host` should not be the model-facing target namespace for new environment
work. It is already worn out because it refers both to the protocol/backend and
to the old combined filesystem/process target.

## Target Model

Change built-in tool target requirements so filesystem and environment actions
can be routed independently.

Expected first shape:

```text
read_file
write_file
edit_file
apply_patch
grep
glob
list_dir
  target_requirement: Required { namespace: "fs" }
  default target: fs:session

run_process
write_process_stdin
future pty/computer-use tools
  target_requirement: Required { namespace: "env" }
  default target: env:<active-env-id>
```

This is a breaking change from P51's first cut, where host filesystem and process
tools all used `Required { namespace: "host" }`.

Keep the deterministic rule from P51: core resolves the default target before
execution and stamps `execution_target` onto each tool call. Runtime code then
resolves that target to the appropriate live context.

`fs:session` is deliberately one opaque deterministic target. Core does not
parse file-tool JSON and therefore does not stamp the final route backend
(`VfsWorkspace`, `EnvironmentFilesystem`, etc.) onto the call before execution.
The session filesystem router resolves the path inside `fs:session` at execution
time and records the route/effects post-hoc through tool output, `ToolEffect`s,
and environment projection metadata.

## Filesystem Package

Extract a generic filesystem tool package out of the old combined host tool
package.

Target shape:

```text
crates/tools/src/fs/
  mod.rs                 FileSystem trait, FsPath, errors, adapters
  tools/
    read_file.rs
    write_file.rs
    edit_file.rs
    apply_patch.rs
    grep.rs
    glob.rs
    list_dir.rs
  vfs/
    mounted.rs
    snapshot.rs
    workspace.rs
```

The exact module names can differ, but the dependency direction should be:

```text
generic file tool implementations -> FileSystem trait
VFS implementation                 -> FileSystem trait + VFS stores/CAS
environment filesystem adapter     -> FileSystem trait + host protocol client
environment action tools           -> environment/action context
```

Generic file tools must not depend on process execution, environment lifecycle,
host target resolution, or host connection specs.

## Shared File Tool Implementations

Do not fork `read_file`, `grep`, `glob`, `edit_file`, or `apply_patch` for VFS.
Those implementations should remain shared and operate on `Arc<dyn FileSystem>`.

VFS-specific behavior belongs behind the trait:

- mount resolution;
- snapshot/workspace dispatch;
- CAS reads and writes;
- workspace commit compare-and-set;
- synthetic mount directories;
- VFS-specific `ToolEffect`s.

Environment filesystem behavior also belongs behind the trait:

- remote path normalization;
- host-protocol filesystem calls;
- capability and permission errors;
- optional scoped route prefixes.

The file tools should only know how to parse tool arguments, resolve relative
paths against a filesystem cwd, call `FileSystem`, and format model-visible
output.

## Runtime Contexts

Split the current `HostToolContext` responsibilities.

Proposed minimal contexts:

```rust
pub struct FsToolContext {
    pub fs: Arc<dyn FileSystem>,
    pub blobs: Arc<dyn BlobStore>,
    pub limits: ToolLimits,
    pub fs_cwd: Option<FsPath>,
}

pub struct EnvironmentToolContext {
    pub process: Option<Arc<dyn ProcessExecutor>>,
    pub blobs: Arc<dyn BlobStore>,
    pub limits: ToolLimits,
    pub process_cwd: Option<FsPath>,
}
```

This is illustrative, not a required final API. The important part is that file
tools do not require an environment context, and environment actions do not
pretend the session filesystem is the environment's native filesystem.

The two cwd concepts are distinct. `fs_cwd` is the base for relative paths in
the session filesystem. `process_cwd` is the default shell/process working
directory in the active environment.

An environment may also expose a filesystem adapter implementing `FileSystem`.
That adapter is mounted into the session filesystem as a route. It is still used
through `fs:session`, not by targeting `env:<id>` directly from the model.

## Session Filesystem

Introduce a session filesystem router that generalizes the existing
`MountedVfsFileSystem`.

It should route by deepest matching path prefix to one of several backends:

```text
VfsSnapshot
VfsWorkspace
EnvironmentFilesystem
Future filesystem backends
```

This is the file-tool layer from `04-environments.md`. It is not an environment
and has no process executor.

The router should be able to report route metadata for environment projection:

```text
path
access
source
same_state_as_active_env
```

The first implementation can keep using existing VFS mount records for VFS
routes and add an internal route table for environment-backed filesystems. A
larger schema migration is not required for this preparatory refactor.

## Environment Tools

Move process tools under an environment/action package conceptually separate
from filesystem tools.

Initial environment actions:

- `run_process`;
- `write_process_stdin`;
- later: pty/session processes;
- later: computer-use actions;
- later: other actions that require a real OS/session/sandbox surface.

These tools target `env:<env_id>` and validate required capabilities at runtime.
Failures should be capability-derived and instructive:

```text
No execution environment is active. The session filesystem is available through
file tools, but it has no shell. Activate an environment to run commands.
```

That error belongs in the environment action executor, not in the generic
filesystem layer.

## Host Protocol

Keep the host protocol as the boundary for real environment backends.

An environment provider may use the host protocol to supply:

- an environment filesystem adapter implementing `FileSystem`;
- process execution;
- stdin/pty;
- computer-use or display/control capabilities when added;
- target lifecycle and status outside deterministic core.

Do not move host protocol DTOs into the engine. The engine records only semantic
tool target identity and active tool specs.

## Implementation Plan

### [x] G1. Introduce filesystem target namespace

- Add constants for the `fs` and `env` target namespaces.
- Replace hardcoded target namespace literals in built-in tool specs and tests.
  `HOST_TARGET_NAMESPACE` already exists, but at least one P51-era path hardcodes
  `"host"` directly; do not repeat that pattern for `fs` or `env`.
- Change file tool specs to require `fs`.
- Change process tool specs to require `env`.
- Keep compatibility shims only where needed for tests during the refactor.
- Update P51-era tests that assert `host`.

### [x] G2. Extract generic filesystem tool context

- Add `FsToolContext`.
- Move file tool invocation helpers to accept `&FsToolContext` or a narrower
  trait/object containing `fs`, `blobs`, `limits`, and `fs_cwd`.
- Remove process access from generic file tool code.
- Keep behavior unchanged for read/write/edit/grep/glob/list-dir tests.

### [x] G3. Move filesystem modules out from under host

- Move `host/fs` to a top-level filesystem module within `tools`, or create the
  new top-level module and re-export while callers are migrated.
- Move VFS filesystem implementations under the filesystem/VFS area.
- Update imports in skills, prompt loading, temporal server, CLI/eval tests, and
  toolset resolution.
- Explicitly migrate gateway VFS API code that currently reaches for
  `HostToolTargets::{local_execution_target, execution_target}`. That path is a
  high-risk consumer because it wires public session/VFS behavior, not only
  internal tests.

### [x] G4. Introduce environment action context

- Add `EnvironmentToolContext` and target resolution for `env:<id>`.
- Move `run_process` and `write_process_stdin` to use that context.
- Replace the old host target wrapper with top-level `ToolTargets`.
- Preserve host-protocol-backed internals as implementation detail.

### [x] G5. Add session filesystem router

- Generalize `MountedVfsFileSystem` into a router that can mount VFS and
  environment filesystem backends.
- Preserve deepest-prefix-wins behavior.
- Preserve VFS workspace commit effects.
- Expose route metadata needed by `VfsCatalog` / `EnvironmentActive` projection.

### [x] G6. Update hosted runtime composition

- Stop creating a `host:local` target for VFS-only sessions.
- Always configure `fs:session` when file tools are enabled.
- Configure `env:<id>` only when an execution/action environment exists.
- Update gateway/session tool setup and VFS APIs so they set and wait for the
  `fs` default target independently from the `env` default target.
- Ensure `run_process` is unavailable or fails instructively when no active
  environment exists.

### [x] G7. Rename public concepts

- Prefer "environment" in docs, APIs, and model-visible prompt text.
- Keep "host protocol" only for backend transport/control code.
- Avoid model-visible `host` terminology except during temporary compatibility
  windows.

Progress:

- Built-in tool logical IDs now use `fs.*` and `env.*`.
- `HostToolContext`, `HostToolTargets`, `HostToolsetConfig`, and
  `InlineHostToolRuntime` were removed from the tools crate.
- `tools::host_protocol` now means host-protocol adapters, not a semantic tool
  target or combined filesystem/process runtime.
- API wire/config names now use `FilesystemToolMode` and `tools.filesystem`.

## Tests

Add or update tests for:

- file tools work against an in-memory `FileSystem` without any environment;
- file tools work against `MountedVfsFileSystem` through `fs:session`;
- process tools do not route to `fs:session`;
- process tools require `env:<id>` and fail clearly without one;
- tool planning stamps `fs:session` for file calls and `env:<id>` for process
  calls in the same session;
- VFS workspace effects are preserved after module/context extraction;
- session filesystem route precedence remains deepest-prefix-wins;
- environment filesystem routes can be read through the generic file tools.

## Non-Goals

- Do not implement full environment lifecycle APIs in P75.
- Do not implement sandbox providers in P75.
- Do not implement computer use in P75; only leave the environment action shape
  broad enough for it.
- Do not make the engine depend on host protocol DTOs.
- Do not create VFS-specific copies of generic file tools.

## Done When

- VFS-only sessions use file tools through `fs:session` and have no environment
  target.
- File tools are generic over `FileSystem` and do not depend on process or host
  lifecycle concepts.
- VFS code is treated as a filesystem implementation, not as a host.
- Process tools target an environment namespace and cannot accidentally execute
  against a VFS-only filesystem target.
- The hosted runtime can compose a session filesystem separately from an active
  environment action target.

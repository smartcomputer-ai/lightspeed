# P49: Agent Standard Tools

**Status**
- In progress

**Progress**
- Scaffolded `crates/forge-agent-tools` and added it to the workspace.
- Reworked the crate around optional tool packages. The first package is now
  `host`, not a top-level workspace/filesystem design.
- Added `runtime::{ToolTarget, ToolCatalog, ToolBinding, ResolvedToolProfile}`
  so model-facing profiles and invocation dispatch metadata are built together.
- Added host filesystem capability types under `src/host/fs/`: `FileSystem`,
  `FsPath`, `FileAccessPolicy`, filesystem operation option/result types, and
  error types.
- Added `LocalFileSystem`, `ReadOnlyFileSystem`, `ScopedFileSystem`, and
  deterministic `InMemoryFileSystem`.
- Added `ScopedLocalFileSystem` for host-local scoped access with canonical
  root enforcement and symlink escape checks.
- Added host process capability types under `src/host/process/`, including
  `ProcessExecutor` request/result boundary types.
- Added `HostToolContext` and `HostToolLimits`.
- Moved concrete host tool modules under `src/host/tools/`. Each module owns
  typed args/results and canonical invocation functions. `HostToolOperation`
  names the logical host operation, `HostToolSurface` names the model-visible
  surface, and `HostTool` combines the two into a concrete model-facing binding.
  Each surface owns its model-visible names, descriptions, schemas, catalog
  bindings, JSON argument adapters, and dispatch to the concrete operation.
- Implemented `invoke_read_file`, `invoke_write_file`, and `invoke_list_dir`,
  including typed args/results, `cwd` path resolution, write parent-directory
  creation, read line slicing, and focused tests.
- Added `invoke_edit_file`, `invoke_run_process`, and
  `invoke_write_process_stdin` with typed args/results, capability gating,
  process defaults from `HostToolLimits`, exact-match edit validation, and tests.
- Added generic filesystem-backed `invoke_glob` and `invoke_grep` operations
  with recursive traversal, result limits, include filtering, and tests.
- Ported the core Codex apply-patch parser, hunk matcher, and filesystem
  application engine into `src/host/apply_patch/`, adapted to Forge `FileSystem`;
  added `invoke_apply_patch` and tests for add/update/delete/move/error paths.
- Added host profile builders in `src/host/profiles.rs`: `DirectFs`,
  `CodexLike`, `ClaudeCodeLike`, and `Custom`. Profile resolution now returns a
  `ResolvedToolProfile` containing the Forge `ToolRegistry`, schema/description
  documents, and the invocation `ToolCatalog`.
- Added a Claude Code-like host tool surface for `Read`, `Write`, `Edit`,
  `Glob`, `Grep`, and `Bash`. The surface owns Claude-shaped schemas and JSON
  argument adapters, then normalizes calls into the same canonical host
  operations.
- Wired the `CodexLike` profile through the explicit Codex-like surface. It
  currently shares canonical names, schemas, and JSON decoding, while keeping
  distinct `host.codex.*` catalog bindings for dispatch and future
  Codex-specific behavior.
- Split host tool surface code out of `src/host/tools/mod.rs` into
  `canonical.rs`, `codex.rs`, `claude.rs`, and `shared.rs`. Operation modules
  remain provider-neutral; surface modules own model-visible schemas, argument
  adapters, and output shaping.
- Added `InlineHostToolRuntime` for direct in-process invocation and
  `HostToolEffectExecutor` for Forge `ToolInvoke` effects. The catalog binding
  records an activity type and execution mode so a Temporal activity dispatcher
  can use the same resolved profile without changing tool modules.
- Updated `HostToolEffectExecutor` to the narrow `EffectExecutionRequest`
  runner boundary (`session_id` plus intent) so host tools compose with LLM
  adapters under the local runtime router while still receiving the same durable
  `ToolInvoke` intent.

**Design correction**
- `forge-agent-tools` should not be a filesystem/process crate. It should be an
  optional tool package crate. Host filesystem/process interaction is the first
  package under `host`.
- The host package should not make "workspace" the foundation. A workspace is a
  scoped filesystem view. Full local access, read-only local access, scoped
  virtual filesystems, Postgres-backed filesystems, S3-backed filesystems, and
  sandboxed filesystems should all fit behind the same host filesystem trait.
- Things that change together should live together: model-visible names,
  descriptions, schemas, typed args/results, JSON decoding, model-visible output
  shaping, and concrete invocation should be centered on the tool module/package,
  not split across top-level `ops`, `schema`, `registry`, and `toolsets` modules.
- Tool invocation dispatch is a profile/catalog concern. The Forge core persists
  tool specs and `ToolInvocationIntent` values; the tools crate supplies a
  sidecar `ToolCatalog` that maps a visible `ToolName` to a logical tool and
  activity type.

## Goal

Create `forge-agent-tools`, the standard optional tool package crate for Forge
coding agents.

This crate owns model-visible tool contracts and optional concrete tool
packages for common agent use cases. It depends on `forge-agent`, but keeps
local filesystem, abstract filesystem, process behavior, and other tool
substrates out of the deterministic core crate.

`forge-agent-tools` is optional infrastructure. Agent implementations can ignore
it entirely and provide their own `ToolRegistry`, schemas, and `ToolInvoke`
execution path.

## Design Position

The Forge core should see tools as durable data:

- tool name
- JSON schema refs
- provider-visible description refs
- `ToolInvoke` effect intents with `arguments_ref`
- `ToolInvocationReceipt` values

`forge-agent-tools` should own optional agent tool packages:

- typed args and result records
- JSON schemas and descriptions
- `ToolRegistry`/`ToolSpec` builders
- package/profile builders that also produce a `ToolCatalog`
- canonical operation functions where a package has local behavior
- inline `EffectExecutor` implementations for in-process runners and tests
- catalog metadata, including activity type, that future Temporal activities can
  use one tool at a time

Do not make "workspace" the foundation of the host package. The foundation is:

- an agent filesystem capability
- an optional process capability
- explicit access policy exposed by the filesystem capability
- model-visible tool surfaces mapped onto canonical operations

A workspace is just a scoped filesystem view, usually rooted at a project
directory. A full-access local agent, read-only local agent, scoped virtual
filesystem, Postgres-backed tree, S3-backed tree, container filesystem, or
future activity-backed filesystem should all fit behind the same agent
filesystem trait.

One crate is enough for now. Host filesystem/process tools are a package inside
the crate. Other tool packages can be added beside `host` later.

Do not make inline Tokio execution the core abstraction. P47 already provides
the useful runtime boundary through `EffectExecutor` and
`ToolInvocationIntent`. P49 should build catalog/profile metadata that can be
used by inline runners today and Temporal activity dispatchers later.

## Recommended Names

Use neutral names for the capability layer:

| Old/current name | Target name | Reason |
|---|---|---|
| `WorkspaceToolContext` | `HostToolContext` | Context for host-substrate tools, not all future tools. |
| `WorkspaceToolLimits` | `HostToolLimits` | Limits apply to host-substrate tools. |
| `WorkspaceFileSystem` | `FileSystem` | Filesystem available to an agent. |
| `WorkspacePath` | `FsPath` | May represent absolute or relative paths in the filesystem namespace. |
| `WorkspaceFsError` | `FsError` | Error belongs to filesystem capability, not workspace scope. |
| `WorkspaceFsResult` | `FsResult` | Same. |
| `LocalWorkspaceFileSystem` | `LocalFileSystem` plus `ScopedFileSystem` | Local host access and scoping are separate concepts. |
| `InMemoryWorkspaceFileSystem` | `InMemoryFileSystem` | Useful beyond workspace tests. |
| `WorkspaceProcessExecutor` | `ProcessExecutor` | Process capability available to an agent. |
| `WorkspaceProcessHandle` | `ProcessHandle` | Handle is not workspace-specific. |
| `WorkspaceToolProfilePreset` | `HostToolPreset` | Host tool surface controls model-visible host tools, not filesystem scope. |
| `WorkspaceToolExecutor` | `HostToolEffectExecutor` | Executes host tool effects inline for local runners/tests. |

Use `fs` as the module abbreviation because it is conventional. Spell out
`process` in module names and public APIs.

## Target Crate Shape

Move the target API away from `src/workspace/` and away from top-level
filesystem/process assumptions:

```text
crates/forge-agent-tools/
  Cargo.toml
  src/lib.rs
  src/error.rs
  src/runtime/mod.rs
  src/runtime/target.rs
  src/host/mod.rs
  src/host/context.rs
  src/host/executor.rs
  src/host/fs/mod.rs
  src/host/fs/access.rs
  src/host/fs/path.rs
  src/host/fs/local.rs
  src/host/fs/scoped.rs
  src/host/fs/scoped_local.rs
  src/host/fs/read_only.rs
  src/host/fs/memory.rs
  src/host/process/mod.rs
  src/host/process/local.rs
  src/host/tools/mod.rs
  src/host/tools/shared.rs
  src/host/tools/canonical.rs
  src/host/tools/codex.rs
  src/host/tools/claude.rs
  src/host/tools/read_file.rs
  src/host/tools/write_file.rs
  src/host/tools/edit_file.rs
  src/host/tools/apply_patch.rs
  src/host/tools/grep.rs
  src/host/tools/glob.rs
  src/host/tools/list_dir.rs
  src/host/tools/run_process.rs
  src/host/tools/write_process_stdin.rs
  src/host/apply_patch/mod.rs
  src/host/apply_patch/parser.rs
  src/host/apply_patch/seek_sequence.rs
  src/host/apply_patch/invocation.rs
  src/host/apply_patch/engine.rs
  src/host/profiles.rs
```

`runtime` is package-neutral and owns catalog/profile assembly types.
`host` is the first concrete package and owns host filesystem/process
capabilities, concrete tools, profile presets, and inline effect execution.
There should be no top-level `ops`, `schema`, `registry`, or `toolsets` split.

Candidate public entry points:

```rust
let target = ToolTarget::from(&model_selection);
let resolved = resolve_host_profile(&ctx, &target, HostToolPreset::CodexLike)?;
let resolved = resolve_host_profile_for_model(&ctx, &model_selection, HostToolPreset::CodexLike)?;

resolved.registry      // durable Forge model-facing data
resolved.documents     // schema/description blobs to store
resolved.catalog       // host-side invocation bindings

HostToolEffectExecutor::new(ctx, resolved.catalog)
```

## Layering Contract

Keep package/runtime boundaries separate.

Layer 1 is the package-neutral runtime/catalog API:

- `ToolTarget` derived from `ModelSelection`
- `ToolDocument` and `ToolSpecBundle`
- `ToolBinding` with visible name, logical id, activity type, execution mode,
  and parallelism
- `ToolCatalog`, the sidecar that maps `ToolName` to invocation metadata
- `ResolvedToolProfile`, which carries `ToolRegistry`, documents, and catalog

Layer 2 is the host substrate API. It is provider-neutral and close to the host
effects the agent needs:

- filesystem capability
- process capability
- access policy reported by the filesystem implementation
- local, fake, abstract, container, remote, and future activity-backed
  implementations

Layer 3 is the host tool package:

- `invoke_read_file`
- `invoke_write_file`
- `invoke_edit_file`
- `invoke_apply_patch`
- `invoke_grep`
- `invoke_glob`
- `invoke_list_dir`
- `invoke_run_process`
- `invoke_write_process_stdin`
- `ToolSpec`/`ToolProfile` builders
- model-visible names and descriptions
- JSON schemas
- decoding `ToolInvocationIntent.arguments_ref`
- mapping tool calls to canonical operations
- shaping `ToolInvocationReceipt` output for the model

Do not let model-trained tool names leak into the host capability API. For
example, `Read`, `read_file`, and an MCP-style `filesystem_read_file` can all
map to `invoke_read_file`. Likewise, `Bash` and `exec_command` can map to
`invoke_run_process` through different host profile/tool bindings if both
surfaces are enabled later.

## Internal Capability API

Core context:

```rust
pub struct HostToolContext {
    pub fs: Arc<dyn FileSystem>,
    pub process: Option<Arc<dyn ProcessExecutor>>,
    pub blobs: Arc<dyn BlobStore>,
    pub limits: HostToolLimits,
    pub cwd: Option<FsPath>,
}
```

`cwd` is the default directory for relative model/tool arguments. For a
workspace-scoped filesystem it is usually `"."` or a scoped subdirectory. For a
full local filesystem it can be an absolute host path. For an abstract
filesystem it is a path in that filesystem's namespace.

Filesystem capability:

```rust
#[async_trait]
pub trait FileSystem: Send + Sync {
    fn access_policy(&self) -> FileAccessPolicy;

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>>;

    async fn read_file_text(&self, path: &FsPath) -> FsResult<String> {
        let bytes = self.read_file(path).await?;
        String::from_utf8(bytes).map_err(FsError::invalid_data)
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()>;

    async fn create_directory(
        &self,
        path: &FsPath,
        options: CreateDirectoryOptions,
    ) -> FsResult<()>;

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata>;

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>>;

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()>;

    async fn copy(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()>;
}
```

This should remain close to Codex's `ExecutorFileSystem` operation shape, but
Forge should not copy Codex's per-call sandbox context. The filesystem
implementation is already the enforcement boundary.

`FsPath` should accept both relative and absolute paths. This is the main
correction from the current `WorkspacePath` design.

Initial `FsPath` rules:

- preserve logical slash-separated paths at the capability boundary
- reject empty paths and NUL bytes
- normalize `.` and collapsible internal `..` segments where possible
- allow absolute paths such as `/Users/lukas/dev/forge/Cargo.toml`
- allow relative paths such as `src/lib.rs`
- allow relative paths with leading `..` until they are resolved against `cwd`
  or a scoped filesystem root
- reject path escapes in the resolver or scoped filesystem wrapper, not in the
  broad path type
- leave host-specific mapping to `LocalFileSystem`

For abstract filesystems, "absolute" means absolute in that filesystem's
namespace, not necessarily a host OS path. A Postgres-backed tree or S3-backed
tree can reject path forms it does not support, or expose a virtual absolute
root such as `/`.

Keep higher-level conveniences such as recursive listing, `create_dir_all`,
existence checks, exact edit, and rename/move semantics in operation functions
unless every reasonable filesystem substrate needs the primitive directly. For
example, `invoke_list_dir` can page/recurse over `read_directory`;
`invoke_apply_patch` can use `copy` plus `remove` for moves until a native
rename operation is needed.

Process capability:

```rust
pub type ProcessExecResult<T> = Result<T, ProcessError>;

#[async_trait]
pub trait ProcessExecutor: Send + Sync {
    async fn run_process(&self, request: ProcessRequest) -> ProcessExecResult<ProcessOutput>;

    async fn write_stdin(
        &self,
        request: WriteProcessStdinRequest,
    ) -> ProcessExecResult<ProcessOutput>;
}
```

`ProcessRequest` should contain a resolved argv vector, not a shell string or
`Shell`/`Argv` enum. Model-visible tools such as Codex `exec_command` and
Claude Code `Bash` may accept shell-shaped arguments, but their tool adapters
must resolve those arguments to argv before calling `ProcessExecutor`.

Process execution is a separate trust boundary from filesystem tools. For the
initial Forge agent tools design, granting shell/process execution should be
treated as granting full authority over whatever substrate the process executor
can reach. Do not claim read-only filesystem safety while also exposing an
unconstrained local shell. A read-only or scoped process mode requires a
separately isolated process executor, such as a container, VM, remote worker, or
future policy-aware process implementation.

## File Access Policy

Use a small explicit policy model first:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileAccessPolicy {
    FullReadWrite,
    FullReadOnly,
    ScopedReadWrite { root: FsPath },
    ScopedReadOnly { root: FsPath },
}
```

Definitions:

- `FullReadWrite`: read and write anywhere in the filesystem implementation's
  namespace.
- `FullReadOnly`: read anywhere in the filesystem implementation's namespace;
  all mutation operations fail.
- `ScopedReadWrite`: read and write only below `root`.
- `ScopedReadOnly`: read only below `root`; all mutation operations fail.

"Full" is relative to the filesystem implementation. For `LocalFileSystem`,
full means the host filesystem reachable by the process. For `S3FileSystem`,
full may mean the configured bucket/prefix namespace. For `PostgresFileSystem`,
full may mean the configured logical tree.

The policy is not merely descriptive. The filesystem implementation or wrapper
must enforce it. The registry can use `access_policy()` to avoid exposing
unsupported write tools, but operation functions must still handle
`PermissionDenied` from the filesystem because the filesystem remains the real
authority.

Start with policy implemented through multiple filesystem implementations and
wrappers:

```rust
LocalFileSystem::full_access()                 // FullReadWrite host fs
ReadOnlyFileSystem::new(LocalFileSystem::full_access()) // FullReadOnly host fs
ScopedFileSystem::read_write(root, inner)      // ScopedReadWrite
ScopedFileSystem::read_only(root, inner)       // ScopedReadOnly
InMemoryFileSystem::new(policy)                // deterministic tests
```

This keeps policy composable:

- a full local filesystem can be wrapped as read-only
- a full local filesystem can be wrapped as workspace-scoped read/write
- a scoped read/write filesystem can be wrapped as read-only
- a Postgres or S3 filesystem can enforce policy internally or be wrapped
- a sandboxed/container filesystem can implement the same trait and report the
  policy it enforces

Do not add a `SandboxPolicy` type to this crate in the first pass. A sandbox is
a filesystem/process implementation strategy, not a separate policy category.
Examples:

- `SandboxedLocalFileSystem` implements `FileSystem`.
- `ContainerFileSystem` implements `FileSystem`.
- `RemoteActivityFileSystem` implements `FileSystem`.
- `ReadOnlyFileSystem<T>` and `ScopedFileSystem<T>` are policy wrappers over
  another filesystem.

Codex's sandbox modes remain useful reference material, but Forge should not
copy Codex's sandbox configuration model into the filesystem trait.

## Standard Filesystem Implementations

Initial implementations:

- `LocalFileSystem`: direct host filesystem implementation, constructed through
  an explicit `full_access()` constructor. It supports absolute and relative
  paths and reports `FullReadWrite`.
- `ScopedFileSystem<T>`: wrapper that resolves all paths under a configured
  logical root. Use this for virtual/logical filesystems where path semantics
  are owned by the wrapped implementation.
- `ScopedLocalFileSystem`: secure host-local scoped filesystem. It canonicalizes
  the configured root, rejects parent and symlink escapes, and should be used
  instead of generic `ScopedFileSystem<LocalFileSystem>` for host filesystem
  boundaries.
- `ReadOnlyFileSystem<T>`: wrapper that delegates reads and rejects writes,
  directory creation, remove, and copy-to-destination mutations.
- `InMemoryFileSystem`: deterministic virtual filesystem for tests.

Future implementations should fit without new tool code:

- `PostgresFileSystem`: logical tree in Postgres.
- `S3FileSystem`: bucket/prefix-backed object tree.
- `CxdbFileSystem`: content-addressed/versioned Forge workspace tree.
- `ContainerFileSystem`: filesystem view inside a container/VM.
- `ActivityFileSystem`: Temporal activity-backed filesystem operations.
- `RemoteFileSystem`: remote execution server or worker protocol.

Postgres/S3/CXDB filesystems should not be forced to mimic host paths exactly.
They implement the same operation contract over their own namespace. Unsupported
operations should return `FsError::Unsupported`, and registry construction
should avoid exposing tools that require unsupported write or directory
semantics.

## Canonical Operations

Each operation module should expose a typed function usable by inline execution
and future activity wrappers:

```rust
invoke_read_file(ctx, ReadFileArgs) -> Result<ReadFileResult, ToolError>
invoke_write_file(ctx, WriteFileArgs) -> Result<WriteFileResult, ToolError>
invoke_edit_file(ctx, EditFileArgs) -> Result<EditFileResult, ToolError>
invoke_apply_patch(ctx, ApplyPatchArgs) -> Result<ApplyPatchResult, ToolError>
invoke_grep(ctx, GrepArgs) -> Result<GrepResult, ToolError>
invoke_glob(ctx, GlobArgs) -> Result<GlobResult, ToolError>
invoke_list_dir(ctx, ListDirArgs) -> Result<ListDirResult, ToolError>
invoke_run_process(ctx, RunProcessArgs) -> Result<ProcessOutput, ToolError>
invoke_write_process_stdin(ctx, WriteProcessStdinArgs) -> Result<ProcessOutput, ToolError>
```

`grep` and `glob` can start as generic operations over the filesystem boundary.
If a substrate has a better index or native search implementation, add an
optional search extension later rather than forcing every filesystem to expose
search primitives up front.

## Host Tool Profiles

Use the existing `forge-agent` `ToolRegistry` and `ToolProfile` model. Do not
add a second durable selection mechanism. The tools crate may keep a sidecar
`ToolCatalog` for invocation dispatch, because that catalog is runner-side
metadata rather than Forge session state.

Start with fewer model-visible presets:

```rust
pub enum HostToolPreset {
    DirectFs,
    CodexLike,
    ClaudeCodeLike,
    Custom(HostToolSelection),
}
```

Preset intent:

- `DirectFs`: expose direct filesystem tools using Forge canonical names:
  `read_file`, `grep`, `glob`, `list_dir`, and write tools when the filesystem
  policy permits writes.
- `CodexLike`: expose Codex-compatible process/patch behavior:
  `exec_command`, `write_stdin`, `apply_patch`, and `list_dir` when supported.
- `ClaudeCodeLike`: expose Claude Code-style direct host tools:
  `Read`, `Write`, `Edit`, `Glob`, `Grep`, and `Bash` when supported. These
  tools use Claude-shaped model-visible arguments but dispatch through the same
  canonical Forge operations.
- `Custom`: allow callers to select an exact host tool set for a specific model,
  provider, runner, or product surface.

Preset construction must be capability-gated:

- If `ctx.process` is `None`, omit process tools.
- If `ctx.fs.access_policy()` is read-only, omit `write_file`, `edit_file`, and
  `apply_patch` from direct filesystem surfaces.
- If `ctx.fs.access_policy()` is scoped, model-visible path guidance should say
  paths are resolved within the configured scope.
- If the filesystem reports an operation as unsupported, omit dependent tools
  where that can be known up front.

Do not expose process tools in a surface that is advertised as read-only unless
the process executor is itself isolated to the same read-only filesystem view.
For the initial pass, local process execution is full-authority.

## Tool Set Mappings

Initial mappings:

| Preset | Model-visible tool | Internal operation |
|---|---|---|
| `DirectFs` | `read_file` | `invoke_read_file` |
| `DirectFs` | `write_file` | `invoke_write_file` |
| `DirectFs` | `edit_file` | `invoke_edit_file` |
| `DirectFs` | `apply_patch` | `invoke_apply_patch` |
| `DirectFs` | `grep` | `invoke_grep` |
| `DirectFs` | `glob` | `invoke_glob` |
| `DirectFs` | `list_dir` | `invoke_list_dir` |
| `CodexLike` | `exec_command` | `invoke_run_process` |
| `CodexLike` | `write_stdin` | `invoke_write_process_stdin` |
| `CodexLike` | `apply_patch` | `invoke_apply_patch` |
| `CodexLike` | `list_dir` | `invoke_list_dir` |
| `ClaudeCodeLike` | `Read` | `invoke_read_file` |
| `ClaudeCodeLike` | `Write` | `invoke_write_file` |
| `ClaudeCodeLike` | `Edit` | `invoke_edit_file` |
| `ClaudeCodeLike` | `Glob` | `invoke_glob` |
| `ClaudeCodeLike` | `Grep` | `invoke_grep` |
| `ClaudeCodeLike` | `Bash` | `invoke_run_process` |

`Custom` exposes exactly the caller-selected host tools after capability gating.

Keep `list_dir` as a standard tool. It is useful for process-free substrates
and gives models a cheap directory inspection tool without requiring shell.

Do not include `stat` or `exists` as initial model-visible tools. Keep metadata
and existence checks on the filesystem trait for tool implementation and
validation, and add model-visible `stat`/`exists` later only if a concrete model
profile or product flow benefits from them.

## Migration Plan

The first refactor has moved the implementation in place:

1. Replace the top-level workspace/capabilities/ops/schema/toolsets split with
   `runtime` plus the first concrete package, `host`.
2. Keep host filesystem/process capabilities under `host::fs` and
   `host::process`.
3. Move concrete tool args/results and invoke functions under `host::tools`.
4. Split host tool identity into `HostToolOperation`, `HostToolSurface`, and
   `HostTool`. `HostToolOperation` is the logical behavior, `HostToolSurface`
   is the model-visible naming/schema family, and `HostTool` is the concrete
   binding used in profiles.
5. Put host profile selection and `ResolvedToolProfile` construction under
   `host::profiles`.
6. Keep inline execution as one runtime implementation through
   `InlineHostToolRuntime` and `HostToolEffectExecutor`.
7. Keep activity dispatch pluggable by recording `ToolBinding.activity_type` and
   `ToolBinding.execution`.

Do not preserve root-jail behavior inside `LocalFileSystem`; that belongs in
`ScopedFileSystem`. `LocalFileSystem` should be the direct full-access local
implementation, with the scary behavior made explicit by construction and API
name.

Recommended constructors:

```rust
let fs = LocalFileSystem::full_access();
let fs = ReadOnlyFileSystem::new(LocalFileSystem::full_access());
let fs = ScopedFileSystem::read_write(root, LocalFileSystem::full_access())?;
let fs = ScopedFileSystem::read_only(root, LocalFileSystem::full_access())?;
```

Default high-level builders should choose scoped access unless the caller
explicitly asks for full local access.

## References

Codex should be the primary reference for argument shapes and model-visible
behavior for `CodexLike`:

- `/Users/lukas/dev/tmp/codex/codex-rs/file-system/src/lib.rs`
  - `ExecutorFileSystem`, file metadata types, operation option types, and
    absolute-path capability shape
- `/Users/lukas/dev/tmp/codex/codex-rs/exec-server/src/local_file_system.rs`
  - local, unsandboxed, and sandbox-dispatch filesystem implementation pattern
- `/Users/lukas/dev/tmp/codex/codex-rs/protocol/src/permissions.rs`
  - read/write/none filesystem access modes and path policy reference
- `/Users/lukas/dev/tmp/codex/codex-rs/protocol/src/protocol.rs`
  - `read-only`, `workspace-write`, and `danger-full-access` reference modes
- `/Users/lukas/dev/tmp/codex/codex-rs/core/src/tools/handlers/`
  - concrete tool handlers such as `unified_exec.rs`, `apply_patch.rs`, and
    `list_dir.rs`
- `/Users/lukas/dev/tmp/codex/codex-rs/core/src/tools/runtimes/`
  - concrete unified exec/apply-patch runtime structure
- `/Users/lukas/dev/tmp/codex/codex-rs/core/src/tools/registry.rs`
  - handler registry shape
- `/Users/lukas/dev/tmp/codex/codex-rs/core/src/tools/spec.rs`
  - how Codex composes configured tool specs and handlers

Claude Code is the reference for `ClaudeCodeLike` direct filesystem/process
tool naming and schemas:

- `/Users/lukas/dev/tmp/claude-code/src/tools/BashTool/BashTool.tsx`
- `/Users/lukas/dev/tmp/claude-code/src/tools/FileReadTool/FileReadTool.ts`
- `/Users/lukas/dev/tmp/claude-code/src/tools/FileWriteTool/FileWriteTool.ts`
- `/Users/lukas/dev/tmp/claude-code/src/tools/FileEditTool/FileEditTool.ts`
- `/Users/lukas/dev/tmp/claude-code/src/tools/GrepTool/GrepTool.ts`
- `/Users/lukas/dev/tmp/claude-code/src/tools/GlobTool/GlobTool.ts`

AOS is a secondary implementation reference, especially for broader filesystem
tooling and effect-adapter separation:

- `/Users/lukas/dev/aos/crates/aos-effect-adapters/src/adapters/host/`
- `refs/aos-agent/src/tools/`
- `refs/aos-agent/src/tools/supported/host_fs_write_file.rs`
- `refs/aos-agent/src/tools/supported/host_fs_apply_patch.rs`
- `refs/aos-agent/src/tools/supported/host_exec.rs`

Do not copy AOS-specific world/session assumptions into Forge.

## Vendor/Port Scope

Vendor/port these Codex pieces as adapted Forge code:

- `codex-apply-patch`
  - primary `apply_patch` parser, engine, invocation shape, shell/heredoc
    detection, output behavior, and tests
- `codex-file-system`
  - primary filesystem trait operation shape, metadata/option types, and local
    filesystem implementation pattern
  - keep the idea of a broad filesystem capability; do not reduce it to a
    workspace-rooted path type
- `codex-utils-absolute-path`
  - path normalization, absolute path wrapper behavior, home expansion, and
    symlink-preserving canonicalization ideas
  - adapt into Forge `FsPath`/local host mapping rather than exposing Codex
    names
- `codex-utils-output-truncation`
  - text/output truncation behavior for model-visible tool results; port the
    small text helpers and tests, replacing Codex protocol-specific content item
    types with Forge receipt/blob handling

Port selected files or behavior, not whole crates:

- `codex-tools`
  - copy/adapt JSON schema helpers and model-facing tool builders for
    `exec_command`, `write_stdin`, `list_dir`, and `apply_patch`
  - do not import unrelated Codex tool packages such as web search, images,
    MCP, agents, plugins, code mode, or goals into `forge-agent-tools`
- `codex-shell-command`
  - useful later for shell parsing, command summarization, and dangerous command
    classification
  - defer until Forge adds approval/process policy for execution, because the
    dependency and behavior surface is larger than the first local executor
    needs

Use as references only for now:

- `codex-core/src/tools/handlers/`
  - handler argument parsing, model-facing output shapes, and edge-case tests
- `codex-core/src/tools/runtimes/`
  - approval, retry, and exec orchestration structure
- `codex-core/src/unified_exec/`
  - interactive process/session lifecycle, `yield_time_ms`,
    `max_output_tokens`, output buffering, and `write_stdin` semantics
- `codex-file-search`
  - fuzzy file search/reference implementation; not required for initial
    `grep`/`glob` tools
- `codex-sandboxing`, `codex-exec-server`, `codex-execpolicy`,
  `codex-shell-escalation`, `codex-arg0`
  - useful substrate references, but too tied to Codex's CLI, platform helpers,
    approval model, and remote exec-server lifecycle for the first
    `forge-agent-tools` pass

## Patch Tool Reference Split

`apply_patch` needs special care. Treat Codex as the primary reference for the
model-facing contract, patch engine, and runtime integration:

- freeform/grammar tool shape
- JSON fallback shape
- argument names and descriptions
- relative-path guidance for model-facing instructions
- interception from shell/exec invocations
- path resolution against `HostToolContext.cwd`
- path collection for write/approval decisions
- parsing, verification, filesystem application, move/add/delete/update
  behavior
- unified diff generation and success/error output shape
- streaming patch progress events

Treat `/Users/lukas/dev/aos/crates/fabric-host/src/patch/` as a supplemental
reference for matching/edit behavior worth comparing against Codex:

- tolerant hunk application
- exact then fuzzy matching
- whitespace normalization
- unicode punctuation canonicalization
- ambiguity detection
- edit-style replacement helpers

The Forge implementation should start from Codex-compatible patch behavior and
only borrow `fabric-host` ideas where they demonstrably improve reliability
without diverging from the tool behavior coding models already expect.

Implementation plan: vendor/port Codex's `codex-apply-patch` crate as the
baseline `apply_patch` implementation inside `forge-agent-tools`, with
Apache-2.0 attribution/notice requirements preserved and modified files marked
as adapted for Forge.

Target adapted module shape:

```text
src/apply_patch/
  mod.rs
  parser.rs
  seek_sequence.rs
  invocation.rs
  engine.rs
  tests/
```

Keep Codex behavior and tests as close as practical, especially parser,
matching, shell/heredoc detection, output shape, and add/delete/update/move
semantics. Adapt only the edges:

- replace `codex_exec_server::ExecutorFileSystem` with `FileSystem`
- replace `codex_utils_absolute_path::AbsolutePathBuf` with Forge `FsPath`
  plus local host path mapping
- remove Codex-specific approval, sandbox, Guardian, and protocol event
  plumbing from the copied engine
- integrate results into `ToolInvocationReceipt` and `BlobStore`

## Execution Semantics

Agent tool execution should:

- load model arguments from `ToolInvocationIntent.arguments_ref`
- write large outputs/errors to `BlobStore`
- return `ToolInvocationReceipt`
- preserve model-visible output separately from diagnostic/raw output where
  useful
- classify failures as tool receipts, not runner panics
- keep read-only filesystem tools parallel-safe
- treat write tools and process tools as exclusive unless the implementation
  proves narrower resource safety

The crate should provide inline handlers for in-process runners and tests. Future
Temporal integration should wrap the same per-tool operation functions as
separate activities, not call a mega tool activity.

The inline executor is allowed to implement `EffectExecutor` by handling only
`AgentEffectIntent::ToolInvoke`. It should return `Unsupported` for LLM effects
or tools it does not know how to execute. A later runner can compose this with
the LLM executor from P50.

## Safety Notes

Start conservative in builders, but keep the foundation general:

- default local tool builders should use scoped filesystem access
- full local access should require explicit construction
- read-only filesystem surfaces must not expose write/edit/apply-patch tools
- read-only claims must not include unconstrained local process execution
- path traversal checks belong in `FsPath` normalization and scoped wrappers
- symlink escape checks belong in local scoped wrappers
- process execution requires an explicit process execution context
- output size limits should materialize oversized output as blobs

## Out Of Scope

- Temporal workflow/activity code.
- Provider-native hosted tools.
- MCP tools.
- AOS workspace/introspection tools.
- Production outbox enforcement.
- Full command approval or shell policy.
- VM/container sandbox orchestration.
- Full Claude Code behavior parity, such as rich grep context output,
  background Bash tasks, read-before-edit/write enforcement, binary/PDF/image
  `Read`, and Claude Code permission flows.

## Dependencies

- Depends on P47 runner/effect contracts in `forge-agent`.
- Provides standard tool packages consumed by in-process/process runners.
- Informs P60 outbox rules for non-idempotent write/process tools.

## Done When

- `forge-agent-tools` builds as a workspace crate.
- The crate is optional: `forge-agent` and `forge-agent-llm` do not depend on
  it.
- It exports a standard agent tool package.
- The internal API is no longer workspace-rooted at the foundation.
- It supports these initial filesystem access policies:
  - full read/write
  - full read-only
  - scoped read/write
  - scoped read-only
- Direct filesystem tools are gated from the filesystem access policy.
- Process tools are exposed only when a `ProcessExecutor` is configured.
- All initial tools have typed args/results, schemas, inline handlers, and
  deterministic unit tests.
- Tool outputs and errors use `BlobStore` where appropriate.
- The implementation does not add local filesystem/process dependencies to
  `forge-agent`.
- `cargo check -p forge-agent-tools` and `cargo test -p forge-agent-tools`
  pass.

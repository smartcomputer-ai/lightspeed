# P195 — Native Windows environments

## Goal and decisions

Agents must be able to develop, build, test, and run software in real Windows
environments, including enterprise machines where Linux compatibility runtimes
are unavailable. The existing Lightspeed agent/runtime can remain on Linux;
envd supplies Windows execution and filesystem operations through the existing
environment boundary. This does not require porting the whole hosted runtime.

1. **Native Windows envd.** Ship `lightspeed-envd.exe`, initially targeting
   `x86_64-pc-windows-msvc`. Windows file, process, terminal, and lifecycle
   behavior belongs in native platform implementations.
2. **PowerShell is the default command shell.** Prefer PowerShell 7 (`pwsh.exe`)
   when installed, with Windows PowerShell (`powershell.exe`) as the supported
   baseline fallback. Advertise the actual shell and version to the agent.
3. **Git Bash is an optional, explicitly selected shell.** Discover and use it
   when installed. It is neither a daemon dependency nor an automatic fallback
   for a failing PowerShell command. A missing selected shell is a clear error.
4. **WSL and Cygwin are out of scope.** Do not require, provision, or use them
   as fallbacks. Their absence is a normal supported enterprise configuration.
   We are not building a separate Cygwin-targeted daemon or an MSYS runtime port.
5. **Separate the shell from the operating system.** Selecting Git Bash does
   not change the environment into Linux, change the daemon's path contract,
   or replace native file operations with shell commands.
6. **Keep structured execution and filesystem tools.** `argv` remains the
   process primitive. Reading, editing, searching, and transferring files must
   not depend on PowerShell or Bash text parsing.
7. **Preserve existing behavior on Linux and macOS.** Share protocol and
   lifecycle invariants; isolate platform mechanisms behind small interfaces.
   Do not make deterministic harness code inspect the host OS or execute I/O.

The successful use of OpenCode with Git Bash in a Windows enterprise project
motivates keeping Bash available. It does not establish that Bash is always
better for an agent, or that the daemon itself should emulate Unix. Shell
effectiveness should be measured on our workloads after correctness is in place.

## Findings and current baseline

On 2026-11-06, a Windows Server 2025 Desktop Experience development VM was
prepared on `hz01` (`windows-build`, Incus project `build-windows`). It has
8 configured vCPUs, 32 GiB RAM, and a 150 GiB disk. Development access is RDP
through an SSH tunnel; no public RDP listener is required by this work.

The Windows checkout is `C:\dev\lightspeed`. Installed tools are Rust 1.99.0
with the MSVC host toolchain, Clippy, rustfmt, Rust sources, Git for Windows,
Visual Studio 2022 C++ Build Tools, Windows SDK, CMake, NASM, and VS Code with
rust-analyzer. The desktop **Lightspeed** and **Lightspeed terminal** shortcuts
load the build environment. No Windows CI runner has been registered.

The standalone Rust compile/link/run check passed. The first release build at
the base commit above compiled dependencies, then failed with **27 errors and
one warning** in envd. This is evidence that the toolchain works, not that the
remaining port consists of only 27 small fixes. Several non-Unix fallbacks
already compile while omitting required behavior.

| Area | Evidence in the current source | Required work |
| --- | --- | --- |
| Filesystem primitives | `filesystem/os.rs` imports `OwnedFd`, Unix string extensions, and `rustix`; `filesystem.rs` imports `rustix::io::Errno` | Native Windows handles and platform-neutral error translation |
| Confinement and identity | `filesystem/confinement.rs` uses Unix directory modes and device/inode identity | Windows file identity, private state permissions, and race-resistant root/state exclusion |
| Transfer and enumeration | `filesystem/transfer/journal.rs`, `transfer/mod.rs`, and `walk.rs` use Unix byte names and permissions | Windows-safe name/journal representation, ordering, metadata, and transfer semantics |
| PTY interruption | `exec/execution.rs` calls Unix-only `process_group_leader()` | Windows terminal interruption and ownership |
| Process lifecycle | Non-Unix paths in `process_group.rs` do not create or signal process groups and return no liveness/start-time facts | Native process-tree tracking, cancellation, cleanup, and leftover/idle reporting |
| Model-facing shell | `crates/tools/src/builtin/codex.rs` constructs `bash -lc` / `bash -c` unconditionally | Select the shell from the target environment, including when the controller runs on Linux |
| Environment paths | `crates/environment-protocol/src/paths.rs` explicitly requires absolute POSIX paths | An explicit Windows-capable path contract across clients and tools |
| Discovery | Data `initialize` has cwd/home and capabilities but no explicit shell catalog/path convention | Discoverable execution facts; do not infer them from descriptive build information |
| Configuration/lifecycle | State defaults use XDG/HOME; non-Unix automatic re-exec reports unsupported; upgrade archives expect `lightspeed-envd` | Windows state/home defaults, `.exe` packaging, installation and restart/upgrade strategy |

These paths are under `crates/environment-daemon/src` unless otherwise noted.
The build log is `C:\dev\logs\envd-windows-build.log`; the verification record
is `C:\dev\logs\build-tools-verification.json`. Provisioning scripts and the
RDP helper live in the sibling `ls.bot` repository under
`infra/incus/windows-build` and `infra/scripts/windows-build`. Credentials do
not belong in this document or in the Lightspeed checkout.

Git Bash was also checked on the VM: it reports `/c/dev/lightspeed`, `cygpath`
resolves that to `C:\dev\lightspeed`, and the invoked Rust compiler still
reports `x86_64-pc-windows-msvc`. Wrapping the current Rust build in Bash does
not supply the missing Unix APIs.

## What to learn from Codex

The inspected local checkout was `../tmp/codex`, at
`40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f` (2026-09-21). The links below pin
that source revision. These are implementation observations, not claims that
every current Codex product uses an identical execution path.

| Observation | Source | Application to Lightspeed |
| --- | --- | --- |
| Windows defaults to PowerShell, with `pwsh` preferred over Windows PowerShell; the ultimate fallback is `cmd.exe` | [Shell discovery][codex-shell-detect] | Adopt PowerShell preference and explicit discovery. Do not silently adopt the `cmd.exe` fallback as our default-shell policy. |
| Shell types have distinct argument construction: Bash `-c`/`-lc`, PowerShell `-Command` and optional `-NoProfile`, CMD `/c` | [Shell invocation][codex-shell] | One typed shell adapter per supported shell; no universal command-string escaping function. |
| Native terminal execution uses ConPTY, including Windows-specific input normalization | [PTY implementation][codex-pty], [input normalization][codex-input] | Preserve the common terminal API while implementing Windows input, resize, interrupt, and close behavior deliberately. |
| Windows Job Objects track/terminate process trees | [Job implementation][codex-job] | Use owned Windows handles and Job Objects instead of making Unix process-group helpers no-ops. |
| ConPTY spawning requires atomic job attachment, while the pipe path documents a spawn/assignment race and a weaker fallback | [PTY job attachment][codex-pty-attrs], [pipe spawning][codex-pipe] | Learn from both the implementation and its limitations. Require containment before execution where our cleanup contract promises it; fail clearly if it cannot be established. |
| Paths are typed file URIs with explicit POSIX/Windows grammar, rather than interpreted by the controller's OS | [Path URI implementation][codex-path] | Make remote path parsing independent of the Lightspeed host. URI encoding is a concrete candidate, not a reason to copy Codex's whole type. |
| Windows guidance warns against passing destructive filesystem operations through multiple shells | [Model-facing shell guidance][codex-guidance] | State OS/shell facts and prefer structured file tools; keep a shell operation in one shell. |
| Native sandbox code uses Windows access tokens and process APIs | [Token handling][codex-token], [process creation][codex-process] | Shell selection and OS isolation are separate concerns. Bash is not a security boundary. |

Do not copy Codex wholesale. In particular, inspect Job Object breakaway flags,
handle lifetime, and normal-exit behavior against Lightspeed's own process/job
contracts. Codex's general Job Object helper permits explicit breakaway and
also provides a stricter variant; a kill-on-close flag alone does not prove that
every descendant will be contained. Likewise, lexical path equality is not
filesystem identity or confinement, and ASCII case-folding is not a complete
Windows filesystem model.

Current official [OpenAI Windows documentation][openai-windows] also describes
a native PowerShell agent, with WSL as a separate choice. [OpenCode's Windows
documentation][opencode-windows] recommends WSL for its own best experience.
These are different product choices, not comparative model benchmarks. Our
deployment requirement excludes WSL regardless of those recommendations.

## Architecture and protocol work

### Remote platform and shell facts

Extend environment discovery with typed execution facts: OS/platform, path
convention, available shell identities and executable paths, actual shell
versions, and the default shell. Treat these as observations of the selected
environment. `ImplementationInfo.target` is descriptive and must not become a
capability or authorization mechanism.

Keep command execution expressed as program plus arguments. The model-facing
adapter resolves a requested shell against discovered facts, then builds its
argv. It must not use `cfg!(windows)` on a Linux controller to decide how a
remote Windows environment runs commands. Unavailable shells and unsupported
capabilities return typed errors before spawning.

Proposed shell behavior:

- Prefer `pwsh.exe`; support Windows PowerShell 5.1 where PowerShell 7 is absent.
  Guidance and tests must account for their different syntax and encoding.
- Use explicit executable paths and deterministic profile handling. Default
  unattended execution should avoid loading user profiles. Define how the
  existing `login` option maps to each shell before changing its behavior;
  intentional profile loading remains explicit.
- Select non-interactive versus interactive invocation according to the tool
  request. A PTY must continue to support interactive child programs.
- Discover Git Bash from an operator-configured path or verified Git for
  Windows installation. Do not mistake a WSL `bash.exe` launcher on PATH for
  Git Bash. Never require installing Git Bash to use the default environment.
- Do not silently switch shells after a command error. Direct `.exe` execution
  remains available without a shell; batch-file invocation needs an explicit,
  tested CMD adapter where required by a build tool.
- Test native argument passing separately from shell-language quoting,
  especially PowerShell 5.1 versus PowerShell 7 and `.cmd`/`.bat` entry points.
  Preserve native program exit status and distinguish it from a shell parse
  failure. Do not flatten an argv request into an interpolated shell string.
- Preserve Windows-required environment variables under the existing scrubbed
  environment policy, including usable PATH, system, home, and temporary
  directory information. Handle environment-variable names case-insensitively
  on Windows, including collisions between ordinary and redacted values.

Git Bash can rewrite path-like arguments and environment values when calling
native programs. Test both real paths and literals that resemble paths; keep
any conversion controls scoped to the relevant invocation. A blanket global
conversion setting would change the behavior of otherwise working build tools.

### Paths and filesystem operations

At the start of the port, `EnvironmentPath` accepted only POSIX paths. Merely
replacing backslashes with slashes is insufficient: Windows paths have drive
and UNC roots, case behavior, aliases, reserved names, and reparse points.

The selected wire grammar, native conversion, equality rules and version
boundary are recorded under Contract package decisions below, including the
assessment of file URIs. Keep Windows-native display paths in model context;
protocol encoding need not be the syntax an agent types into a shell.

Required boundaries:

- Native environment paths, relative transfer paths, and Lightspeed VFS paths
  are distinct domains. Do not change VFS semantics to accommodate a Windows
  drive or expose Git Bash's `/c/...` spelling as a universal filesystem root.
- Convert shell path spellings only at the relevant shell boundary. Ordinary
  filesystem requests use the canonical environment path representation.
- Define handling of drive-absolute paths, UNC shares, drive-relative paths,
  extended/device namespaces, alternate data streams, reserved names, Unicode,
  trailing dots/spaces, and ambiguous case aliases. Unsupported forms must be
  refused explicitly; they must not bypass confinement through normalization.
- Preserve confinement under concurrent rename and junction/symlink changes.
  A string-prefix or canonicalize-then-open check is not a replacement for the
  current descriptor-relative guarantees. Choose native handle-based operations
  and file identity checks, and test races and state-directory aliases.
- Use native ACLs for private daemon state and identity material. Do not map
  Unix mode bits to a fictional Windows permission guarantee.
- Define file versions, executable metadata, link behavior, enumeration order,
  atomic writes/renames, and open-file sharing behavior. Advertise unsupported
  operations explicitly instead of substituting weaker behavior silently.
- Audit scan/search/glob, transfer journals, inventory names, publish/replace,
  and VFS/environment transfer boundaries. Restart recovery must interpret a
  journal using its recorded format, not the controller's host conventions.

These changes cross protocol and tool boundaries. Update protocol versions
where required, generated contracts and TypeScript consumers, schemas,
conformance fixtures, and all cwd/home/path renderers together.

### Process trees, jobs, and terminals

Keep the existing distinction: processes may leave services running for later
calls; jobs sweep descendants when their root finishes. Cancellation, timeout,
environment close, daemon restart, and natural root exit are separate events.
Write down the Windows behavior for each rather than applying kill-on-close
indiscriminately.

Use native Job Objects and owned process handles for tree membership and
termination. Establish membership before a child can spawn descendants; use
atomic job assignment or a suspended-spawn sequence as appropriate. Decide
breakaway and nested-job policy explicitly. If the promised containment cannot
be obtained, refuse execution instead of silently degrading to root-PID kill.

Replace the Unix-only foreground-group lookup with a Windows terminal path.
Reuse the existing `portable-pty` abstraction where it meets the contract;
review its Windows implementation before adding lower-level code. Validate
ConPTY resize, UTF-8 input/output, CR/LF normalization, EOF, Ctrl-C, and closing a
terminal with surviving children. Avoid shell-specific newline conversion in
ordinary byte pipes.

The protocol currently describes Unix signals and SIGTERM/SIGKILL escalation.
Specify semantic interrupt/terminate/kill behavior and capability reporting for
Windows. Do not invent Unix signals for native Windows exit codes or claim a
graceful interruption was delivered when only forced termination is available.
Preserve byte offsets, output draining, gaps, redaction, and settled-exit rules.

Implement Windows process identity and liveness for leftover tracking, idle
reporting, and restart reconciliation. Do not persist raw handles or trust a
reused numeric PID. Test detached descendants and daemon death against the
chosen ownership policy.

### Registration, installation, and enterprise use

Start with a foreground daemon using existing outbound registration, so a
managed machine can connect without a new inbound listener. Preserve TLS,
credential handling, and gateway behavior; integrate with the existing trust
configuration for enterprise certificate authorities.

Resolve home and private state directories for the actual execution identity.
Distinguish an interactive developer account from a service account; do not
assume Administrator, HOME, a logged-in desktop, or the developer's profile.
Filesystem confinement of API calls is not a sandbox for arbitrary child
processes. Initially use the operator-selected Windows account and document
that boundary; a Codex-like restricted-token sandbox is separate work unless
an agreed deployment requirement makes it necessary.

Before unattended release, choose service supervision and identity, graceful
shutdown, restart policy, and writable state locations. Add Windows artifact
naming and discovery, `.exe` extraction/validation, and a stop/replace/restart
or helper-based upgrade path with recovery. The current non-Unix automatic
re-exec is explicitly unsupported; do not present it as implemented. Keep
checksum and build/protocol verification. Review signing and offline
distribution requirements for enterprise rollout separately from the first
development executable.

## Contract package decisions

The contract package selects the following boundaries. It does not enable the
Windows filesystem or execution backends. The initial validation target
remains x86_64 Windows Server 2025; Windows support requires native backend
conformance evidence before being advertised.

### Paths and compatibility

Keep `EnvironmentPath` as a UTF-8 string with two disjoint grammars: existing
POSIX paths (`/workspace/src`) and uppercase drive-absolute Windows paths
(`C:/work/src`). File URIs were evaluated but not selected: they would require
re-encoding every existing POSIX path and introduce percent/authority decoding
without improving the handle-based confinement boundary. Native conversion
takes an explicit remote convention; it converts `c:\work` to `C:/work` and
renders it back as `C:\work`. It never resolves dot segments or strips device
prefixes. Literal `%` and `#` remain filename characters, not URI syntax.

Initially reject UNC, drive-relative, device/extended namespaces, alternate
streams, reserved device names, control characters and trailing dots/spaces.
Keep the existing 4096 UTF-8 byte ceiling. Equality and prefix operations are
lexical and case-sensitive; they are never proof of filesystem identity or
confinement. Windows handle/identity checks must account for case, short-name
aliases and reparse points. `TreePath` and VFS keep their existing domains;
Windows destinations must validate relative names before creating anything.

The data and registration version becomes **5**, with exact matching; the
controller remains **4**. Deploy matching runtime/gateway and daemon builds
together. Existing POSIX path spellings and stored POSIX values remain valid.
Generated data/registration schemas, OpenRPC, bootstrap/handshake fixtures,
release metadata and web demo daemon versions are aligned. No public API DTO
or workflow contract changes in this package.

### Discovery and shell policy

`initialize.platform` contains `os`, `pathConvention`, verified `shells`
(`kind`, absolute `executable`, observed `version`) and optional `defaultShell`.
An empty catalog means no shell was verified. Build-target strings are not
used for selection. Clients reject inconsistent path, shell and lifecycle
facts at initialization. The contract package initially reported an empty shell
catalog; package 4 now fills it with verified installation probes.

The selected model-facing shell contract is an optional enum: `bash`, `powerShell`,
`windowsPowerShell`, or `gitBash`. Omission selects the discovered default;
there is no arbitrary executable-path selector or error-triggered fallback.
On Windows prefer verified PowerShell 7, then Windows PowerShell 5.1; never
select Git Bash implicitly. Package 4 verifies a Git for Windows installation rather than
using an arbitrary `bash.exe` from PATH. Missing selections are typed errors.

Omitted `login` preserves Unix Bash's existing login behavior. PowerShell and
Git Bash default to no profiles; explicit `login: true` loads profiles.
PowerShell pipes use `-NonInteractive`; terminal requests omit it. Shell
commands remain one argv argument. The PowerShell adapter sets console/native
pipeline text to UTF-8 without BOM and preserves a failing native exit code;
parse failures are nonzero. File cmdlet encodings remain version-specific.
Direct argv requests never pass through a shell adapter. A separate CMD/batch
adapter is deferred and must be explicit and tested before it is supported.

### Ownership and termination

Process/job capabilities state their `tree` and `termination` model. Processes
also report separate `interrupt.pipe` and `interrupt.pty` flags. Missing
interrupt support is an error, never a forced kill presented as Ctrl-C.

| Event | Windows process | Windows job |
| --- | --- | --- |
| Natural root exit | Keep descendants and the Job Object handle; report leftovers and keep the environment active. | Sweep descendants before settled completion. |
| Timeout, terminate/cancel, kill or environment close | Terminate the owned tree. | Terminate the owned tree. |
| Connection loss | Continue under daemon ownership. | Continue under daemon ownership. |
| Daemon death or restart | Non-inheritable kill-on-close ownership terminates the tree; old process handles expire. | Terminate the tree; persisted queued/running jobs become interrupted. |

Attach a non-breakaway Job Object before execution; refuse spawn when nested
job restrictions or atomic containment cannot satisfy that promise. Keep its
handle independently of process output retention while descendants survive.
Do not persist handles or adopt raw PIDs after restart. The initial Windows
termination mode is `immediateKill`, with zero advertised grace. Advertise
terminal interruption only after ConPTY delivery works; pipe interruption is
initially unavailable. A terminal EOF request that cannot preserve the terminal
must fail explicitly. Existing POSIX group, grace and restart behavior stays
unchanged and continues to acknowledge that descendants can escape a group.

Windows exit status uses the existing `code` variant, interpreting the native
32-bit value as signed i32 (0xC0000005 is -1073741819), never inventing a signal.
Output offsets, draining and settled-exit rules retain their existing meaning.

### Remaining integration and evidence

Packages 2 and 3 implement the native backends and enforce these contracts;
package 4 adds shell probes, the selector and native cwd/home/model rendering.
Package 5 adds Windows transfer validation, native journal formats and recovery
as described below. Native
environment-variable handling must preserve required system/home/temp values
and reject case-insensitive collisions across ordinary and secret overrides.
Service identity, supervision and replacement upgrades remain package 6 work.

Contract tests cover drive roots, native conversion, unsupported forms, lexical
boundaries, shell preference/profile rules, inconsistent handshake facts and
lifecycle wire examples. Windows PowerShell 5.1 is exercised locally for native
exit codes, Unicode output and parse errors. Native filesystem confinement,
Job Objects, ConPTY and PowerShell 7 execution remain backend acceptance tests.

On the Windows development VM, the protocol and client suites passed 130 tests
and 4 doctests, and scoped Clippy passed with all targets and warnings denied.
At that contract-package checkpoint, the daemon check reported 27 errors and
one warning in the existing Unix-specific implementation. The full release metadata script could not run
because Node.js is absent; a direct check confirmed that both protocol versions
match `release/metadata.env`. Linux/macOS regression checks and the full CI
Clippy gate remain required before a Rust PR.

### Native backend implementation

The filesystem and execution packages are implemented together under envd
protocol 5, selected by the contract package. `filesystem.transfer` separates
core file access from transfers. Transfers were withheld at this backend
checkpoint and are enabled by the transfer package below.

The native filesystem uses handle-relative opens, retained ancestor handles,
reparse-point inspection and volume/file identity. It supports read/write,
metadata, enumeration, search, glob and scan. Atomic writes use private staging
and restore the existing target's DACL, or the inherited DACL for a new file.
State defaults to the execution account's local application-data directory;
state and identity keys have protected account/SYSTEM ACLs. Identity installation
is atomic without transient hard-link aliases, and existing key aliases fail.

Native execution attaches a non-breakaway Job Object during `CreateProcessW`,
before the child can execute. Pipes preserve raw bytes. ConPTY supplies terminal
input, UTF-8 output, Ctrl-C and internal resize. Normal process exit preserves
descendants and terminal ownership; job completion, explicit termination,
timeouts and daemon death remove the owned tree. Shutdown stops work even if
connection tasks retain the runtime. Persisted Windows PIDs are never adopted.
Pinned cwd paths are validated again after symlink resolution, so Win32 cannot
normalize a literal native name to a different directory at process creation.

Explicit limits for these packages:

- Transfers at this checkpoint were unavailable pending package 5.
- Cross-directory rename is unavailable: Windows reopens its destination for
  write access, conflicting with the ancestor pin that prevents junction
  replacement. Same-directory rename and atomic file replacement are supported.
- Windows executable metadata is false; setting executable bits is unsupported.
  Directory publication is atomic without a directory crash-durability promise.
- Pipe interruption and ConPTY input EOF are unavailable. Internal terminal
  resize is tested, but no public resize method is added.
- Direct native spawning accepts executables; scripts require an explicit
  interpreter. Shell discovery, model-facing commands and source-root discovery
  are integrated by package 4 below.

Validation on Windows Server 2025 passed 123 daemon unit tests and 5 client
round-trip tests, including the shared data conformance suite. The one ignored
test is a child-process helper invoked explicitly by the native execution tests.
Protocol/client suites passed 130 tests and 4 doctests; remote-adapter tests
passed 14 tests. Scoped Clippy passed for environment-daemon,
environment-client, environment-protocol and tools with `--all-targets --locked
-- -D warnings`. The native executable's `--print-build` reports
`x86_64-pc-windows-msvc` and protocol 5. Linux/macOS execution, the full workspace
Clippy gate and Windows release/service installation remain separate validation
requirements.

### Model-facing environment implementation

The daemon now probes shells once at startup with bounded, owned subprocesses.
PowerShell 7 is preferred over Windows PowerShell 5.1. Git Bash is accepted only
from a standard Git for Windows installation after checking both Git and Bash;
a PATH `bash.exe` is never used. Installing a shell requires a daemon restart to
refresh discovery. No WSL or Cygwin dependency is introduced.

Both model command surfaces use the discovered shell catalog and accept the
optional selector. Shell failures do not trigger fallback. PowerShell profile,
noninteractive, UTF-8 and native-exit behavior uses the contract package's pure
adapter. Direct program argv and durable jobs bypass shell selection.

Model-facing descriptions distinguish shell command text from direct argv.
Process/job argv help names the executable and separate arguments, with a short
explicit-shell example. Shell selectors refer to the environment context for
available shells and the default, keeping platform-specific guidance there.

File tools, apply-patch, process/job cwd and skill/prompt discovery resolve paths
using the remote convention. Windows drive paths and backslashes are accepted
without reinterpreting VFS paths. Active-environment context publishes native
cwd/home, OS, installed shells and versions, and execution capabilities through
the existing catalog mechanism. Discovery still does not wake environments.
Windows directory listing defaults to the working directory; logical POSIX/VFS
listing retains its root default.

The native integration test edits a Rust test through structured tools in a
directory containing spaces and Unicode, builds and runs it with the discovered
default shell, and runs the resulting executable directly and as a durable job.
Shared adapter tests exercise Windows shell and path facts independently of the
controller's compilation target. The skill scan test now uses the shared source
resolver on Windows as well as Unix.

Windows validation passed 348 tool unit tests, 124 daemon unit tests, six native
client round-trip tests, 131 protocol/client tests and five doctests. The
additional installation-dependent test passed explicitly with PowerShell 7.6.9,
Windows PowerShell 5.1 and Git for Windows Bash. The harness working-directory
validation vectors also passed. Scoped all-target Clippy passed with warnings
denied for the protocol, client, daemon, tools and harness crates.

The broader daemon suite exposed a liveness-observation issue: an exited Windows
process remains queryable while a handle survives. The creation-time observer
now polls the process handle before treating its identity as live, with a
regression test retaining a handle after process exit.

The envd protocol remains **5** and the controller protocol remains **4**.
A real Linux-hosted session against Windows and Linux/macOS regression execution remain acceptance checks;
the local native integration test does not establish those results. The hosted
runtime's native Windows check reaches existing `tokio::net::UnixStream` uses
in its gateway and cannot complete on this host. Porting that hosted runtime is
outside this plan's native-daemon scope.

### Transfer implementation

Windows now uses the shared capture, inventory, chunk upload, digest validation,
receipt and recovery engine and advertises `filesystem.transfer: true`.
Materialization publishes to a new destination with a same-parent native rename;
file transfers atomically replace files with `create`/`ifMatch` rechecked at
commit. Existing file DACLs are preserved; new file writes recover inherited
DACLs. Ancestor handles remain pinned throughout staging and publication.

Windows inventory admission rejects reserved/unsupported native names,
case-colliding paths within or across pages, and executable bits. A rejected
page leaves the inventory unchanged. Active publishers also compare destination
segments without case sensitivity. Logical VFS paths are unchanged.

Native journal records tag Unix bytes or Windows UTF-16LE, preserving native
segments without lossy conversion. Legacy Unix and foreign-format records are
quarantined on Windows. Interrupted operations fail on restart; a commit in
progress reports that the destination may already have been published.
Completed captures retain their inventory and receipt. Cleanup blocked by an
open Windows handle stays recorded, including after receipt expiry, and is
retried on sweeps or restart.

Explicit limits:

- Atomic replacement of an existing materialized tree remains unsupported on
  Windows and returns `capabilityUnavailable`; no delete-then-copy fallback is
  used. Publication also refuses a destination that appears before commit.
- Staged and newly materialized Windows trees have protected account/SYSTEM
  ACLs. File writes restore destination or inherited permissions at commit.
- Journal files are flushed before write-through replacement. Recovery covers
  daemon restarts, without promising directory durability after power loss.
- Cross-directory rename and Unix executable bits remain unsupported.

Shared conformance now exercises transfer capture, bounded content reads and
writes, stale file versions at commit, and receipt replay. Native transfer tests
cover Unicode/spaces, case collisions, invalid names, chunk retries and gaps,
digest mismatches, pinned ancestors, abort cleanup, foreign journal quarantine
and sharing violations during recovery. VFS materialize/capture and large binary
blob write/reference tests run on both Windows and Unix.

Windows validation passed 348 tool tests, 131 protocol/client tests and five
doctests, including generated-contract freshness checks. Scoped Clippy passed
for the protocol, client, daemon and tools crates with all targets and warnings
denied. Native integration passed eight tests, including shared data conformance,
VFS materialize/capture and binary blob transfers on all three tool surfaces.
The installation-dependent shell test remains separately exercised by package 4.

The broader daemon checks exposed an execution completion race: requesting
Windows job termination could return before owned process objects had finished
exiting. Job cleanup now retains process handles and waits for both process exit
and empty job accounting. Leftover reports verify membership and exclude exited
or reused PIDs. A regression test holds the process handle through termination;
the full parallel daemon unit suite then passed 143 tests.

The envd protocol remains **5** and the controller protocol remains **4**;
this package uses the existing wire contract. Linux/macOS regression execution,
Linux-hosted acceptance and distribution/service checks remain outstanding.

### Native Windows live protocol coverage

`crates/environment-daemon/tests/windows_live.rs` adds eight opt-in live tests.
Each launches the actual envd executable with a private temporary workspace and
state, connects over real loopback WebSockets, and kills/reaps only its owned
daemon. The listener reports its actual bound port so port-zero allocation is
race-free. The fixture uses canonical Windows paths, including spaces and
Unicode, and compiles a small dependency-free native program for argv, pipe,
terminal and process-tree observations. Retained process handles prevent PID
reuse from weakening termination assertions.

Coverage complements the existing unit and in-process round-trip suites:

- Full shared data conformance across a real executable and socket boundary.
- Installed PowerShell 7, Windows PowerShell 5.1 and Git Bash with Unicode,
  environment overrides, native cwd and explicit failure exit codes.
- Direct argv quoting and byte-exact binary stdin/output across disconnect and
  reconnection, plus ConPTY Unicode input, EOF refusal and interruption from a
  second connection.
- Real daemon death killing natural-exit descendants and a running durable job;
  restarted envd retains completed job output, interrupts unfinished jobs and
  refuses old interactive handles.
- Read-only file and capture access with mutation refused.
- Abrupt transfer interruption during inventory, partial upload and ready-to-
  publish staging, with failed receipts and cleanup verified after restart.
- Eight simultaneous publishers on separate sockets contending for a
  case-aliased destination; the winner resumes uploaded chunks after reconnect,
  and publication/capture receipts survive restart without replaying writes.

All eight passed on the Windows development VM in a parallel run (about twelve
seconds). PowerShell 7 was made available on the test process's PATH from its
portable installation. The existing ignored model-tool shell integration test
also passed, and all-target daemon Clippy passed with warnings denied. These
tests need no Temporal, PostgreSQL, gateway,
provider account or model credentials. See the
[testing guide](../documentation/development/testing-and-evaluation.md#run-live-tests)
for prerequisites and commands. Service installation, upgrades and a Linux-hosted
controller remain separate acceptance checks.

## Implementation packages

Land reviewable packages on the implementation branch. Keep this document's
Progress section current, and record protocol decisions before code relies on
them. A compiling binary alone is not acceptance for the port.

| Order | Package | Exit condition |
| --- | --- | --- |
| 1 | Platform, paths, shells, and lifecycle contract | Selected path representation, discovery shape, shell/profile policy, supported path forms, and process/job ownership semantics have explicit examples and tests; required protocol/schema changes are identified. |
| 2 | Native filesystem and configuration | Windows backend supports core read/write/list/search/glob and cwd/state handling; confinement and identity tests pass; transfer gaps are explicit. Unix behavior remains covered. |
| 3 | Native execution | Pipe and PTY commands run; tree cancellation, timeout, output settlement, normal-exit leftovers, idle reporting, and restart behavior satisfy the agreed contract. |
| 4 | Model-facing Windows environment | `exec_command` defaults to discovered PowerShell, optional Git Bash is explicit, context states Windows facts, structured file tools work, and a Linux-hosted agent can complete a Windows edit/build/test task. |
| 5 | Transfer and full conformance | Journals/recovery, transfer/publish, file versions and unsupported-capability handling pass on Windows; path-sensitive consumers and generated contracts are aligned. |
| 6 | Distribution and unattended operation | Windows artifacts are built/tested in CI; registration, account/state permissions, supervision, restart, and supported upgrade/manual-install flows are verified. |

Packages 2 and 3 may require small shared abstractions from package 1. Keep
incomplete Windows capabilities unavailable until their semantics work; do not
disable confinement or advertise no-op cleanup to get a demonstration running.
The first useful development milestone is packages 1–4 with honest capability
reporting. Production readiness also requires packages 5–6 and the agreed
deployment acceptance checks.

## Validation

Run scoped Rust tests and Windows builds as each backend lands. Unix-only
fixtures should be gated only when testing Unix-specific behavior; add Windows
equivalents for shared promises. The shared conformance suite currently runs
commands through `/bin/sh` and needs platform-aware fixtures, not wholesale
skipping on Windows.

Minimum acceptance coverage:

- Native PowerShell with Git Bash absent; PowerShell 7 and Windows PowerShell
  baseline; explicit Git Bash selection with present and missing installations.
- Direct execution and shell commands with spaces, quotes, Unicode, long
  arguments, multiline text, empty arguments, environment overrides and secrets.
- Files under paths with spaces and Unicode; drive/UNC rules; case aliases,
  junctions, symlinks, state exclusion, concurrent path replacement, and private
  state ACLs. Include cross-platform transfer collisions and rejected names.
- Nonzero exit codes, large/interleaved output, stdin/EOF, redaction, PTY input,
  resize and Ctrl-C; timeout/termination must account for grandchildren.
- Normal process exit with a surviving service, job completion with cleanup,
  environment close, daemon restart, PID reuse, and nested-job restrictions.
- A Windows-native Cargo or MSBuild build/test loop, and an end-to-end agent
  workflow controlled from Linux. No test may rely on WSL or Cygwin.
- Linux/macOS regression checks and exact CI Clippy before a Rust PR:
  `cargo clippy --workspace --all-targets --locked -- -D warnings`. If unrelated
  workspace crates cannot build on Windows, run that gate on its supported CI
  host and add scoped Windows gates; do not disable the existing gate.

After correctness, compare PowerShell and Git Bash using the same models and
tasks. Record success, retries, latency, and token use. Give both profiles
accurate OS/shell context. This evaluation can refine guidance and opt-in shell
selection without making Bash the default by anecdote.

## Windows implementation handoff

Continue on `windows-native-envd` in `C:\dev\lightspeed`. Confirm the branch and
worktree before editing; do not reset somebody else's changes. Start from the
contract package and keep the baseline compiler log for comparison.

```powershell
. C:\dev\Enter-Lightspeed.ps1
git status --short --branch
cargo build --locked --release -p environment-daemon --bin lightspeed-envd --target x86_64-pc-windows-msvc
```

Alternatively, `C:\dev\build-envd.ps1` runs the same build and overwrites the
build log. On success the artifact is
`target\x86_64-pc-windows-msvc\release\lightspeed-envd.exe`.

Use the Contract package decisions above for path grammar, versioning, shell
selection, profile semantics and tree ownership. Packages 2 through 4 must
implement and validate those contracts before enabling Windows capabilities.
Service identity and upgrade supervision remain required before unattended
release; no Windows version beyond Server 2025 is claimed yet.

## Progress

- [x] Native Windows development VM and Rust/MSVC toolchain prepared.
- [x] VS Code, rust-analyzer, and repository checkout available.
- [x] Baseline release build attempted; 27 source errors recorded.
- [x] Codex source and relevant official documentation inspected.
- [x] Native Windows / PowerShell default / optional Git Bash direction agreed.
- [x] Implementation branch created from fetched `origin/main`.
- [x] Cross-platform protocol and lifecycle decisions finalized and contract-tested.
- [x] Native filesystem, process, and terminal backends implemented and tested on Windows, with the explicit limitations above.
- [x] Model-facing shell selection and Windows context implemented, with native edit/build/test and job coverage.
- [ ] Real Linux-hosted session against Windows and Linux/macOS regression execution verified.
- [ ] Transfer/conformance, Windows CI, and unattended release completed.

## References

Source observations and documentation were reviewed on 2026-11-06. The Codex
checkout is evidence for design lessons, not a dependency to import wholesale.

[codex-shell-detect]: https://github.com/openai/codex/blob/40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f/codex-rs/shell-command/src/shell_detect.rs
[codex-shell]: https://github.com/openai/codex/blob/40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f/codex-rs/core/src/shell.rs
[codex-pty]: https://github.com/openai/codex/tree/40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f/codex-rs/utils/pty/src/win
[codex-input]: https://github.com/openai/codex/blob/40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f/codex-rs/utils/pty/src/windows_input.rs
[codex-job]: https://github.com/openai/codex/blob/40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f/codex-rs/utils/pty/src/win/job.rs
[codex-pty-attrs]: https://github.com/openai/codex/blob/40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f/codex-rs/utils/pty/src/win/procthreadattr.rs
[codex-pipe]: https://github.com/openai/codex/blob/40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f/codex-rs/utils/pty/src/pipe.rs
[codex-path]: https://github.com/openai/codex/blob/40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f/codex-rs/utils/path-uri/src/lib.rs
[codex-guidance]: https://github.com/openai/codex/blob/40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f/codex-rs/core/src/tools/handlers/shell_spec.rs
[codex-token]: https://github.com/openai/codex/blob/40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f/codex-rs/windows-sandbox-rs/src/token.rs
[codex-process]: https://github.com/openai/codex/blob/40eeb6e8a89ef421c25d4c40e06fa1d40ce66b4f/codex-rs/windows-sandbox-rs/src/process.rs
[openai-windows]: https://learn.chatgpt.com/docs/windows/windows-app#windows-subsystem-for-linux-wsl
[opencode-windows]: https://docs.opencode.ai/docs/windows-wsl/

Additional background: [MSYS2 argument/path conversion](https://www.msys2.org/docs/filesystem-paths/)
explains why shell path translation needs an explicit boundary. Rust's
[Cygwin target documentation](https://doc.rust-lang.org/rustc/platform-support/x86_64-pc-cygwin.html)
describes a separate Tier 3 target without precompiled artifacts; it is not a
shortcut for the selected native Windows implementation.

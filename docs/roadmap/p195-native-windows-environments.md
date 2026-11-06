# P195 — Native Windows environments

**Status:** Design agreed in discussion on 2026-11-06; implementation not
started. The platform and shell choices below are decisions. Proposed protocol
shapes and the explicitly listed open questions need resolution in the first
implementation package.

**Branch:** `windows-native-envd`, created from the latest fetched `origin/main`
at `e6558e3102ec1d67b76a50265aa457902b1bf8ba`.

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

The current `EnvironmentPath` contract is POSIX-specific. Merely replacing
backslashes with slashes is insufficient: Windows paths have drive and UNC
roots, case behavior, aliases, reserved names, and reparse points.

The first package must settle a typed cross-platform representation. Codex's
file-URI approach is the preferred design candidate to evaluate, because it
keeps remote paths independent of the controller host. Record the selected
wire grammar, native conversion, equality rules, and compatibility/versioning
decision before implementing it. Keep Windows-native display paths in model
context; protocol encoding need not be the syntax an agent types into a shell.

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

Resolve these details in package 1: the path wire format and version boundary;
supported UNC/device/alternate-stream forms; PowerShell profile semantics;
shell-selection configuration and its model-facing schema; tree ownership
after daemon death; graceful-interrupt capabilities; and the minimum Windows
versions to claim beyond the tested Server 2025 development machine. Service
identity and upgrade supervision must be settled before unattended release.

## Progress

- [x] Native Windows development VM and Rust/MSVC toolchain prepared.
- [x] VS Code, rust-analyzer, and repository checkout available.
- [x] Baseline release build attempted; 27 source errors recorded.
- [x] Codex source and relevant official documentation inspected.
- [x] Native Windows / PowerShell default / optional Git Bash direction agreed.
- [x] Implementation branch created from fetched `origin/main`.
- [ ] Cross-platform protocol and lifecycle decisions finalized.
- [ ] Native filesystem, process, and terminal implementations completed.
- [ ] Model-facing shell selection and Windows context completed.
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

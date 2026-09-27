# Lightspeed Rust maintenance audit — 2026-09-13

The Rust count is materially inflated by tests: **150,726 non-test code lines and 106,960 test/test-support code lines**. The audit found a concentrated set of complex functions and several worthwhile consolidation opportunities. It did **not** establish that the repository needs a broad rewrite or that a large fraction of production code can be deleted.

The strongest initial changes are shared live-test setup, shared daemon redaction helpers, and shared native MCP inventory policy. Large command/state-machine functions deserve targeted tests and careful decomposition, not an automatic “extract until the score passes” refactor.

This was a read-only repository audit. No source changes were made. Static analysis used a snapshot of 521 tracked Rust files, including existing staged/unstaged changes. The Rust files still matched the snapshot after the coverage runs. Git revision and initial worktree status are recorded in `git-head.txt` and `git-status.txt`.

## Size breakdown

| Category | Rust code lines | Share |
|---|---:|---:|
| Non-test source | 150,726 | 58.5% |
| Inline test-only source | 62,456 | 24.2% |
| Separate test files/modules | 41,986 | 16.3% |
| `test-support` crate | 2,518 | 1.0% |
| **Total** | **257,686** | **100%** |

“Non-test” is a source classification, not a measurement of runtime reachability or CPU usage. It includes ordinary library code, CLI/eval code, build scripts, public conformance helpers, and in-memory implementations compiled without `cfg(test)`. It includes OS-specific branches regardless of the current platform. No generated Rust files were identified by the header/path classification; macro-generated code is not expanded or counted. Generated JSON/TypeScript artifacts are outside this Rust audit.

All 521 per-file category sums reconcile exactly with the baseline `cloc` code count. Whole-repository code count at capture was 481,009 across languages; that number includes documentation/data categories understood by cloc.

| Crate | Non-test | Tests and support |
|---|---:|---:|
| temporal-server | 33,812 | 29,897 |
| engine | 16,344 | 10,545 |
| tools | 15,390 | 8,270 |
| cli | 12,233 | 3,872 |
| temporal-workflow | 11,875 | 5,563 |
| store-pg | 10,064 | 5,469 |
| environment-daemon | 7,470 | 3,584 |
| api | 7,096 | 3,913 |
| llm-runtime | 5,700 | 16,034 |

The most misleading whole-file count is [engine/core/drive.rs](/Users/lukas/dev/lightspeed/crates/engine/src/core/drive.rs): 8,081 code lines become **1,968 non-test code lines** after excluding its test module. By contrast, [gateway/service/mod.rs](/Users/lukas/dev/lightspeed/crates/temporal-server/src/gateway/service/mod.rs) remains **4,365 non-test code lines**.

Complete crate totals, the top 20 complex functions, the largest production files, and file churn are in [inventory.md](inventory.md). Raw results are in `summary.json` and `production-functions.json`.

## Complexity and maintenance hotspots

The current Rust parser identified **6,313 named non-test function definitions**. All matched a rust-code-analysis result. Anonymous closures are not ranked separately; complexity of nested closures is included in the containing function's aggregate.

- Median function PLOC: 10; 95th percentile: 62; 99th percentile: 118.
- Median cognitive complexity: 0; 95th percentile: 9; 99th percentile: 20.
- 30 named functions have cognitive complexity above 25; 107 have PLOC above 100.

These are descriptive thresholds, not pass/fail rules. PLOC is rust-code-analysis's physical instruction-line measure and does not exactly equal cloc code lines. Large enum dispatchers naturally have many branches. Review the responsibilities and invariants behind the numbers.

| Function / location | PLOC | Cognitive | Cyclomatic | Containing-file changes in 90 days |
|---|---:|---:|---:|---:|
| [scan](/Users/lukas/dev/lightspeed/crates/environment-daemon/src/filesystem/scan.rs:17) | 289 | 191 | 115 | 2 |
| [Operation::execute](/Users/lukas/dev/lightspeed/crates/environment-daemon/src/filesystem/transfer/session.rs:720) | 349 | 134 | 114 | 1 |
| [admit_command](/Users/lukas/dev/lightspeed/crates/engine/src/core/admit.rs:15) | 820 | 130 | 164 | 15 |
| [tool_call_completed_proposals](/Users/lukas/dev/lightspeed/crates/engine/src/core/drive.rs:1638) | 327 | 107 | 56 | 35 |
| [Scanner::advance](/Users/lukas/dev/lightspeed/crates/environment-daemon/src/filesystem/transfer/session.rs:102) | 137 | 69 | 41 | 1 |
| [TransferManager::execute](/Users/lukas/dev/lightspeed/crates/environment-daemon/src/filesystem/transfer/session.rs:409) | 253 | 67 | 82 | 1 |
| [visible_mcp_result](/Users/lukas/dev/lightspeed/crates/temporal-server/src/worker/mcp.rs:962) | 170 | 48 | 31 | 6 |
| [invoke_batch](/Users/lukas/dev/lightspeed/crates/temporal-server/src/worker/session_tools.rs:1435) | 233 | 38 | 59 | 54 |
| [start_session_internal](/Users/lukas/dev/lightspeed/crates/temporal-server/src/gateway/service/mod.rs:1071) | 208 | 32 | 81 | 68 |
| [project_event_kind](/Users/lukas/dev/lightspeed/crates/api-projection/src/lib.rs:658) | 450 | 29 | 91 | 41 |

**Maintenance priority:** gateway lifecycle handling, session tool dispatch, and core tool-result/admission handling combine substantive responsibilities with frequent containing-file changes. Filesystem scanning/transfer has the highest static complexity even though its current files have little recorded churn.

Churn is the count of non-merge commits touching a file, including test-only changes, formatting, and initial additions. It does not mean the named function changed that many times. Rename history is not reconstructed; new/renamed paths can look artificially quiet, including the in-progress environment refactor. Uncommitted changes are in the source snapshot but are not additional commits in these counts. This is why no single blended “health score” is used.

## Duplication results

Scans used jscpd 5.2.0, Rust only, weak mode (comments ignored), minimum 100 tokens and 15 physical lines. Production and tests were scanned separately.

| Scan | Reported clone matches | Tool-reported duplicated tokens |
|---|---:|---:|
| Production, exact token matches | 77 | 12,828 / 1,006,031 = 1.28% |
| Tests and support, exact token matches | 173 | 32,216 / 717,487 = 4.49% |
| Production, identifiers normalized | 453 | 77,724 / 1,006,031 = 7.73% |

These ratios are detector statistics, **not percentages of safely removable code**. Matches can overlap, include boilerplate, and cross function boundaries. Normalizing identifiers produces useful candidates but also makes unrelated typed forwarding methods look alike. Literal values were not normalized. Semantic duplication can remain undetected.

Masked files preserve original line positions using whitespace. Consequently, jscpd's physical-line denominator includes padding and its reported line-duplication percentages should not be used. The token ratios above avoid that particular denominator problem. Minimum-line thresholds can also be affected by internal gaps; the findings below were inspected in original source.

### Reviewed clusters

1. **Live-test environment/client setup — high confidence.** There are 13 `dotenv_var` definitions under `llm-runtime/tests`, including an existing shared implementation in [support/mod.rs](/Users/lukas/dev/lightspeed/crates/llm-runtime/tests/support/mod.rs:66). Exact matches between Responses caching/prompts/skills setup extend to approximately 169–170 physical lines. The copies have already diverged: [the skills copy](/Users/lukas/dev/lightspeed/crates/llm-runtime/tests/openai_responses_skills_live.rs:75) uses `split_once('=')?`, so a nonempty noncomment line without `=` ends lookup before subsequent keys. The shared helper explicitly skips that line. Some copies also strip quotes differently. This is observed control-flow divergence; no real `.env` was read to investigate it.
2. **Daemon secret-redaction helpers — high confidence.** [jobs.rs](/Users/lukas/dev/lightspeed/crates/environment-daemon/src/jobs.rs:946) and [process.rs](/Users/lukas/dev/lightspeed/crates/environment-daemon/src/process.rs:1122) repeat `redactions_for_secret_env`, `redact_bytes`, and `find_subslice`. The detector found a 32-line/246-token match. Both callers live in one crate and use the same algorithm. Sharing a private helper would make future fixes consistent without unifying process/job lifecycle logic.
3. **Native MCP inventory policy — high confidence.** [Responses](/Users/lukas/dev/lightspeed/crates/llm-runtime/src/openai_responses.rs:588), [Completions](/Users/lukas/dev/lightspeed/crates/llm-runtime/src/openai_completions.rs:777), and [Anthropic](/Users/lukas/dev/lightspeed/crates/llm-runtime/src/anthropic_messages.rs:914) repeat inventory retrieval, ordering, exposed-name filtering, omitted-name logging, and per-request cap accounting. These are shared policy operations. The construction of native provider request types afterward should remain separate.
4. **Worker universe resolution — high confidence, small extraction.** [bots.rs](/Users/lukas/dev/lightspeed/crates/temporal-server/src/worker/bots.rs:45) and [channels.rs](/Users/lukas/dev/lightspeed/crates/temporal-server/src/worker/channels.rs:41) repeat the fixed-versus-runtime universe resolver, wrong-universe rejection, and retryability mapping. The normalized detector reports a much larger 156-line cluster that also includes activity wrappers. Share the resolver, not the role-specific activity transport.
5. **ID macros — real duplication, lower priority.** The two `string_id!` definitions in [engine/session/ids.rs](/Users/lukas/dev/lightspeed/crates/engine/src/session/ids.rs:6) and [engine/core/components/ids.rs](/Users/lukas/dev/lightspeed/crates/engine/src/core/components/ids.rs:9) account for the largest exact production match, about 80 lines. A private engine-local macro could serve both. Similar macros across auth/MCP/VFS/domain crates are less compelling: adding a cross-domain dependency solely to eliminate wrappers may make the architecture worse.
6. **Prompt/skill VFS root resolution — mixed.** [prompts/vfs.rs](/Users/lukas/dev/lightspeed/crates/tools/src/prompts/vfs.rs:223) and [skills/vfs.rs](/Users/lukas/dev/lightspeed/crates/tools/src/skills/vfs.rs:222) share path-derived IDs, duplicate-ID validation, attachment selection, and snapshot/workspace inspection. However, prompt revision tracking and skill trust/scope differ. Consider sharing attachment/path primitives; do not create one generic prompt/skill subsystem just because the source looks similar.
7. **Rust API service/client forwarding — mostly mechanical.** Large identifier-normalized matches in [api/service.rs](/Users/lukas/dev/lightspeed/crates/api/src/service.rs:84) and [cli/api_client.rs](/Users/lukas/dev/lightspeed/crates/cli/src/api_client.rs:145) include regular typed method wrappers, sometimes overlapping within one file. These could be candidates for existing-manifest-driven generation in a separate design review, but are not evidence of duplicated business policy.
8. **OpenAI client configuration — plausible small shared helper.** Audio, Completions, and Responses clients repeat environment overrides and organization/project header construction. Endpoint defaults and request types differ. A private OpenAI header/config utility is more appropriate than merging clients.

Raw browsable reports: [production clones](dup-production/jscpd-report.html), [test clones](dup-tests/jscpd-report.html). Normalized matches are in `dup-renamed/jscpd-report.json`.

## Proposed changes, in practical order

| Change | Concrete benefit | Risk / validation |
|---|---|---|
| Consolidate live-test dotenv/model/client setup into existing support modules | Remove large copied setup blocks and the observed parser divergence | Low–medium. Preserve each suite's variable precedence/defaults; add offline parser/client-config tests; compile live targets without running them. |
| Share daemon redaction helpers privately | One implementation for both jobs and processes | Low for extraction. Existing daemon unit tests plus explicit byte-level cases; preserve current streaming/chunk semantics. This audit does not certify the redaction algorithm against all leak cases. |
| Centralize native MCP inventory policy in llm-runtime | One place for naming, filtering, caps, and error mapping | Medium. Test ordering, invalid names, collisions, cumulative caps across servers, and unchanged native provider output. |
| Share worker universe resolution | One place for universe admission and retryability mapping | Low–medium. Test wrong fixed universe, unknown runtime universe, transient runtime failure; preserve separate role queues and activities. |
| Classify calls once and consolidate common dispatch inside `invoke_batch` | Remove duplicated denial/workflow/concurrency/control dispatch in the fast and generic paths | Medium. Preserve lazy VFS/environment setup, await paths, sibling promise accounting, ordering, and cleanup. Add path-parity tests before moving code. |
| Decompose `admit_command` by command family | Reduce the 820-PLOC function's review surface while retaining a visible exhaustive dispatcher | Medium–high. Cover uncovered rejection/idempotency paths first; preserve event order and replay behavior. Extraction may not reduce total LOC. |
| Give tool-result completion a small explicit per-batch accumulator and effect handlers | Localize promise, workflow emission, environment selection, and join handling | High. Shared state currently enforces cross-call invariants; preserve duplicate-ID detection, exclusivity across per-call resumes, and exact event ordering with replay vectors. |
| Separate filesystem scan policy/budget accounting from traversal; split transfer request handlers by operation | Make the most deeply nested I/O logic auditable | High. Preserve anchored path confinement, symlink races, quotas, retry identity, validate-before-mutate behavior, and staging/publication boundaries. Avoid a new generic filesystem framework. |
| Move gateway session lifecycle implementation into a focused module, then simplify start/retry/setup handling | Reduce navigation and review burden in the 4,365-line gateway service module | Medium. A file move alone is organizational; substantive simplification must preserve workflow start retry and managed-binding validation semantics. |

I would start with the first two changes and measure the result. The native MCP policy extraction is the first production change likely to improve maintenance beyond a small helper. I would not start by rewriting the core state machine or imposing a global LOC/complexity gate.

Additional inspection: `visible_mcp_result` repeats admitted/omitted media rendering for image/audio versus resource blocks, and could use one private admission/rendering helper. `project_event_kind` is large mainly because it exhaustively translates an event vocabulary; splitting by event family can aid navigation, but replacing typed mappings with generic JSON machinery would sacrifice useful compiler checks. Neither is a reason to chase code deletion for its own sake.

## Focused coverage and tests

Executed on the current macOS toolchain, default features, with an isolated coverage target directory:

- `engine --lib`: **225 passed**.
- `environment-daemon --lib`: **78 passed**.
- `llm-runtime --lib`: **146 passed**.
- **449 passed total; no failed or ignored tests in these selected targets.**

No credentialed/live integration suites were run. The daemon's local unit tests exercised local filesystem/process behavior; they did not require the development Temporal/PostgreSQL services.

The table below combines each named function with instrumented regions inside its source span, including nested closures/async bodies. Identical source-coordinate regions across instantiations are merged and considered covered if any execution covers them. This is an audit-derived source-region metric, not branch coverage or the raw cargo-llvm-cov whole-crate summary.

| Function | Covered source regions | Percentage |
|---|---:|---:|
| `admit_command` | 563 / 768 | 73.3% |
| `tool_call_completed_proposals` | 296 / 336 | 88.1% |
| filesystem `scan` | 400 / 471 | 84.9% |
| transfer `Operation::execute` | 416 / 496 | 83.9% |
| transfer `TransferManager::execute` | 244 / 331 | 73.7% |
| transfer `Scanner::advance` | 162 / 202 | 80.2% |
| Anthropic `materialize_tools` | 128 / 146 | 87.7% |

Earlier progress figures of 76%/89% for engine admission/completion referred to the direct function body regions only. The final table also includes nested closures, matching the inclusive complexity ranking.

These numbers do not include tests from all dependent crates or live suites, and unexecuted regions are not proof of dead code. They establish a useful local baseline for refactoring. They do not establish assertion quality. Test code remains visible in the stock HTML coverage reports; do not interpret their overall percentages as production-only coverage.

[Engine coverage HTML](coverage-engine-html/html/index.html) · [Daemon/runtime coverage HTML](coverage-runtime-daemon-html/html/index.html). Raw exports and the source-span aggregation script are included.

## Reproduction and limitations

Tools: cloc 2.06; Rust 1.97.1; tree-sitter Python 0.25.2 with tree-sitter-rust 0.24.2; rust-code-analysis-cli 0.0.25; jscpd 5.2.0; cargo-llvm-cov 0.9.1. Tooling was installed outside the repository. The matching `llvm-tools-preview` component was installed for coverage.

`audit.py` snapshots tracked `.rs` files, marks `cfg(test)`/test-annotated items, handles the repository's `cfg(all(test, ...))` form, follows test-only out-of-line modules, and writes complementary whitespace-masked files preserving original byte/line positions. It uses the current source tree rather than Cargo macro expansion. The current parser reported no Rust syntax errors and no unresolved test modules. There were no relevant inner `cfg(test)` or more complex conditional test expressions requiring evaluation in this source set. Future syntax/configuration patterns may require extending the classifier.

The older parser bundled with rust-code-analysis reported five recoverable errors around `?` syntax in `crates/vfs/src/snapshot.rs`; its metrics for that file are advisory. None of the highlighted hotspots is in that file. The newer parser used for the size split parsed all files successfully. `rca-parse-errors.txt` records the older parser's warnings.

`reproduce-static.sh` installs pinned tooling into this audit directory if needed and regenerates static data. Run it from a shell with Cargo, uv, Node/npm, and cloc available. It overwrites the generated snapshot/split/complexity subdirectories of this audit directory. It does not modify the repository. The hand-reviewed narrative in this report is specific to this snapshot and is not automatically refreshed.

Coverage commands are recorded in `reproduce-coverage.sh`; they run only the selected unit-test targets, regenerate JSON and HTML, and write to this audit directory. They deliberately do not source `.env` or run ignored tests. These are optional separate steps from static analysis.

This audit did not execute CPU profiling, inspect production telemetry, prove dead-code reachability, analyze every abstraction's value, or perform an exhaustive security/correctness review. Its output is a prioritized maintenance shortlist backed by measurements and source inspection.

Tool references: [rust-code-analysis metrics](https://mozilla.github.io/rust-code-analysis/metrics.html), [jscpd](https://github.com/kucherenko/jscpd), [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov).

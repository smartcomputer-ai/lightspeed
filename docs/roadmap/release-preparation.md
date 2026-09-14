# Release preparation

## Scope

Prepare v0.3.0 from current main and make subsequent stable version bumps
repeatable without changing independent compatibility boundaries.

## Implementation

- [x] Make the four released executable crates and `release-info` inherit one
  Rust workspace product version.
- [x] Add `npm run release:prepare -- <version>` to synchronize the workspace
  version, release metadata, and Cargo lockfile, restoring edited files if
  preparation fails.
- [x] Verify resolved Cargo versions, workspace inheritance, and lockfile
  consistency alongside the existing schema, protocol, and toolchain checks.
- [x] Reject prereleases until npm and GitHub publication have a separate
  prerelease channel.
- [x] Prepare v0.3.0 notes and document the coordinated upgrade, configuration
  conversion, fresh session histories, and explicit daemon update requirements.
- [x] Complete focused release-script, build identity, and documentation checks.

## Validation

All 16 CI/release-script tests pass, including five preparation tests covering
version validation, workspace and lockfile updates, idempotence, stale lockfile
rejection, and rollback. Metadata verification and workflow YAML/shell syntax
checks pass. The documentation checks pass nine adapter tests, Astro diagnostics,
and a static build validating 46 published HTML/Markdown pages and their links.
The native server builds and reports `lightspeed-server 0.3.0`; the `release-info`
crate check also passes. No live suites, database migration, Linux release image
build, tag push, or publication was performed during preparation.

Publication remains the existing annotated-tag workflow after the preparation
change is merged through CI. A tag push can publish automatically unless the
`official-release` environment has approval rules configured. This work does
not add a deployment or change running services.

## Follow-up opportunities

Snapshot metadata verification now explicitly installs the Rust toolchain named
by release metadata and Node 24, then fetches locked Cargo dependencies before
the offline check. The first snapshot after version preparation failed with
`spawnSync cargo ENOENT`: the host preflight previously needed only Node and
shell tools, while Rust builds happened inside Docker. PR/main CI already set
up Cargo and therefore did not expose the missing snapshot prerequisites.
The fetch also makes the check independent of a previous job's registry cache.
Validation: `cargo fetch --locked` followed by metadata verification passes with
an isolated, initially empty `CARGO_HOME`; workflow YAML/shell syntax and
whitespace checks pass.

The first two tagged v0.3.0 builds failed before artifact staging: the inline
transfer dispatch test exceeded its one-second deadline on the release runner.
The test now uses the existing 30-second protocol ceiling, preserving its
dispatch, byte round-trip, and read-only assertions. A separate deterministic
test backdates an operation's start time and verifies that expiry rejects both
progress and publication without replacing the destination. Production timeout
limits and enforcement are unchanged.
Validation: all 81 native macOS daemon library tests pass, including the dispatch
and deterministic expiry tests; formatting and whitespace checks pass.

Toolchain and Node pins still appear in multiple workflows and the build
environment. Consolidate those separately from product versioning. If preview
releases become necessary, add explicit npm dist-tag and GitHub prerelease
handling before accepting prerelease versions. Product versions, API and
environment protocols, and database revisions must remain independent inputs.

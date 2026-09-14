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

Toolchain and Node pins still appear in multiple workflows and the build
environment. Consolidate those separately from product versioning. If preview
releases become necessary, add explicit npm dist-tag and GitHub prerelease
handling before accepting prerelease versions. Product versions, API and
environment protocols, and database revisions must remain independent inputs.

# Contributing and releasing

A contribution should make the changed behavior understandable, keep its
contracts and documentation aligned, and provide evidence that it works. A
release carries those changes as one coherent set of runtime, client,
connector, and documentation artifacts.

Start with [Local development](local-development.md) to work from a checkout.
This page covers preparing the contribution and how the repository turns a
tested source revision into published components. Installing those components
is covered by [Self-host Lightspeed](../deployment/self-hosting.md).

## Prepare a contribution

Follow the repository's [contribution policy](../../../CONTRIBUTING.md): fork
the repository, make a topic branch, include appropriate tests or replay
vectors, and open a pull request that explains the change. Keep unrelated
edits out of the contribution so a reviewer can follow the behavior and its
evidence together.

Before implementation, identify the owning layer. The
[architecture guide](../how-it-works/architecture.md) explains why the engine
decides deterministically, adapters perform I/O, clients depend on the public
API, and compute remains separate from VFS. The short repository rules in
[`AGENTS.md`](../../../AGENTS.md) preserve those boundaries. A new provider
request belongs at the provider boundary; a durable agent decision needs
recorded facts the reducer can replay.

Use the smallest test that demonstrates the changed behavior, then widen
validation as needed. A wire change needs generated consumers; a durable
decision needs replay coverage; SQL needs migration validation. Follow
[Testing and evaluation](testing-and-evaluation.md) and
[Changing contracts](changing-contracts.md) for the corresponding commands.
Update the relevant manual page when a user, integrator, or operator needs to
understand new behavior.

The contribution policy requires a DCO sign-off on each commit:

```bash
git commit -s
```

Read the linked policy before signing off. This adds the `Signed-off-by` line;
it is separate from cryptographic commit signing.

In the pull request, lead with the problem and the resulting behavior. Include
the compatibility effects a reviewer needs to assess and the checks you ran.
State relevant checks that remain unrun, such as a provider-backed test that
needs credentials. For a bug fix, a short before/after example and the test
that reproduces it are often more useful than a file-by-file account of the
implementation.

## Understand the CI gate

The [CI workflow](../../../.github/workflows/ci.yml) runs on pull requests,
pushes to `main`, and manual dispatch. Its change classifier selects the work
that applies to the changed paths:

| Change | Selected checks |
| --- | --- |
| Rust implementation | Formatting, Clippy, workspace tests, release metadata, and a live PostgreSQL migration-ledger test. |
| TypeScript consumers or public API inputs | Generated consumers, typechecks, tests and builds, Platform migration validation, and release packaging checks. API changes also select Rust checks. |
| Published manual, site, assets, or imported references | Documentation adapter tests, Astro diagnostics, and the static build with link and asset validation. |
| Root npm dependency files | Consumer and documentation checks. |
| Build, release, workflow, or unclassified inputs | All suites. |
| Internal prose | The lightweight aggregate gate. |

The `required` job collects the selected results; a selected job's failure or
cancellation fails that gate. The authoritative path rules are in
[`classify-changes.mjs`](../../../scripts/ci/classify-changes.mjs). CI itself
publishes nothing.

Passing a focused local test does not reproduce every selected CI job.
Conversely, a documentation-only change doesn't need a live model call to
establish that its links and examples build. Use the
[testing guide](testing-and-evaluation.md#check-contracts-and-the-wider-build)
to reproduce the relevant checks and their prerequisites.

## Keep release identity and compatibility together

[`release/metadata.env`](../../../release/metadata.env) supplies product
version and compatibility metadata for the build. It records the Rust build
inputs, API and environment protocol versions, and both database schema
boundaries. Validate changes to those values with:

```bash
scripts/release/verify-metadata.sh
```

The check compares the metadata with its owners: executable package versions,
the pinned toolchain and build image, protocol constants, runtime schema
revision, and Platform migration journal and supported baseline. These
values have different meanings. A product version bump does not itself
migrate a database or make an older daemon speak a new protocol.

Prepare a stable release from freshly fetched `origin/main` on a dedicated
branch:

```bash
git fetch origin main
git switch -c release/0.3.0 origin/main
npm run release:prepare -- 0.3.0
scripts/release/verify-metadata.sh
```

The preparation command updates `workspace.package.version` in `Cargo.toml`,
`LIGHTSPEED_PRODUCT_VERSION` in `release/metadata.env`, and the Cargo lockfile.
The four executable packages and `release-info` inherit the workspace version;
internal library versions remain independent. Cargo refreshes workspace package
versions offline while preserving locked external dependencies. If preparation
fails, the command restores the edited files. Run `npm ci` and `cargo fetch
--locked` first if dependencies are not cached locally.

Write `release/notes/v<version>.md`, including breaking changes and the supported
upgrade procedure. Review the diff and merge the preparation branch through CI.
The command does not commit, tag, publish, change compatibility revisions, or
generate release notes automatically.

Release staging sets the client and Configurator package versions and embeds
the source commit in the client's `release.json`. Their package versions in
the checkout are placeholders, so don't update every package version as a
substitute for following the release metadata and staging process.

The migration authoring steps are in
[Changing contracts](changing-contracts.md#database-migrations). For a release
that affects retained state, also document the operator's procedure in
[Upgrades and recovery](../deployment/upgrades-and-recovery.md).

## Build the release locally

Local packaging uses Docker and the checked-in release build environment.
Use a clean, dedicated checkout when validating a release: the build mounts
the checkout, replaces `dist/`, and runs dependency installs there. A dirty
checkout can otherwise package local edits while reporting the current HEAD
as its source identity.

The Makefile provides three entry points:

```bash
make release-dist
make release-images
```

`make release-dist` builds the Linux amd64 build-environment image and runs
the packaging script inside it, recording its exact image identity. Rust
builds the runtime, Incus provider, and CLI for the GNU target, then builds
`lightspeed-envd` separately for the static musl target. The build also
compiles the TypeScript client, Configurator, product web UI, demo, and docs;
stages runtime dependencies; and produces the manifest, SBOM, and checksums.

`make release-images` requires those staged outputs. It copies them into the
four component images and runs basic identity/import smoke checks. It does
not recompile Rust or rebuild the UI. `make release` runs both targets in
order.

Official images and executable archives target Linux amd64 today. Local
image builds use the Docker daemon's default platform, so use a Linux amd64
build environment for the complete image workflow. The separate macOS
workflow checks native Apple Silicon compilation and executable identity;
it does not publish macOS archives. Ordinary macOS development uses the
source workflow in [Local development](local-development.md).

The outputs have distinct jobs:

| Output | Purpose |
| --- | --- |
| `dist/bin/` and `dist/archives/` | Four Linux executables and their archives, plus the static demo and docs archives. |
| `dist/npm/` | The publishable `@lightspeed-ai/agent-client` package. |
| `dist/contracts/` | Generated public API contracts. |
| `dist/configurator-mcp/` and `dist/runtime/` | Staged inputs for component images. |
| `dist/release-manifest.json` and `dist/envd.json` | Release identity, compatibility and component references, plus daemon discovery metadata. |
| `dist/checksums.txt` and `dist/sbom.spdx.json` | Artifact hashes and the dependency inventory. |

The images are `runtime`, `configurator-mcp`, `platform`, and
`platform-workers`. The last name is retained for compatibility; it contains
the connector host. Bots and Channels core run in the Rust `runtime` image.
The demo and documentation site are separate static archives served under
`/demo/` and `/docs/`.

A local build verifies construction, but it does not publish an official
release. Publication fills in component references and digests that can be
absent from a local manifest. The
[release implementation guide](../../releasing.md) covers the packaging and
publication details, and the
[docs serving contract](../../site/README.md#content-negotiation-at-deployment)
covers the static site's HTML and Markdown outputs.

## Main snapshots

A successful CI run for a push to this repository's `main` triggers
[`snapshot-main.yml`](../../../.github/workflows/snapshot-main.yml). It
checks out that tested SHA, confirms that it is still the head of `main`, and
builds a complete artifact set. Documentation-only main changes also produce
a complete snapshot with the updated manual.

Components are first published under run-specific staging names. The
workflow records their exact digests, completes the manifest and bundle, and
checks the packaged components, including image smoke tests against
PostgreSQL. It checks `main` again before exposing the public snapshot alias:
`release-bundle:sha-<full-sha>`.

The bundle's manifest supplies the component digests. Pin that coherent set
instead of choosing independently moving component references. A superseded
run can leave staged objects without a completed public snapshot; the
finished bundle is the publication boundary a consumer should use.

## Tagged releases

Pushing a `v*` tag triggers
[`release-tag.yml`](../../../.github/workflows/release-tag.yml). The workflow
requires an annotated tag, a valid version matching
`LIGHTSPEED_PRODUCT_VERSION`, and a target commit reachable from `main`.
The target can be an earlier commit on `main`; it need not be the current
head. Annotation is checked, but cryptographic tag signatures are not
validated by this workflow.

After the preparation change has merged and CI passes, tag its exact merged
commit with the reviewed notes. For example, in a clean checkout of that commit:

```bash
scripts/release/verify-metadata.sh
git tag -a v0.3.0 -F release/notes/v0.3.0.md
git push origin v0.3.0
```

Pushing the tag starts publication. The protected Linux amd64 `hz01` runner
must be online, GHCR publishing must be enabled, and the `official-release`
environment must contain `NPM_TOKEN`. The environment only pauses publication
if approval rules have actually been configured in GitHub.

The release workflow independently tests and builds the tagged source. It
does not promote an already-built main snapshot. Final publication runs
through the `official-release` environment, validates the staged bundle by
digest, and publishes exact-version OCI aliases, the npm package, and the
GitHub Release with its assets. Existing aliases are checked against their
expected digests so a retry cannot silently replace a different artifact.

Only stable `major.minor.patch` versions are accepted, without leading zeroes.
Prereleases are rejected because publication assigns npm's `latest` tag and
creates an ordinary GitHub Release. A future prerelease channel must choose a
separate npm dist-tag and mark the GitHub Release as a prerelease. `+build`
metadata is also rejected because versions become OCI tags.

Official builds also publish image provenance and SBOM information and
attest the release artifacts. A locally built archive has not passed through
that publication and attestation path.

## Publication and deployment

This repository publishes components. A successful main snapshot can also
send a `lightspeed-main` repository dispatch containing its source SHA and
release-bundle digest to the configured deployment repository. The GitHub
variable `LIGHTSPEED_DEPLOYMENT_REPOSITORY` and secret
`LIGHTSPEED_DEPLOYMENT_DISPATCH_TOKEN` configure that notification. If they
are absent, publication can still succeed; the notification emits a warning.

The deployment repository owns what happens next: validation, installation,
and service rollout. Publishing a release here does not itself deploy
production. For an installation you operate, follow
[Self-host Lightspeed](../deployment/self-hosting.md) and
[Upgrades and recovery](../deployment/upgrades-and-recovery.md) using the
components from one manifest.

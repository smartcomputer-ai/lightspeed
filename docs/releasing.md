# Build and release

Lightspeed owns and publishes a coherent release containing the hosted runtime,
the Incus provider, envd, the CLI, Configurator MCP, the platform backend/web
image, the connector-host image (published as `platform-workers`), the generated
TypeScript client, the static in-browser demo and documentation site, API
contracts, checksums, an SPDX SBOM, and a release manifest. A consumer should
pin one manifest rather than selecting components separately.

For the contributor walkthrough, start with
[Contributing and releasing](documentation/development/contributing-and-releasing.md).
[Changing contracts](documentation/development/changing-contracts.md) covers
API generation, protocol compatibility, and migration authoring. This file
retains the detailed packaging and publication procedures.

## Database migrations

PostgreSQL migrations are embedded in `lightspeed-runtime`. Apply them before
starting any gateway or worker process:

```bash
lightspeed-runtime migrate
lightspeed-runtime schema-version
lightspeed-runtime
```

The migrate command uses `LIGHTSPEED_POSTGRES_URL` (falling back to the test
URL only in development), takes a PostgreSQL advisory lock, verifies immutable
SHA-256 checksums in `schema_migrations`, and applies each pending file in its
own transaction. Normal startup only verifies the ledger and exits with a
diagnostic when migration is required.

An existing Lightspeed schema without ledger entries is never baselined
automatically. By default, both startup verification and `migrate` fail and
list the recognized tables. Upgrading such a pre-ledger database requires an
explicit, validated adoption procedure. Preserve databases with valuable data
until that path has been established. For disposable development installations,
reset the full Lightspeed schema or database before applying the embedded
migrations; resetting only the environment tables is insufficient.

Deployments that deliberately provision the Lightspeed tables through an
external schema-management system can set:

```bash
LIGHTSPEED_ALLOW_UNLEDGERED_SCHEMA=true
```

This permits gateway and worker startup only when Lightspeed detects its tables
without ledger entries. It emits a warning and makes the operator responsible
for schema compatibility. It does not fabricate migration records, bypass a
stale or corrupted ledger, or make `lightspeed-runtime migrate` accept an
unledgered database.

Never edit a migration after an official release. Add the next contiguous SQL
file under `crates/store-pg/migrations/`, register it in
`crates/store-pg/src/migrations.rs`, and bump `LIGHTSPEED_SCHEMA_REVISION` in
`release/metadata.env`.

The TypeScript platform owns a separate Drizzle migration history under
`platform/db/migrations/` and applies it when the platform backend starts. CI
tests both an empty installation and an upgrade from the supported baseline in
`LIGHTSPEED_PLATFORM_UPGRADE_FROM` against real PostgreSQL. The manifest
records that baseline and `LIGHTSPEED_PLATFORM_SCHEMA_REVISION`. Run the same
gate with a non-production database whose user may create temporary databases:

```bash
LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL=postgres://... npm run test:migrations
```

The platform ledger was rebased on 2026-08-30 to the single
`0000_platform_baseline` entry (auth, universes, setup installations): bots,
triggers, events, channel accounts, and pairings moved into the Rust core
schema, so the platform database holds people and the universe mapping
and nothing else. Keep that shape: a new area gets its own migration, and its
tables live in their own `platform/db/src/schema/<area>.ts`. A rebase
invalidates the previous Drizzle ledger. A valuable existing database needs a
validated migration/adoption procedure; replacing ledger rows by hand does not
establish that its schema or data matches the baseline. Disposable development
databases can be reset with `./dev.sh reset`. A journal with a single entry
passes the gate on the empty-install check alone; the upgrade check resumes
with the next migration. See
[Upgrades and recovery](documentation/deployment/upgrades-and-recovery.md) for
deployment maintenance and recovery requirements.

## Local release build

The Linux build runs inside the digest-pinned Debian 12/Rust image:

```bash
make release
```

`make release-dist` compiles the runtime, Incus provider, and CLI for the GNU
target, then builds envd separately for the static musl target. It builds the
generated client, Configurator, web UI, and documentation site, and produces
`dist/`.
The demo build is packaged as a target-independent static archive whose files
are served under `/demo/` with an `index.html` fallback; it is not included in
the Platform image.

The documentation build is packaged as `lightspeed-docs-<version>.tar.gz` with
the contents of `docs/site/dist/` at its root. It contains the HTML pages,
styles, scripts, fonts, images, licenses, Pagefind search index, sitemap,
static 404 page, per-page Markdown exports, and `llms.txt`. Serve it under
`/docs/`, stripping that prefix when looking up files, with directory indexes
and a real 404 response for unknown paths.
It requires no application runtime and is not included in the Platform image.
The [documentation serving contract](site/README.md#content-negotiation-at-deployment)
describes how the deployment can select Markdown using the `Accept` header.

The same root lockfile deterministically stages the platform and connector-host
runtime payloads. `make release-images` copies those prebuilt files into the
`runtime`, Configurator, platform, and `platform-workers` images; it does not
invoke Cargo or rebuild the web UI. The `platform-workers` image is the
connector host (`platform/connectors`) with every provider dependency; it
keeps its name so image references and manifest keys stay stable while Bots
and Channels core run inside the `runtime` image. Image smoke tests compare
the runtime's `lightspeed-runtime` executable byte-for-byte, start the platform
image against PostgreSQL, check its health and SPA, and load the connector
host's configuration and task-queue derivation from the staged runtime.

The Rust container is named `runtime` because it is the hosted product core,
not merely an HTTP server. Its executable is `lightspeed-runtime`; standalone
archives use
`lightspeed-runtime-<version>-<target>.tar.gz` and publish as `runtime-bundle`.

The runtime tarballs are intermediate image inputs and are removed before the
release bundle is finalized; the published images carry their own digest,
SBOM, and provenance records.

Release and snapshot builds first publish the composite `build-env` image under
a run-specific staging tag, then run that exact image by digest. The release
manifest records this composite image digest, not merely the Rust base-image
digest.

Windows envd is built separately on the hz01 Windows VM. Both workflows select
one commit before starting Linux and Windows jobs, and assembly depends on both
jobs succeeding. Main snapshots retain the existing successful-Linux-CI trigger.
Windows PR CI is not added. A failed or unavailable Windows builder prevents
publication and deployment of that release, even if Linux staging succeeded.

`LIGHTSPEED_ENVD_TARGETS` in `release/metadata.env` is the complete required
target set and is also embedded in the server. Manifest v1 adds `envdTargets`,
`envdBinaries` keyed by target, and `envdBuilds` for provenance. `envdTarget` and
`binaries.envd` remain aliases for the Linux bootstrap artifact so existing
installers and rollback bundles remain compatible. Discovery (`envd.json`)
contains exactly the same target/file/checksum set. Each daemon target gets its
own immutable OCI archive reference; Windows uses
`envd-x86_64-pc-windows-msvc-bundle`. Both archives are also in the aggregate
release bundle and tagged GitHub Release assets.

Linux packaging uses `--partial` internally until the Windows archive arrives.
Normal manifest verification requires all targets; `--partial --published` is
rejected. Assembly verifies the Windows archive checksum, product version,
commit, protocol, pinned Rust version and Cargo.lock hash before publication.
`dist/provenance/windows-envd.json` records Windows, MSVC, Windows SDK, linker,
Cargo, Rust and compiler flags. The Windows job attests its archive and provenance
before upload; the Linux build-container digest only describes Linux builds.

### Windows builder

The registered runner is `hz01-windows-lightspeed`, selected with
`[self-hosted, windows, x64, hz01, protected]`. Its Windows service runs under
the dedicated non-administrator local account `lightspeed-build`, with delayed
automatic startup and recovery configured by the runner installer. Runner files
and checkouts are under `C:\actions-runner` and `C:\actions-runner\_work`;
builds do not use `C:\dev\lightspeed`. The runner directory is writable only by
the build account, Administrators and SYSTEM. The label `protected` is routing
metadata; trusted workflow triggers enforce which source is built.

The VM has Visual Studio C++ Build Tools, the Windows SDK, Git for Windows,
PowerShell 7, Node, and a separate Rust installation. `CARGO_HOME` and
`RUSTUP_HOME` in the runner's `.env` point into
`C:\actions-runner\toolchains`; `PATH` in the same file includes the installed tools.
The build script discovers MSVC using `vswhere`, reads the pinned Rust version
from release metadata, and uses `--locked`. Keep these tools available to the
service account, rather than installing them only in an interactive user's
profile. After changing runner environment files, restart the runner service.

To reproduce the Windows job in a clean dedicated checkout, using PowerShell 7:

```powershell
./scripts/release/build-windows.ps1 -ExpectedSha (git rev-parse HEAD)
```

The script rejects dirty or mismatched checkouts, compiles envd with a static C
runtime, rejects imports of an external Visual C++ runtime DLL, and packages
`lightspeed-envd.exe` at the archive root. It extracts that archive and runs an
ignored live test against those exact bytes: startup and WebSocket handshake,
Unicode file write/read, process output, and exit status. It also verifies
`--version` and `--print-build`. Output is under `dist-windows/`; use a clean
checkout or a new `-OutputDirectory` for another invocation. This acceptance
does not require Git Bash, PowerShell 7 inside envd, or symlink privileges.

Runner administration follows GitHub's
[Windows service setup](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/configure-the-application).
Registration credentials are not repository files. This build integration does
not install envd as a production service or implement Windows self-upgrades;
those deployment lifecycle checks remain separate work.

Release constants—including both Rust and platform database compatibility—are
centralized in `release/metadata.env`; run
`scripts/release/verify-metadata.sh` after changing a product, protocol, schema,
toolchain, or build-image version. Every executable reports the product
version, full source commit, target, and Rust version through `--version`.

Use `npm run release:prepare -- <major.minor.patch>` to update the product
version, shared Rust workspace version, and Cargo lockfile together. Released
Rust packages inherit `workspace.package.version`; npm package versions are
assigned during staging. The preparation command supports stable releases and
does not publish. Add reviewed notes under `release/notes/`, merge through CI,
and use those notes as the annotated tag message, as described in
[Tagged releases](documentation/development/contributing-and-releasing.md#tagged-releases).

## Publication

- Pull requests and pushes to `main` run path-classified checks on
  GitHub-hosted runners. Rust
  inputs run formatting, lint, cached workspace tests, contract checks, and
  the live migration-ledger acceptance test. TypeScript/contract inputs run
  every generated consumer, all platform unit tests, the Channels Temporal
  integration suite, and the platform empty-install/upgrade migration gate.
  Published manual, site, asset, and included reference changes run the docs
  adapter tests, Astro diagnostics, and a static build with link and asset
  validation. Shared dependency changes also select docs; build, release,
  workflow, and unclassified inputs select all suites. Internal prose selects
  only the lightweight required gate. The root consumer checks exclude docs
  tests so unrelated platform changes do not run them. CI publishes nothing.
- `.github/workflows/macos.yml` provides a manual native Apple Silicon
  compile/`--version` smoke test. macOS development uses `cargo run`; no macOS
  release archive is published.
- The `main` ruleset requires a pull request and the successful, up-to-date
  `required` CI gate. The successful `main` CI workflow triggers
  `.github/workflows/snapshot-main.yml`, which checks out that exact tested SHA,
  confirms that it is still the head of `origin/main`, and builds one coherent
  artifact set on the Linux and Windows hz01 builders without repeating the
  Linux CI test suite. Windows runs its packaged executable acceptance check.
  Documentation-only pushes also start this chain: they skip unrelated CI
  suites, then produce a complete snapshot including the updated manual.
- Snapshot components are first published under a run-specific staging tag and
  recorded by digest in the manifest. After package, archive, manifest, image,
  checksum, and binary/image identity checks pass, the workflow rechecks the
  head of `main` and assigns `release-bundle:sha-<full-sha>` as the single
  public snapshot identity. Consumers resolve that tag once and follow only the
  digest-pinned component references in its manifest. A superseded or canceled
  run may leave staging objects but cannot expose a complete snapshot.
- Every snapshot and tagged release includes `artifacts.docs` in the release
  manifest, with `file`, archive `sha256`, `basePath: "/docs/"`, and a
  digest-pinned `oci://.../docs-bundle@sha256:...` URL. The docs archive also
  ships inside the release bundle and is covered by checksums and build
  provenance. Tagged releases publish it as a GitHub Release asset and assign
  the `docs-bundle:<version>` alias. Documentation is built from the same
  commit as the other release components even when its CI suite was skipped.
- After every completed current-main snapshot, the workflow sends the private
  deployment repository a `lightspeed-main` repository dispatch containing the
  full Git SHA and exact release-bundle digest. Configure
  `LIGHTSPEED_DEPLOYMENT_DISPATCH_TOKEN` as a narrowly scoped GitHub App
  installation token or fine-grained token that may trigger Actions in the
  repository named by the `LIGHTSPEED_DEPLOYMENT_REPOSITORY` GitHub variable.
  Until both values exist, snapshot publication succeeds with an explicit
  warning and an operator can dispatch the digest manually.
  That same notification identifies the documentation through the bundle's
  manifest; no separate docs notification or publication channel is needed.
- A `v<product-version>` annotated tag on `main` triggers
  `.github/workflows/release-tag.yml`. It independently tests and builds the
  exact tagged commit, applies SemVer aliases from the manifest's exact
  digests, publishes the stable TypeScript client, and creates the GitHub
  Release. Release versions must be stable `major.minor.patch` values;
  prereleases are rejected until an isolated publication channel exists, and
  `+build` metadata is incompatible with OCI tags. The workflow never
  looks up or promotes a prior main snapshot.

The `official-release` GitHub environment protects tagged-release credentials;
configure `NPM_TOKEN` only there. Snapshot publication uses the scoped GitHub
token; only the final cross-repository notification uses
`LIGHTSPEED_DEPLOYMENT_DISPATCH_TOKEN`. That notification starts the private
deployment repository's own checks and image publication, not a production
deployment.

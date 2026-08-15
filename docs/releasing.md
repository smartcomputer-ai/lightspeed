# Build and release

Lightspeed owns and publishes a coherent release containing the hosted runtime,
the Incus provider, envd, the CLI, Configurator MCP, the platform server/web
image, one Channels image startable in each supported role, the generated
TypeScript client, API contracts, checksums, an SPDX SBOM, and a release
manifest. Foundry is deliberately not a release artifact. A consumer should
pin one manifest rather than selecting components separately.

## Database migrations

PostgreSQL migrations are embedded in `lightspeed-server`. Apply them before
starting any gateway or worker process:

```bash
lightspeed-server migrate
lightspeed-server schema-version
lightspeed-server both
```

The migrate command uses `LIGHTSPEED_POSTGRES_URL` (falling back to the test
URL only in development), takes a PostgreSQL advisory lock, verifies immutable
SHA-256 checksums in `schema_migrations`, and applies each pending file in its
own transaction. Normal startup only verifies the ledger and exits with a
diagnostic when migration is required.

An existing Lightspeed schema without ledger entries is never baselined
automatically. By default, both startup verification and `migrate` fail and
list the recognized tables. Upgrading such a pre-ledger database requires an
explicit, validated adoption procedure; until one exists, reset the full
Lightspeed schema or database before applying the embedded migrations.
Resetting only the environment tables is insufficient.

Deployments that deliberately provision the Lightspeed tables through an
external schema-management system can set:

```bash
LIGHTSPEED_ALLOW_UNLEDGERED_SCHEMA=true
```

This permits gateway and worker startup only when Lightspeed detects its tables
without ledger entries. It emits a warning and makes the operator responsible
for schema compatibility. It does not fabricate migration records, bypass a
stale or corrupted ledger, or make `lightspeed-server migrate` accept an
unledgered database.

Never edit a migration after an official release. Add the next contiguous SQL
file under `crates/store-pg/migrations/`, register it in
`crates/store-pg/src/migrations.rs`, and bump `LIGHTSPEED_SCHEMA_REVISION` in
`release/metadata.env`.

The TypeScript platform owns a separate Drizzle migration history under
`platform/db/migrations/` and applies it when the platform server starts. CI
tests both an empty installation and an upgrade from the supported baseline in
`LIGHTSPEED_PLATFORM_UPGRADE_FROM` against real PostgreSQL. The manifest
records that baseline and `LIGHTSPEED_PLATFORM_SCHEMA_REVISION`. Run the same
gate with a non-production database whose user may create temporary databases:

```bash
LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL=postgres://... npm run test:migrations
```

## Local release build

The authoritative build runs inside the digest-pinned Debian 12/Rust image:

```bash
make release
```

`make release-dist` compiles all Rust executables in one Cargo invocation,
builds the generated client, Configurator, and web UI, and produces `dist/`.
The same root lockfile deterministically stages platform and Channels runtime
payloads. `make release-images` copies those prebuilt files into the `runtime`,
Configurator, platform, and Channels images; it does not invoke Cargo or
rebuild the web UI. The `channels` image includes all connector dependencies
and selects `workflows`, `activities`, `telegram`, `whatsapp`, or `all` at
startup; `all` is the default and starts Telegram unless
`LIGHTSPEED_CHANNELS_CONNECTORS`
selects another connector set. Image smoke tests compare the runtime's
`lightspeed-server` executable byte-for-byte, start the platform image against
PostgreSQL, check its health and SPA, and validate every Channels role.

The Rust container is named `runtime` because it is the hosted product core,
not merely an HTTP server. Its executable and standalone archive remain named
`lightspeed-server` and `server-bundle` during the compatibility window.

The runtime tarballs are intermediate image inputs and are removed before the
release bundle is finalized; the published images carry their own digest,
SBOM, and provenance records.

Release and snapshot builds first publish the composite `build-env` image under
a run-specific staging tag, then run that exact image by digest. The release
manifest records this composite image digest, not merely the Rust base-image
digest.

Release constants—including both Rust and platform database compatibility—are
centralized in `release/metadata.env`; run
`scripts/release/verify-metadata.sh` after changing a product, protocol, schema,
toolchain, or build-image version. Every executable reports the product
version, full source commit, target, and Rust version through `--version`.

## Publication

- Pull requests and pushes to `main` run path-classified checks on
  GitHub-hosted runners. Rust
  inputs run formatting, lint, cached workspace tests, contract checks, and
  the live migration-ledger acceptance test. TypeScript/contract inputs run
  every generated consumer, all platform unit tests, the Channels Temporal
  integration suite, and the platform empty-install/upgrade migration gate.
  Build, release, and workflow changes run both suites; documentation-only
  changes run only the lightweight required gate. CI publishes nothing.
- `.github/workflows/macos.yml` provides a manual native Apple Silicon
  compile/`--version` smoke test. Published standalone archives remain
  Linux-only in the first cut; macOS development uses `cargo run`.
- The `main` ruleset requires a pull request and the successful, up-to-date
  `required` CI gate. The successful `main` CI workflow triggers
  `.github/workflows/snapshot-main.yml`, which checks out that exact tested SHA,
  confirms that it is still the head of `origin/main`, and builds one coherent
  Linux artifact set on hz01 without repeating the CI test suite.
  Documentation-only pushes do not start the `main` CI/snapshot chain.
- Snapshot components are first published under a run-specific staging tag and
  recorded by digest in the manifest. After package, archive, manifest, image,
  checksum, and binary/image identity checks pass, the workflow rechecks the
  head of `main` and assigns `release-bundle:sha-<full-sha>` as the single
  public snapshot identity. Consumers resolve that tag once and follow only the
  digest-pinned component references in its manifest. A superseded or canceled
  run may leave staging objects but cannot expose a complete snapshot.
- After every completed current-main snapshot, the workflow sends the private
  deployment repository a `lightspeed-main` repository dispatch containing the
  full Git SHA and exact release-bundle digest. Configure
  `LIGHTSPEED_DEPLOYMENT_DISPATCH_TOKEN` as a narrowly scoped GitHub App
  installation token or fine-grained token that may trigger Actions in the
  repository named by the `LIGHTSPEED_DEPLOYMENT_REPOSITORY` GitHub variable.
  Until both values exist, snapshot publication succeeds with an explicit
  warning and an operator can dispatch the digest manually.
- A `v<product-version>` annotated tag on `main` triggers
  `.github/workflows/release-tag.yml`. It independently tests and builds the
  exact tagged commit, applies SemVer aliases from the manifest's exact
  digests, publishes the stable TypeScript client, and creates the GitHub
  Release. Release versions may use prerelease suffixes but not `+build`
  metadata because the same version is also an OCI tag. The workflow never
  looks up or promotes a prior main snapshot.

The `official-release` GitHub environment protects tagged-release credentials;
configure `NPM_TOKEN` only there. Snapshot publication uses the scoped GitHub
token; only the final cross-repository notification uses
`LIGHTSPEED_DEPLOYMENT_DISPATCH_TOKEN`. That notification starts the private
deployment repository's own checks and image publication, not a production
deployment.

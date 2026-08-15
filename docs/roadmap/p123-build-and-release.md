# P123 — Lightspeed-owned build and release

Status: Lightspeed repository implementation complete; infrastructure and
private deployment-repository cutover remain.

Implemented here:

- digest-pinned Debian 12/Rust 1.97.1 release build environment, published and
  recorded by its composite image digest;
- one Cargo release compilation for server, provider, envd, and CLI artifacts;
- shared executable version/source metadata;
- embedded advisory-locked PostgreSQL migrations with an immutable checksum
  ledger, explicit `migrate`, startup verification, and schema diagnostics;
- deterministic standalone archives, checksums, release manifest schema, SPDX
  SBOM, and artifact smoke tests;
- runtime and Configurator images that consume prebuilt `dist/` output;
- publishable `@lightspeed/agent-client` release metadata;
- separate coherent workflows: successful `main` CI automatically builds and
  publishes only SHA snapshot references, while SemVer tags independently test
  and build their exact commit before publishing version aliases, npm, and a
  GitHub Release;
- two-phase snapshot publication with run-specific staging identities,
  digest-pinned manifests, and one release-bundle SHA alias as the atomic
  completion marker; and
- a manual Apple Silicon macOS build/smoke guard, while macOS release archives
  remain deferred.

Remaining outside this repository:

- provision and harden the hz01 build VM/runner carrying the workflow labels;
- configure GitHub release environments and npm publication credentials;
- configure the narrowly scoped `LIGHTSPEED_DEPLOYMENT_DISPATCH_TOKEN` secret
  and `LIGHTSPEED_DEPLOYMENT_REPOSITORY` variable so the implemented exact
  snapshot-bundle dispatch reaches the private deployment repository;
- complete the deployment/migration/rollback drill; and
- retire the hz02 CI guest after the required acceptance runs.

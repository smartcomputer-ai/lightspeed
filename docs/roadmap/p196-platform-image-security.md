# P196 — Platform image security and registry acceptance

**Status:** Implemented and verified locally on `fix/platform-production-image`,
2026-11-09. The rebuilt image has not been published or evaluated against registry
security policies; successful proxy pulls remain an open acceptance requirement.

## Goal

Ship Platform with its required production dependencies and an explicitly pinned
base containing available security fixes. Preserve startup, database migrations,
authentication, native dependencies, and the release pipeline's locked package
versions. Establish the actual registry blocking rule before claiming that an
image change resolves a denied pull.

## Evidence

Image security findings need to be traced to package origins and the registry's
configured policy actions. Policy-violation totals are not counts of distinct
CVEs, and evaluating an older release today does not establish a historical
clean baseline.

For Nexus Firewall, **Proxy Warning** entries alone do not establish which policy
denies a pull. Nexus quarantines a container when a violation has a failing
action at the proxy stage; the actual failing entries need to be collected from
the complete
[container report](https://help.sonatype.com/en/firewall-for-docker.html).

Comparing the published GHCR image SBOMs and the local `v0.6.1..v0.7.0` source
diff established the following package origins:

| Component | Origin in the original Platform images |
| --- | --- |
| glibc, Perl, zlib and other Debian packages | Pinned `node:24.21.0-bookworm-slim` base |
| `undici@6.27.0`, `npm@11.19.0`, brace-expansion, glob, minimatch, npm's tar and other utilities | `/usr/local/lib/node_modules/npm` in the Node base |
| `undici@8.12.3` | `better-auth` optional Vitest peer, then Vitest's optional jsdom peer |
| `source-map@0.6.1` | Development tooling retained through Drizzle Kit and Vite/Terser |
| `fflate@0.8.3` | Platform shared code's production dependency |
| `set-cookie-parser@3.1.2` | Better Auth / Better Call production dependency |

The listed package versions were already present in 0.6.1. The platform base
layers and Dockerfile did not change for 0.7.0. Its shared npm lockfile did change:
the visual-testing commit `bf6c24c5` added screendiff-js and upgraded Vitest from
5.0.3 to 5.1.0, together with several transitive dependencies. Playwright and
screendiff themselves were absent from the installed production image.

An isolated install reproduced Vitest, Vite, jsdom and Drizzle Kit surviving
`npm ci --workspace @lightspeed-ai/platform-backend --omit=dev`. Adding
`--omit=peer` did not eliminate them. Better Auth declares optional peers that
are also development roots in the workspace manifests.

Runtime 0.7.0 contains the same original glibc, Perl and zlib versions, but no
npm or separately installed undici package. Image acceptance must be assessed
independently using the evaluated digest, policy actions, waivers and pull/cache
history before attributing differences to OS packages.

## Implemented changes

### Production dependency staging

[install-runtime-dependencies.mjs](../../scripts/release/install-runtime-dependencies.mjs)
operates only on the disposable staging directory created by
[stage-runtimes.sh](../../scripts/release/stage-runtimes.sh):

1. Remove development dependency declarations from the staged workspace
   manifests and their copied lock metadata.
2. Recompute the lock's dependency reachability offline, with lifecycle scripts
   disabled, before installing the selected runtime workspace.
3. Reject any change to a retained external package's version, resolved source
   or integrity compared with the committed lockfile.
4. Install the resulting graph with `npm ci --omit=dev --offline --ignore-scripts`.

The repository's development manifests and committed lockfiles remain intact.
Required peers and ordinary optional dependencies remain eligible, including
platform-specific native binaries. Globally omitting optional dependencies
would risk breaking those binaries and is not the solution.

The staging fix applies to Platform and the connector host because they share
the script. The connector host's Docker base and the standalone Configurator
packaging are unchanged.

### Platform image

[platform.Dockerfile](../../release/platform.Dockerfile) now uses
`node:24.22.2-trixie-slim`, pinned to image index
`sha256:0820eaa85e07d0490446dd01d14707e17e6108a36988cf374899cd057eb3812d`.
Node remains on major version 24; the OS base changes from Debian 12 Bookworm
to Debian 13 Trixie. npm/npx and Yarn are removed from the final filesystem;
Platform starts directly with `node --import tsx`.

The image contains glibc `2.41-12+deb13u4`. Debian records that version as fixing
[CVE-2026-5450](https://security-tracker.debian.org/tracker/CVE-2026-5450)
and [CVE-2026-5928](https://security-tracker.debian.org/tracker/CVE-2026-5928).
The current Bookworm package `2.36-9+deb12u14` remains affected by both, so
updating only the Node patch version on Bookworm would not address them.

This is not a claim of zero vulnerabilities. As checked on 2026-11-09,
[CVE-2026-5435](https://security-tracker.debian.org/tracker/CVE-2026-5435),
[CVE-2026-6238](https://security-tracker.debian.org/tracker/CVE-2026-6238),
[CVE-2026-6791](https://security-tracker.debian.org/tracker/CVE-2026-6791) and
[CVE-2026-6368](https://security-tracker.debian.org/tracker/CVE-2026-6368)
remain open for Trixie and are classified by Debian as minor issues without a
dedicated security advisory. Scanner severity and configured policy actions
still need to be evaluated independently.

The Rust Runtime image, database revisions, public APIs and workflow protocols
are unchanged. The build environment remains on its existing pinned base;
Linux dependencies staged on Bookworm were tested in the final Trixie image.

### Numeric runtime identity

Platform, connector host and Configurator images now declare `USER 1000:1000`,
preserving their existing runtime identity while allowing Kubernetes to verify
`runAsNonRoot` from numeric image metadata. Runtime retains `USER 65532:65532`.
Release smoke checks verify both the declared and effective user/group IDs.
All three Dockerfiles built locally, and identity checks passed on local image
candidates while rejecting the previous named-user Platform image.

Platform migrations, health and web serving were also rechecked by applying the
numeric-user setting to the previously validated Linux image. This avoids using
host-staged native dependencies for the Linux startup check. Cluster policies
that require explicit pod user/group fields still need those fields supplied.

### Regression protection

The new release test reproduces an optional development peer leaking into a
production install, then checks that staging removes it and its descendants.
It verifies that production dependencies, required peers and a platform-specific
optional dependency remain loadable at their locked versions. A stale manifest
that would change a retained package version is rejected.

[smoke-images.sh](../../scripts/release/smoke-images.sh) checks the final
Platform dependency inventory, including nested packages, for unwanted test and
development tools. It also rejects npm/npx and Yarn in the final filesystem.
These checks run before the existing startup, migration and HTTP smoke checks.

## Local validation

Completed on 2026-11-09:

- All 17 release-script tests passed. The focused dependency test was rerun
  after adding the stale-manifest rejection case and passed.
- TypeScript SDK and production web builds passed.
- The actual staging script completed for both Platform and connector host
  inside a Linux/amd64 Node container, including offline dependency installs.
- Both Linux/amd64 images built successfully. Platform's final image used the
  Trixie base above, running under local Docker emulation on Apple Silicon.
- Platform migrated an isolated PostgreSQL 17 database, became healthy, served
  `/app`, signed in a seeded password administrator and retrieved the persisted
  session. The final image dependency inventory guard passed.
- Connector configuration and workflow contract checks passed; Temporal worker
  and WhatsApp modules loaded from the staged connector image.
- Shell syntax checks, Node syntax checks and `git diff --check` passed.

The original published Platform SBOM contained **193 distinct application npm
name/version pairs** under `/app/node_modules`; the rebuilt installation contains
**96**. This comparison excludes Node's global npm installation and is not a
prediction of a registry scanner's component or violation count.

The rebuilt image excludes the separately installed undici packages, source-map,
Vitest, Vite, jsdom and Drizzle Kit. Node's own embedded HTTP implementation is
outside that package-removal claim. Required `fflate@0.8.3` and
`set-cookie-parser@3.1.2` remain; their findings require the individual advisories
and supported fixed versions before a targeted dependency change.

The latest local candidate is `lightspeed-platform:numeric-user`, derived from
the Linux-validated `lightspeed-platform:nexus-fix` image with the numeric-user
setting. It is a validation artifact, not a published release or a replacement
for the existing 0.7.0 tag.

## Remaining acceptance

- [x] Trace components to actual image contents and dependency roots.
- [x] Remove unwanted production dependencies without changing retained versions.
- [x] Update the Platform base to include the confirmed glibc fixes.
- [x] Remove package managers and add release regression checks.
- [x] Verify Linux packaging, startup, migrations, password authentication and
  connector imports locally.
- [ ] Collect the complete blocking policy entries and affected image digest
  from the registry scanner; warning severities alone do not identify a
  denied-pull trigger.
- [ ] Publish a candidate through the normal release workflow and evaluate its
  exact digest against registry security policies. Record remaining components,
  policy actions and the actual pull outcome.
- [ ] Resolve remaining application findings using their CVE/Sonatype IDs and
  fixed-version information; document any accepted residual risk separately.
- [ ] Verify supported OIDC authentication, custom CA trust and PostgreSQL
  workload identity integrations after the Debian base change. Local password
  authentication does not establish those integrations.
- [ ] Review the Runtime and other Node images separately if their evaluations
  require equivalent base or package-manager changes.

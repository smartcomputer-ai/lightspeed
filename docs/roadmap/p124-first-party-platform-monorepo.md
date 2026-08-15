# P124 — First-party platform monorepo

Status: repository implementation complete 2026-08-15. Public-tree import,
workspace consolidation, CI, and coherent release construction are in place.
The imported product is greenfield, so active product identity was reset to
Lightspeed without a legacy compatibility window. Private source removal and
the first deployment cutover remain external follow-up work.

Builds on [P123](p123-build-and-release.md). P123 remains the release authority;
P124 extends its coherent build, manifest, provenance, and publication model to
the first-party TypeScript platform.

Implemented repository slice 2026-08-15:

- moved the public TypeScript client to `clients/typescript` and Configurator
  MCP to `platform/configurator-mcp`;
- moved committed generated API artifacts to their owner at
  `crates/api/contract`;
- imported the platform server, web UI, operator CLI, shared inputs, database,
  Channels, and mechanically preserved Foundry package under `platform/`;
- established the private root npm workspace, in-tree client dependencies,
  `@lightspeed/*` package names, and Node 24 baseline;
- updated generators, CI, release staging, SBOM inputs, documentation, and
  development commands for the new paths;
- extended the P123 manifest and release workflows with digest-pinned Rust
  runtime, platform, Configurator MCP, and a configurable Channels image;
- added platform empty-install/upgrade migration checks, a non-skipping
  Channels Temporal integration gate, runtime-image smoke tests, and a single
  successful-main-CI prerequisite for snapshot publication;
- recorded the platform schema revision and supported migration baseline in
  the same release manifest as the artifact digests;
- consolidated every Channels role and connector into one image selected by
  its startup command, while giving Foundry no independent image or
  publication entry;
- standardized imported deployment settings on `LIGHTSPEED_PLATFORM_*` and
  removed the pre-release legacy environment-variable aliases;
- reset active Channels workflow IDs, task queues, search attributes, hash
  domains, Platform setup resource IDs, browser storage keys, and the Channels
  database role to Lightspeed-owned names;
- mechanically normalized Foundry's imported identifiers without otherwise
  changing its unsupported product or release status;
- made snapshot dispatch target a configurable private deployment repository
  using `LIGHTSPEED_DEPLOYMENT_REPOSITORY` and
  `LIGHTSPEED_DEPLOYMENT_DISPATCH_TOKEN`;
- renamed the top-level local environment from `local/` to `dev/`, keeping its
  Compose topology and configuration beside its lifecycle helpers rather than
  treating the environment as a generic script collection;
- replaced the Platform-only development launcher with one profile-aware
  `dev/stack.mjs` supervisor for infrastructure, runtime, Platform,
  Configurator, and Channels development;
- separated the runtime and Platform migration domains into `lightspeed` and
  `lightspeed_platform` databases on the shared local Postgres server, and made
  the reset helper use the runtime's ledgered migrator;
- normalized the Rust Temporal client boundary so the shared
  `TEMPORAL_ADDRESS=host:port` convention works across Rust and TypeScript; and
- made Platform authentication origins explicit and configured both loopback
  Vite origins in the unified development supervisor; and
- aligned the `full` development profile with Platform's multi-universe proxy
  boundary by defaulting the runtime to `trusted-header`, while retaining
  `single` for focused runtime development; and
- deleted the top-level example `profiles/` fixtures and references.

Still pending: the first infrastructure-backed publication and deployment
acceptance of the extended P123 artifact set, remaining runtime-configurable
branding, replacement of the Channels emission mirror after that wire type is
generated publicly, import/license review closure, and private
deployment/source retirement.

## Goal

Make the public `smartcomputer-ai/lightspeed` repository the source of the
complete Lightspeed product: deterministic Rust runtime, generated API clients,
management plane, web UI, channel ingress, and optional workflow integrations.

Keep a private Lightspeed deployment repository containing only infrastructure,
encrypted secrets, compose and deployment configuration, DNS/TLS policy,
release pins, and product-specific runtime configuration. The recommended
repository name is `lightspeed-deployment`.

The intended boundary is:

```text
public Lightspeed repository       private deployment repository

runtime and API                    release-manifest pin
generated clients                 deployment topology
platform server and web UI        encrypted secrets
channel workers                   DNS/TLS and ingress policy
database schema and migrations    product-specific runtime config
images and release manifest       deploy/rollback scripts
tests, docs, and SBOM              production operations
```

## Decision

Import the former private `app/` npm workspace into this repository as the
first-party platform layer:

- `server`;
- `web`;
- `channels`;
- the existing `foundry` package only as a mechanical compatibility import;
- `db`;
- `core`; and
- `cli`.

Import the current source snapshot as new commits. Do not import the private
repository's Git history, because that history has touched secrets and
production details. Preserve applicable authorship and licensing records
without copying private Git objects.

Do not create a second public `lightspeed-platform` repository. The platform
and runtime already share one evolving protocol and release boundary. A second
public repository would preserve the client publication, revision locking, and
cross-repository coordination that P124 is intended to remove.

The workspace moves as a unit because the current server and database still
reference Foundry. P124 does not redesign, extract, promote, or add release
work for Foundry while its product future is undecided. The platform server/UI,
Channels roles, Telegram connector, and WhatsApp connector remain independently
buildable and deployable components.

## Why this is one product boundary

The current repository split does not correspond to an independent domain:

- the platform gateway is a universe-scoped Lightspeed API proxy built around
  the generated TypeScript client;
- web types and configuration references derive from the Lightspeed contract;
- Channels creates Lightspeed sessions and uses Lightspeed workflow tools
  directly;
- the platform server and Channels share one application database schema and
  migration history; and
- server routes import contracts from the worker packages.

Today, one protocol change requires an OCI release bundle, a packaged client
tarball, lock-update scripts, repository dispatch, two-source artifact tags,
and mirrored TypeScript wire contracts. In one repository the runtime change,
generated client, consumers, tests, and release artifacts can land atomically.

This direction also matches Lightspeed's product-first architecture: the public
repository becomes a hosted agent product rather than a headless engine plus a
privately maintained product plane.

## Repository and package shape

Use one private root npm workspace, with its lockfile authoritative for
development and CI. Keep package-local client and Configurator lockfiles only
as deterministic P123 release-staging inputs; they are not independent
development-resolution authorities. Keep the TypeScript client separately
identifiable as the public client and place the cohesive product plane under
`platform/` without adding generic root `apps/` or `packages/` taxonomies:

```text
clients/
  typescript/          generated @lightspeed/agent-client

platform/
  server/              management API and static web host
  web/                 React management UI
  cli/                 platform operator CLI
  shared/              shared Zod inputs and deterministic helpers
  db/                  Drizzle schema, migrations, and database adapter
  channels/            channel workflows and provider workers
  foundry/             mechanical compatibility import only
  configurator-mcp/    generated MCP facade
  scripts/             platform development and generation helpers

crates/
  api/
    contract/          committed generated schema, manifest, OpenRPC, reference

dev/                   local Compose environment, configuration, and helpers
scripts/
  release/             coherent build and publication automation
```

The root workspace spans `clients/typescript` and `platform/*`. Every component
retains its own package boundary, but a directory named `packages/` is not
required to enforce that boundary. The existing `interop/` directory is
removed because it mixes a public client, a deployable service, and generated
Rust API output.

The generated `@lightspeed/agent-client` remains the sole stable TypeScript
contract boundary. In-tree consumers use the workspace source directly during
development and CI. Tagged releases may continue publishing the client to npm
for external consumers through P123.

Rename imported internal packages to the `@lightspeed/*` scope. Keep the
workspace root and application-only packages marked `"private": true` unless a
package is intentionally supported as a public npm artifact. Open source does
not imply npm publication.

Replace the duplicated Channels `contracts/emissions.ts` shapes with
generated-client exports. Leave Foundry's internal shape untouched until the
feature is retained or removed; P124 must not create new Foundry architecture.
Do not introduce any new platform-local copy of Rust wire vocabulary.

The imported code remains outside the deterministic `engine` crate. Platform
HTTP, authentication, database access, connectors, and Temporal workers are
side-effecting product-plane components and must continue to communicate
through the public `api` and generic workflow-tool protocols.

## Product neutralization and greenfield identity

The imported workspace was never a public production compatibility boundary.
Use a clean Lightspeed identity for every active product and operational name;
do not carry pre-release aliases or dual-poll legacy queues into the product.

### Runtime-configured product identity

Move deployment identity out of source wherever it is not part of a durable
protocol:

- product name, descriptions, logos, and links shown by the web UI;
- public origin and domain examples;
- bootstrap setup display names;
- support and legal links;
- cookie/storage namespaces where migration is safe; and
- deployment-specific endpoints and feature availability.

Provide neutral Lightspeed defaults. A private deployment supplies any
deployment-specific branding and URLs through runtime configuration, not a
private source build or post-build source patch.

Platform configuration uses documented `LIGHTSPEED_PLATFORM_*` names. Core
runtime configuration remains under `LIGHTSPEED_*`, Channels-specific settings
use `LIGHTSPEED_CHANNELS_*` where a product prefix is needed, and Configurator
settings use `LIGHTSPEED_CONFIGURATOR_MCP_*`. Do not accept legacy product-name
aliases in application or development code.

### Durable operational identity

Because the imported product is greenfield, reset active durable names before
the first deployment, including:

- Temporal workflow IDs, task queues, signals, queries, and workflow type
  names;
- Lightspeed tool IDs, profile IDs, setup IDs, and environment metadata tags;
- database schemas, migration ledger entries, roles, constraints, and stored
  enum/string values;
- channel session keys, pairing identities, deduplication keys, and delivery
  routing identities; and
- browser storage or auth cookie names whose replacement would log users out or
  orphan state.

Use `lightspeed.*`, `lightspeed-*`, `Lightspeed*`, and
`LIGHTSPEED[_COMPONENT]_*` forms as appropriate. Existing development
databases, Temporal workflows, browser storage, and connector state may be
reset; they are not upgrade inputs. The initial migration may therefore be
corrected before its first supported release.

Foundry's imported identifiers use the same Lightspeed naming rules, but this
mechanical neutralization does not give the package a supported image or
deployment path.

## Component and feature boundaries

Monorepo membership does not make every integration part of the default
deployment:

- the management server and web UI form the ordinary platform plane;
- Channels is an optional capability with independently runnable workflow,
  activity, and provider roles launched from one image;
- Foundry remains a mechanically imported, unsupported candidate integration;
  it receives no extraction or new release work until a separate keep/remove
  decision;
- Telegram and WhatsApp provider workers are independently selectable; and
- no optional connector may be required to build or start the core platform.

The Baileys-backed WhatsApp connector remains an explicit, default-off role
with its unofficial status and operational risk documented. It shares the
Channels image to keep the release topology small; enabling it is still a
deployment choice, and it can be removed later without changing the platform
image.

## Coherent public release

Extend P123 so one Lightspeed release identifies every supported first-party
artifact built from the same source revision and generated contract. The public
repository owns builds and publishes immutable artifacts. At minimum, the
release manifest identifies as applicable:

- Rust runtime image plus provider, envd, server-bundle, and CLI artifacts;
- Configurator MCP image;
- `@lightspeed/agent-client` package and contract revision;
- platform server/web image; and
- one Channels image startable as `workflows`, `activities`, `telegram`,
  `whatsapp`, or `all`.

Foundry artifacts are deliberately absent from the required P124 release set.
If Foundry is retained, a later roadmap item may add supported artifacts; if it
is removed, no P124 release compatibility promise blocks deletion.

Every image is selected by digest. The manifest also carries checksums,
source/build metadata, migration compatibility, and the P123 SBOM/provenance
outputs.

The private deployment pipeline consumes a pinned, completed public release
manifest. It must not rebuild public Lightspeed source from a Git revision.
Deployment-specific behavior comes from runtime configuration. This preserves
the P123 supply-chain boundary and prevents the private repository from
recreating a second build and compatibility system.

The deployment bundle may add private configuration identities, but it records
the unchanged public artifact digests. Rollback selects a previous complete
release manifest plus its documented database compatibility procedure.

## CI and contract enforcement

Path filtering may avoid unrelated Rust and Node work, but it must encode the
actual dependency graph. A skipped consumer suite must never make a contract
change appear green.

Required gates:

- Rust API schema/OpenRPC/reference generation is current;
- the TypeScript client generated output is current;
- all platform packages typecheck and run their unit tests;
- database migrations apply from an empty database and upgrade the supported
  previous release;
- relevant Postgres and Temporal integration suites run without hidden
  environment-variable skips;
- platform/web production builds and every selected image smoke-test;
- a contract or generated-client change always triggers every TypeScript
  consumer check, regardless of path filters;
- shared workspace metadata and lockfile changes trigger every affected Node
  package; and
- the release manifest cannot publish until all declared artifacts exist by
  digest and report the same source/contract identity.

Branch protection should require one aggregate P124 compatibility result so a
path-filtered job that is intentionally skipped cannot accidentally bypass the
cross-language gate.

## Open-source import gate

Fresh commits avoid publishing private history, but they do not prove that the
snapshot is safe or legally distributable. Complete all of the following
before the first public commit:

1. Build the import from an explicit source allowlist; exclude `.git`, local
   state, generated secrets, logs, databases, credentials, production exports,
   connector sessions, and build output.
2. Scan the staged snapshot and generated release artifacts for credentials,
   tokens, private keys, internal hostnames, private addresses, user data,
   hardcoded universe/account IDs, and production endpoints.
3. Manually review security-sensitive defaults, development bootstrap users,
   example credentials, OAuth configuration, CORS/cookie policy, and logging.
4. Establish copyright and Apache-2.0 relicensing authority for imported
   source. Preserve required notices and attribution.
5. Audit direct and transitive dependency licenses plus vendored UI code,
   fonts, icons, images, and other assets. Produce the P123 SBOM and required
   NOTICE material.
6. Ensure tests and fixtures contain only synthetic identities and data.
7. Add ongoing secret, dependency, license, and generated-artifact checks to
   public CI.

Do not treat a clean secret scanner result as the whole review; internal
topology and personal or production identifiers may not match secret patterns.

## Sequencing

The imported application is greenfield. Repository work therefore uses a clean
Lightspeed identity and may invalidate pre-release development state. Private
deployment cutover and obsolete-machinery removal still remain sequenced so
the source-of-truth transition is explicit and reviewable.

### Phase 1 — Inventory and freeze

- record the exact imported source revision;
- inventory package dependencies, generated contracts, database migrations,
  durable IDs, runtime images, and deployment inputs;
- classify every imported product-specific value as branding/configuration,
  durable operational identity, or private deployment material; and
- define the supported upgrade and rollback boundary.

### Phase 2 — Safe import and workspace integration

- pass the open-source import gate;
- import the complete workspace as new commits under the chosen platform
  directory;
- establish Apache-2.0 and package metadata;
- rename package scopes and greenfield durable runtime identities; and
- replace packaged-client and sibling-checkout dependencies with in-tree
  workspace dependencies.

### Phase 3 — Neutralization and contract consolidation

- add runtime product configuration with neutral Lightspeed defaults;
- remove pre-release environment aliases and reset durable identities to
  Lightspeed names;
- replace mirrored wire types with generated-client exports;
- update developer commands, READMEs, architecture guidance, and examples; and
- make every optional worker independently selectable.

Delete the current top-level example `profiles/` tree and its README/Quick
Start references rather than carrying those development fixtures into the new
product layout. This does not remove the profile API or profile registry.

### Phase 4 — CI and release integration

- add dependency-aware cross-language checks;
- extend P123 builds, SBOM, provenance, smoke tests, and the release manifest;
- publish a non-production snapshot containing all selected platform artifacts;
  and
- pass empty-install, upgrade, rollback, Postgres, Temporal, and deployment
  acceptance tests.

### Phase 5 — Private deployment cutover

- change the private deployment repository to pin one complete public
  Lightspeed release manifest;
- deploy unchanged public images by digest with private runtime configuration;
- verify auth, universe administration, gateway passthrough, web UI, Channels,
  migrations, metrics, and rollback; and
- verify that no pre-release worker or state is reused across the cutover.

### Phase 6 — Remove obsolete machinery

After production acceptance and rollback-window closure, remove:

- `prepare-lightspeed-release` and app-local client tarballs;
- Lightspeed-client lock/update scripts;
- cross-repository protocol dispatch and two-source artifact tags;
- sibling-checkout bootstrap assumptions;
- mirrored emission contracts;
- private builds of public source; and
- application source from the private repository.

Rewrite the private repository README around deployment and operations. Retain
only the public release pin, deployment configuration, secrets, infrastructure,
and operational procedures.

## Accepted tradeoffs

- The public repository gains a larger Rust and TypeScript build/test surface.
  Dependency-aware path filtering contains latency without weakening contract
  checks.
- The repository carries management-plane, authentication, database, and
  connector concerns in addition to the runtime. This is the intended
  first-party product boundary, while crate and package boundaries still
  prevent those concerns from entering the deterministic engine.
- Public releases contain independently deployed artifacts. One manifest is
  preferable to coordinating product versions across repositories; using one
  Channels image for several roles keeps that artifact set intentionally
  small.
- The greenfield identity reset intentionally invalidates imported development
  state. Avoiding permanent compatibility code is more valuable than preserving
  that pre-release state.
- The optional WhatsApp connector carries additional policy and optics risk.
  Keeping it default-off limits runtime exposure, while the shared Channels
  image accepts a larger dependency and distribution surface in exchange for
  simpler builds, promotion, rollback, and deployment configuration.

## Non-goals

- Publishing every internal package to npm.
- Folding the TypeScript product plane into Rust crates or the deterministic
  engine.
- Making Channels, Foundry, Telegram, or WhatsApp mandatory.
- Deciding whether Foundry remains a Lightspeed feature or investing in its
  package boundaries, contracts, tests, images, or documentation beyond the
  minimum needed to preserve the imported build.
- Replacing generic workflow-tool protocols with integration-specific runtime
  APIs.
- Moving production secrets, host topology, DNS/TLS configuration, or private
  operational history into the public repository.
- Redesigning deployment infrastructure while moving the product source.

## Completion criteria

P124 is complete when:

- all first-party application source lives and is reviewed in the public
  Lightspeed repository;
- one PR and one required compatibility gate cover runtime contract changes,
  generated clients, and all in-tree consumers;
- one P123 release manifest identifies all selected components from one source
  and contract revision, with immutable digests and provenance;
- a clean installation and a supported production upgrade both pass database,
  Temporal, auth, gateway, UI, and Channels acceptance tests;
- the private deployment repository deploys unchanged public artifacts by
  digest with runtime product configuration and can roll back through the
  documented procedure;
- no active execution or stored state depends on a retired durable identity;
- obsolete tarball, lockstep, dispatch, sibling-checkout, and mirrored-contract
  machinery is deleted; and
- the private repository contains deployment and operations material, not a
  second copy or build of Lightspeed product source.

# P168 — Environment Skill Catalogs and Catalog Refresh

Status: implemented, 2026-09-09. Separate VFS and environment catalogs use idle-boundary discovery. Within-run refresh, catalog merging, and automatic workspace materialization remain deferred.

UI follow-up, 2026-09-11: the shared profile/session configuration editor exposes
independent skill discovery switches for environments and VFS, with environment
scope fields and required linked VFS roots. Empty VFS drafts remain editable and
are validated before saving.

Discovery follow-up, 2026-09-11: automatic home discovery now searches only
`.agents/skills` and `.lightspeed/skills`. Settings describe this scope without
adding a separate home switch; project compatibility roots remain supported.

## Scope

Keep the existing workspace model and VFS skill discovery. Add environment
skill discovery as an independent source with its own model-facing catalog.
There is no combined effective catalog, preferred-copy selection, cross-domain
deduplication, or automatic fallback between sources.

Use the existing idle refresh boundary for initial discovery and later updates.
Do not add scans or catalog rebuilds on model continuations, tool completion,
job completion, or filesystem mutation during a run. Within-run discovery can
be added later if a concrete workflow needs it.

The [VFS expansion freeze](p166-vfs-environment-transfer.md#vfs-expansion-freeze)
parks automatic workspace materialization and its profile/UI integration. VFS
retirement and the artifact replacement are also parked. Neither is a
prerequisite for this work; existing explicit capture and materialize remain
available.

Discover skills installed inside the selected environment, including files
written by external installers, and make them available to the model alongside
VFS skills. Add efficient envd filesystem helpers and a shared runtime catalog
refresh path so changes can reach new work at the next idle refresh boundary
without rewriting the existing prompt prefix.

This builds on [skill simplification](p167-simplify-skill-activation.md) and
preserves [catalog supersession](archive/p136-context-catalogs.md). It does not
depend on [VFS–environment transfer](p166-vfs-environment-transfer.md): discovery works on
directories already present in an environment.

## Behavior

The target interaction is:

```text
skills are installed in the environment
-> session reaches its next idle refresh boundary
-> runtime refreshes the environment catalog
-> new work sees the skill and reads SKILL.md with environment tools
```

No registration callback from the installer is required. Manually copied
skills, repository checkouts, and materialized directories use the same path.
Editing or removing a skill becomes visible through discovery at the next idle
refresh. If the agent installs a skill during a run, its catalog entry waits
until that boundary; the agent may still read and use the skill immediately
through ordinary environment tools when it knows the path.

An explicit materialize operation can populate a configured discovery root.
Its results become discoverable at the next idle refresh, without a transfer
completion hook or a within-run rescan. Discovery does not register mappings,
copy files, or require a VFS source. Installed, manually copied, and explicitly
materialized directories all follow the same discovery path.

Ordinary re-reads use the latest file contents. Discovery does not pin a skill
body, install dependencies, execute scripts, or create an activation state.

## Two independent skill catalogs

Publish two separately identified catalogs to the model and API:

| Catalog | Context key | Locations and tools |
| --- | --- | --- |
| VFS skills | `runtime.catalog.skills.vfs` (existing) | Linked workspace/snapshot paths, read with `vfs_*` tools. |
| Selected environment skills | `runtime.catalog.skills.environment` | Paths on the selected environment, read with environment file tools; bundled scripts run there through process tools. |

Keep the existing VFS key. The environment key is stable across environment
switches; its envelope and entries identify the concrete environment. A skill
entry includes its identity, name, description, skill directory, and readable
`SKILL.md` path. Environment skill identity includes the environment and source
location so equal paths on different machines cannot identify the same skill.
Model rendering labels the domain and its reader explicitly.

Share parsing, metadata structures where suitable, and catalog publication
helpers. Keep discovery, permissions, source identities, and freshness separate.
Each source follows its own grant; environment skills do not require
`features.vfs`, and VFS skills do not require an environment. API/UI views retain
the separate catalog identities, content references, and availability states.
They may display both sections, but do not flatten them into a merged menu.

### Duplicate names and copies

A skill present in both domains appears in both catalogs, even when an explicit
materialize or capture copied it. Do not compare content or transfer provenance,
choose a preferred copy, or deduplicate across domains. Equal names are not an
identity. Label source locations so the model and user can choose deliberately.
An unavailable environment skill does not cause its VFS counterpart to replace
it; the independently available VFS catalog continues to describe VFS skills.

Within one environment, aliases resolving to the same canonical directory may
be deduplicated. Independent directories remain distinct even if they have
identical names or contents. This local alias handling does not link catalogs.

### Publication and environment changes

Use the shared `Catalog { title }` context kind, stable keys, and supersession
rules for each catalog independently. Publishers store model-facing text and
retain structured source snapshots through provenance. An environment-only change must not
rewrite, supersede, or clear the VFS catalog. A VFS-only change must not mutate
the environment catalog. No third combined catalog is published.

On an environment switch, invalidate the old environment catalog before the
next model call so its paths are no longer advertised as current. Discover the
new source at the next idle refresh boundary; a mid-run switch does not trigger
a scan or catalog rebuild. Source invalidation is distinct from discovery.
On deselection or disabling environment discovery, clear its catalog using the
existing keyed removal behavior. Leave VFS catalog state alone. Previously
advertised paths must never be interpreted as belonging to the newly selected
machine. Historical catalog versions retain their original source and rendering.

Publish only when that catalog's semantic content changes. Source fingerprints,
scan timestamps, and transfer receipts belong outside the model-facing payload.
Environment identity, useful metadata, and meaningful availability changes are
semantic. Disabling VFS discovery clears only the VFS catalog. Preserve the
existing bounded retention of superseded versions for each key; do not create
a permanent context key for every environment a session visits.

## Discovery roots and interoperability

Configure environment discovery separately from `features.vfs.skills`. Resolve
paths using the environment's execution user and the session's configured
working directory, never the hosted worker's home directory. A temporary `cd`
inside a command does not change the session's discovery scope.

Support project and user `.agents/skills/`, a Lightspeed-specific skill root,
and explicit additional roots. Provide documented compatibility roots for
project-local client installations, such as `.claude/skills/` and `.codex/skills/`,
without recursively scanning every dot-directory or package cache. Resolve
project ancestry within an explicit repository/work root boundary.

Scan skill directories for `SKILL.md` with bounded depth and file/byte limits.
Follow skill-directory symlinks within the configured filesystem access policy,
including links to an installer's canonical copy. Record canonical directory
identity and deduplicate aliases to the same directory in the same environment.
Detect loops and report unreadable targets. A copied directory with the same
name is not necessarily the same skill.

Keep distinct skills with colliding names addressable by source/location.
Use deterministic ordering and make ambiguity visible to clients and the model
rather than silently merging their contents. Separate copied directories stay
separate entries in the environment catalog. Transfer history does not affect
identity or ordering.

Replace the current line-oriented frontmatter parser with YAML parsing that
handles multiline descriptions and nested optional metadata. Share parsing
between VFS and environment catalog builders. Extract the fields needed for
discovery, tolerate unknown fields, and report malformed required metadata per
skill without failing the entire catalog. Do not adopt a vendor's execution
hooks or permission semantics merely because its metadata is present.

## Efficient envd helpers

Use a **bounded filesystem scan** as the reusable environment abstraction.
There is no skill-specific method, frontmatter schema, or catalog type in the
environment protocol. Its current `fs/globFiles`, `fs/searchText`, and
`fs/readFile` operations establish the existing filesystem boundary. Use the
delivered capability-advertised `fs/scan` operation, which combines matching-file
enumeration and optional content reads in one endpoint-local pass. Close any
remaining interoperability gaps within that generic filesystem boundary.

Illustrative shape, not final RPC names or DTOs:

```text
fs/scan(
  roots,
  include_patterns,
  read_content,
  digest_algorithm?,
  symlink_policy,
  limits,
  if_none_match?
)
  -> unchanged { fingerprint }
   | result { entries, fingerprint?, complete, diagnostics }

entry:
  root, relative_path, canonical_path, kind
  file: size_bytes, executable, content?, content_digest?
```

For skills the caller supplies patterns matching `SKILL.md` and requests its
bytes. That filename is query data, not a special envd rule. Preserve byte
content with the protocol's ordinary byte representation; UTF-8 decoding and
YAML parsing are caller concerns. The same operation can discover instruction
files, project manifests, configuration files, or template directories with
small metadata files.

Optional per-file digests support reuse planning for other callers without
returning file contents. Start with SHA-256 over complete file bytes, matching
the transfer layer's content identity. Hashing is opt-in; skill discovery only
needs the small matching documents. Include requested digests in the query's
result fingerprint, and bound bytes inspected for hashing separately from
response size. Transfer inventories also include directory entries so empty
directories can be represented.

Reuse the current glob matching conventions. Bound visited entries, depth,
matched files, bytes per file, total bytes, and elapsed time. Return canonical
paths for symlink-alias detection while retaining the requested/root-relative
paths needed to explain locations. Enforce the endpoint's filesystem access
scope and handle loops, missing roots, unreadable files, and symlink target
changes explicitly. Generic environment information such as the execution
user's home directory and default working directory belongs in endpoint
metadata when needed for root resolution, not in a list of skill directories.

Enumerate and read on the machine, returning the bounded result in one response
rather than one network round trip per directory and file. Do not force the
first implementation to maintain a subscription, watch handle, or per-session
scan object. Large transfers remain the separate file/tree transfer facility;
this operation supplies observations, not durable filesystem snapshots.

### Reuse for transfer planning

The [VFS transfer layer](p166-vfs-environment-transfer.md#reuse-and-incremental-transfer)
shares these traversal, metadata, and hashing primitives. An aggregate result
fingerprint answers whether the observed result changed; per-file digests
identify which bytes a receiver can reuse. Materialization already has its
desired inventory in VFS and inspects environment files as reuse candidates.
Capture inventories environment files and uploads only content missing from
the authorized CAS scope.

Keep byte transfer, missing-content negotiation, staging, and publication in
the transfer operations. They require complete selected-tree inventories and
verification that transferred or reused bytes match the advertised digests.
A scan result alone does not pin files for a later read. Large inventories may
use the transfer facility's bounded streaming instead of expanding the small
scan response into a mandatory transfer transport. File reuse preserves full
replacement semantics, including removals; it does not introduce merging.

Provide bounded inventory batching/streaming for the
[large-tree transfer path](p166-vfs-environment-transfer.md#large-trees-and-one-logical-transfer),
sharing traversal and entry semantics with small catalog scans. Operation
completion must establish whether the entire selected inventory was observed;
independent truncated scans cannot be combined into a supposedly complete
tree. Transfer operations own any retained inventory and byte state needed
across requests. This does not require persistent watches or scan handles for
ordinary small skill discovery at idle refresh boundaries.

### Conditional scans and cache correctness

A fingerprint identifies a complete observed result for the same query and
access scope: matching paths, resolved identities, requested metadata, and
requested file bytes or digests. Include additions, deletions, renames, and
symlink retargeting. Content changes invalidate queries requesting bytes or
digests; metadata-only results do not establish byte equality. Exclude volatile
accounting such as scan duration.
An unrecognized fingerprint or a changed query can simply return a full result.

`if_none_match` allows a small unchanged response when the current observation
matches. It saves transport, parsing, and catalog work; it does not imply that
discovering filesystem changes costs nothing. Initially, a bounded local scan
of small matching files is sufficient. Root directory mtime alone is not a
correct change detector, and size/mtime alone can miss in-place file edits.

Later, envd may cache query results and invalidate them with filesystem watches.
Missed/overflowed events, reconnects, or uncertain watcher coverage require a
rescan. A stat or watcher optimization must not turn an unverified stale result
into an unchanged response. These are generic filesystem optimizations, not
skill state, and are not prerequisites for the first implementation.

Only a complete observation may authorize reuse of a previous complete result.
A limit, read error, or detected concurrent mutation must not produce
`unchanged`. Return diagnostics/completeness explicitly and let the runtime
retain its last good observation as stale. Do not silently replace a complete
catalog with a partial listing or treat unavailable roots as confirmed deletions.
Distinguish a successfully observed absent root from failure to inspect it.

Even a complete scan is a live observation, not an atomic multi-file snapshot.
The runtime uses it to build a menu, and subsequent file reads return current
contents. It does not promise that files cannot change between scan and use.

### Runtime discovery and reuse

The environment path at an eligible idle refresh boundary is:

```text
idle refresh with the selected environment already accessible
  -> conditional fs scan of configured discovery roots
  -> parse changed SKILL.md metadata in runtime code
  -> build the environment catalog
  -> publish only if that catalog's semantic content changed
```

This path does not read VFS catalogs, workspace revisions, or materialization
mapping records. Envd needs no skill identity, workspace ID, catalog merge rule,
or access to Lightspeed stores. Share traversal/entry primitives with capture
where useful without coupling observation to transfer or model interpretation.

An unchanged scan can reuse its parsed environment catalog only for the same
environment, query, and access scope. Still evaluate availability and enabled
state before publication. VFS discovery and its publication proceed separately;
a VFS change does not invalidate an environment observation.

Discover environment skills exclusively through `fs/scan`. An endpoint without
that capability produces an explicit unavailable source; do not emulate discovery
with glob, directory listing, individual file reads, or shell commands. Partial
scans must report their limits and cannot be passed off as complete catalogs.
Correctness must not depend on observing writes through a Lightspeed-specific
installer.

Separate scan fingerprints from published catalog content. Body-only changes
can invalidate a scan without changing the model's menu. File mtimes, last-check
timestamps, and cache counters should not cause prompt updates. The model reads
the current body through its normal file tool when needed.

## Idle-boundary catalog refresh

Reuse the runtime publication machinery used by existing catalogs. Share
helpers for stable keys, semantic content comparison, CAS persistence,
publication/removal, and diagnostics where needed. Each source keeps its own
discovery and freshness policy. Separate catalogs do not require duplicate
publication implementations or a new catalog framework; use the existing
supersession mechanism.

Use the existing refresh path when the session is idle with no active or
queued run. This includes eligible idle API reads and preparation before
admitting new work. Preserve current admission semantics: this is not a promise
of a fresh scan before each already queued run. Do not add a model-continuation
refresh boundary as part of environment skill support.

At that idle boundary, conditionally scan the selected environment if it is
accessible. This observes out-of-band changes as well as changes made through
Lightspeed tools, without tracking every write. Command completion, job
completion, and filesystem edits during a run do not schedule discovery. A
still-running installer can be observed at a later eligible idle refresh.

During a run, keep the discovered menu until the next eligible refresh, subject
to source invalidation on environment switch/deselection. Ordinary skill reads
still use current file contents; a removed file fails through normal filesystem
handling. No watcher, timer, or mutation-driven skill invalidation system is
required.

For VFS catalogs, retain the existing workspace-head freshness inputs and idle
refresh behavior. Other catalogs retain their own policies. Share the
builder/publication implementation between eligible gateway and workflow idle
paths without changing the timing of unrelated catalogs.

External controllers continue to own catalogs they publish through the public
context API. The common runtime refresh path must not overwrite those entries.
VFS-sourced system instructions and frozen tool schemas retain their existing
separate behavior; this proposal generalizes catalog updates only.

Apply context mutations through the existing workflow/engine command boundary.
All environment and storage I/O stays in activities/adapters. The engine keeps
generic catalog ordering, supersession, and retention rules, with no skill-read
or activation state.

## Availability and cache behavior

Do not wake an environment solely because an idle session or skill list is
being inspected. Return the last observation with freshness/availability
information. Once normal readiness handling makes the environment accessible,
discovery can occur at the next eligible idle boundary. Becoming ready during
a run does not itself trigger discovery. A failed scan is distinct from a
successful empty scan.

Bound refresh work so a missing or slow environment does not indefinitely
block an otherwise usable session. Keep the last catalog only for that same
source and expose its stale/unavailable status. Never reuse another
environment's menu after selection changes. Diagnostics and observation times
belong primarily in API/debug views; publish a model-facing status update only
when the useful availability state changes.

Unchanged semantic catalogs produce no context update. Changed catalogs append
a successor using current cache-preserving behavior. Earlier entries render
unchanged until normal superseded-entry cleanup. The current catalog survives
compaction as it does today; skill file contents receive no special retention.

## Implementation and verification

1. Add environment source/location and configuration support, fix shared YAML
   parsing, and publish a separate environment model/API catalog without
   activation fields. Preserve the independent VFS catalog and its key.
2. Integrate the delivered generic conditional scans; require `fs/scan` and
   report discovery unavailable when it is absent. Close the remaining generic
   scan interoperability gaps. Verify
   unchanged results, additions/deletions, same-size edits,
   symlink retargeting, changed queries/access scope, and explicit incomplete
   results. Verify optional file digests against content bytes, hashing limits,
   and directory entries used by transfer inventories. No skill types or YAML
   parsing belong in the protocol or envd helper.
3. Reuse catalog publication at the existing idle refresh boundary, with
   selected-environment identity and bounded availability handling. Add no
   discovery hook to model continuations or tool/job completion.
4. Verify direct-install directory layouts, symlink aliases, multiline metadata,
   name collisions, edits/deletions, and filesystem access/scan limits. Verify
   a copied skill remains visible in both catalogs, equal names in different
   domains remain distinct, and no automatic cross-domain fallback occurs.
   Cover independently enabled/disabled sources and ensure permissions in one
   domain never grant access to the other.
5. Verify an installation or external edit becomes visible at the next eligible
   idle refresh. Verify model continuations and tool/job completion do not scan
   or rebuild catalogs, while direct file reads still see current content.
   Verify an environment switch invalidates the old menu without scanning the
   new source mid-run, and switching or clearing the environment leaves the VFS
   catalog unchanged. Preserve the existing behavior for already queued runs.
6. Verify unchanged scans do not append context, body-only edits remain readable
   without menu churn, updates preserve provider prefixes, and normal catalog
   compaction/removal still works. Include replay coverage for changed boundaries.
7. Verify offline/missing capabilities, partial scans, bounded failures, and
   idle reads that do not power on environments. Regenerate affected API/workflow
   contracts and update current skill/environment documentation when shipping.

Use local filesystem fixtures matching installer output for normal tests;
running npm against public registries is not a prerequisite for discovery tests.

Progress:

- [x] Scope revised: keep workspaces, publish two separate catalogs, and defer within-run refresh, merging, and automatic workspace materialization.
- [x] Environment catalog, independent configuration, and shared parser implemented.
- [x] Separate model/API publication, source labels, and independent removal implemented.
- [x] Generic envd conditional scans: roots/patterns, metadata or raw content, optional SHA-256, complete fingerprints and partial diagnostics.
- [x] Scope narrowed: discovery requires `fs/scan`; endpoints without it report unavailable, with no fallback discovery path.
- [x] Shared publication at existing idle refresh boundaries implemented.
- [x] Installer-layout, availability, cache, and replay checks pass.

### Implementation notes

- `features.environments.skills` independently configures the session discovery
  working directory, project ancestry boundary, and additional roots. The
  endpoint handshake supplies its execution-user home directory; conventional
  `.agents/skills` and `.lightspeed/skills` roots are resolved there. Project
  directories additionally include `.claude/skills` and `.codex/skills`. Home
  compatibility roots require explicit additional roots, including when home
  is itself the working directory or project boundary.
- Gateway and workflow idle refreshes share one environment discovery adapter
  and the existing immutable catalog publisher. Conditional observations use a
  bounded process-local cache scoped by universe, session, environment, query,
  connection, and environment grants; cache loss simply causes a complete scan.
  Semantic snapshots remain durable context provenance. Failed scans retain
  only the same environment's last observation as stale; revoked access drops
  its advertised paths. Inspection does not request power changes.
- Generic scans now report canonical paths, support opt-in confined symlink
  traversal, distinguish missing roots from inspection failures, and enforce
  aggregate quotas. Transfer operations keep their existing secure backend and
  publication semantics. Shared confined access, observations, hashing, and errors
  are owned by `filesystem/backend`; bounded `filesystem/scan.rs` and
  `filesystem/transfer` are siblings. Scan diagnostics use filesystem language.
  No skill parsing moved into envd or the protocol.
- Workflow source invalidation appends ordinary keyed removal commands after
  recorded selection/configuration changes. It performs no scan. Gateway
  publications carry context revision checks and are rejected if work has been
  admitted or the selected source has changed in the meantime.
- The API returns homogeneous `catalogs` views with catalog-level source identity,
  reference, availability, skills, and warnings. Skill entries retain readable
  paths without repeating source identity or exposing runtime context keys.
  CLI and chat picker display each catalog separately. Installer
  copies in the two domains remain distinct; no combined menu is published.

### Verification

Local fixtures cover installer directories, canonical aliases, equal names,
YAML multiline/nested metadata, malformed skills, additions/removals, same-size
and body-only edits, unchanged responses, changed queries/access scope,
symlink retargeting/loops/escapes, directory inventory entries, SHA-256 identity,
aggregate limits, missing capabilities, stale recovery, denied grants, and a
bounded unresponsive endpoint. Workflow tests replay source removal, preserve
VFS entries, and retain the existing idle/queued admission boundary. Existing
catalog retention and compaction checks remain applicable to both generic
catalog keys.

Relevant Rust library, CLI, and protocol suites pass. API and workflow exporters
were run, and all affected TypeScript consumers regenerated; a second generation
produced identical artifacts. TypeScript typechecks, consumer tests, and builds
pass. The root `npm run check` stops at its `git diff --exit-code` generated-file
check because the requested generated updates are intentionally uncommitted;
regeneration stability and the remaining check stages were verified separately.
No live or credentialed suites were run.

The catalog DTO and filesystem ownership follow-up passes 621 scoped Rust tests
(API, CLI, environment daemon, and Temporal server libraries/binaries; one
existing test remains ignored). Projection tests cover separate equal-name/path
catalogs, environment-only unavailability, and selection changes preserving
VFS. CLI tests cover source-specific readers, unavailable sources without
fallback, ambiguous IDs, and separate picker sections. All 74 envd tests retain
transfer and bounded-scan coverage after moving the shared backend. API and
TypeScript/Configurator generation reproduces all 13 checked artifacts exactly;
TypeScript checks, consumer/Configurator tests, and builds pass. The root check's
uncommitted-generated-file limitation described above still applies.

### Explicit VFS discovery follow-up

VFS discovery now requires `features.vfs.skills.roots`, a nonempty list of
absolute paths inside declared workspace links. Absent `skills` disables the
source. There are no conventional-root defaults or CLI chat enablement.
The workflow refresh request carries the optional VFS discovery config as a
single value. Runtime publication is marked with its source ownership so
revocation can remove the VFS menu without touching environment or controller
catalogs. Discovery remains at eligible idle boundaries; revocation performs
no scan. The homogeneous source-oriented catalog API and environment discovery
configuration remain unchanged.

Validation: targeted engine, tools, runtime gateway, workflow, runner, API,
projection, profile, and CLI/TUI tests pass, including explicit-root validation,
revocation replay, source independence, and late-publication rejection. API and
workflow exporters ran; all 16 generated contract/consumer artifacts reproduce
byte-for-byte. TypeScript checks, consumer/Configurator/web tests, and production
and demo builds pass. The editor now preserves environment skill configuration
when VFS settings are edited. The root `npm run check` still stops at its
committed-file diff guard while generated changes remain intentionally
uncommitted; generation reproducibility and the remaining checks ran separately.
Live/credentialed suites were not run.

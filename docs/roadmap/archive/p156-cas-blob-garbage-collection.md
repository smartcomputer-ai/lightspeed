# P156 — CAS Blob Garbage Collection

## Status

Implemented, with safety and load review changes in September 2026. The schema
is greenfield: changes are folded into the existing baseline migrations (schema
revision 9); deployment requires the planned database reset.

## Objective

Reclaim abandoned uploads and blobs released by session retention without
invalidating durable data or making collection expensive for foreground work.
Keep the design small: relational holders, explicit nested edges, a generous
grace, and an incremental collector. No reference counts, recursive mark phase,
workflow leases, or object-store reconciliation service.

## Durable holders

| Holder | How retention is enforced |
| --- | --- |
| Session events | Store extracts canonical refs from event JSON, batches new roots into `cas_session_roots` in the append transaction; one row per session and digest |
| Bot events | Store batches document, prompt, media, and receiver `toolsRef` into `cas_bot_event_roots` in the insertion transaction |
| Session checkpoints | Existing FK to the current state blob; replacing the checkpoint releases the old blob |
| VFS snapshots and workspace base/head | Existing FKs to manifests |
| Nested blob formats | Explicit `cas_blob_edges`; an incoming edge protects its child until the parent is deleted |
| Engine constants | Small explicit collector pin list |

Session roots are automatic for ordinary append, fork opening events, and clone
opening events. Repeated references within a session do not update roots or
relock blobs already protected by that session. Root deletion cascades with the
owning session or bot event. History forks retain source sessions through the
existing retention-tree rules.

New session roots lock their blobs before committing. Root FKs also protect
against concurrent collection: either the holder commits with its blob intact,
or attachment/deletion fails. The root FKs check at commit so whole-universe
cascade deletion can remove all holders. A rejected append rolls back its
entire event batch. Root extraction replaces the generated event-ref JSONB
column and GIN index; collection never scans event JSON.

Nested writers record edges when writing manifests, MCP asset outputs, skill
catalogs, instruction reports, environment publications, and result envelopes.
Chat tool declarations record edges to all schemas and descriptions; bot events
retain the declaration. A conversation can survive deletion of all its old
events: on its next message it touches the declaration, reconstructing it and
its edges only if the catalog row was collected. Ordinary messages do not
rewrite every tool schema.

Profiles deliberately borrow refs. Saving a profile does not retain its
instructions or other referenced content. Prefer inline text, or ensure another
durable resource retains the referenced blob. There is no public pin API.

## Grace and object safety

Every put touches existing content. API session starts and command admissions
batch-touch their refs before signaling Temporal, so an old upload gets a fresh
grace window when used. Missing refs are rejected even when a process still has
cached bytes. Reads do not renew grace.

Default grace is seven days. It covers ordinary write-to-append handoffs, not an
unbounded workflow outage. Workflow state alone is not a holder. If an activity
result or queued command remains uncommitted beyond grace, collection may remove
it and recovery may require resubmission. This is an accepted limit; generic
workflow boundary holds/checks would add machinery and foreground writes for a
rare case. Lower grace values reduce that margin.

Logical identity remains `sha256:<digest>`. Every physical object upload gets a
unique suffix. After catalog deletion, a concurrent put of the same content uses
a different key; delayed object cleanup can only delete the old incarnation.
Concurrent puts retain the winning catalog row's object and remove their own
unused uploads. Objects are removed only after catalog deletion commits.
Failures or crashes can leave unreachable objects, never intentionally dangling
catalog rows. Cleaning such orphans is outside this collector's scope.

## Bounded collection

- One PostgreSQL advisory-lock leader per deployment, shared by background and
  manual deletion. Leadership holds one connection, with no transaction open
  during the one-hour sleep; dropping the guard closes the connection.
- Each hourly pass examines up to 100,000 old catalog rows per universe,
  including live rows, in pages of at most 1,024. The index on
  `(universe_id, touched_at_ms, digest)` supports ordered cursor scans;
  root/holder lookups use indexes. Pages exclude inline payload bytes.
- In-memory per-universe cursors advance through live pages and wrap, eventually
  revisiting released roots and children. A pass stops at the end of a traversal
  rather than wrapping immediately. Restarts begin again. Universe order
  rotates so a pass budget does not always favor the first tenants.
- A shared soft 10-minute pass budget is checked before each page. Each scan and
  delete transaction has a five-second statement timeout and one-second lock
  timeout. The collector yields on errors and revisits skipped rows later.
- The delete rechecks age, pins, roots, holders, and incoming edges. FKs are the
  concurrency backstop. A conflicting holder skips the batch without a tight
  retry loop. Deleting parents cascades outgoing edges; children drain later.
- Logs/CLI report examined rows, candidates, deleted rows/bytes, object failures,
  and holder conflicts. `cas-sweep --dry-run` is read-only and does not advance
  the background cursor. Manual deletion reports a busy background leader.

This bounds load, not time to reclaim an entire universe: at the default cadence
one leader examines up to 100,000 rows per universe hourly, subject to the
shared time budget. Large live catalogs can take multiple passes to traverse;
remaining work resumes from the stored cursor. The first pass runs immediately
after acquiring leadership, followed by a one-hour wait after each pass.

## Verification

Regression coverage includes delayed object deletion after reupload; a real
PostgreSQL append blocked behind deletion; root deduplication and rollback;
bot documents, prompts, media and receiver tools; profile borrowing; batched
admission touches; leadership exclusion/release; and page progress/wrap.
Existing holder, checkpoint, fork/clone, migration, purge, and retention tests
continue to apply. Temporal live coverage exercises old-ref admission and chat
declaration reconstruction, in addition to normal sessions, runs, profiles,
bots, and channels. Final verification on an isolated PostgreSQL 17 / MinIO /
Temporal stack:

- `cargo build` and `cargo test --workspace --no-fail-fast`: passed, 1,771 tests.
- All store live suites: 42 passed, including migration and purge coverage.
- Local Temporal suites for sessions, runs, profiles, bots, channels, MCP,
  preprocessing, subagents, tenancy, and workflow tools: 56 passed, serialized.
  These used fake providers; paid-provider and long retry-budget suites were
  outside this storage change's validation scope.
- API/TypeScript regeneration and `npm run check`: passed, 268 tests plus builds.
  Generated consistency checks used a temporary Git index containing the new
  generated artifacts; the real staging area was unchanged.
- CLI dry-run and deletion: six expected rows and four objects reclaimed,
  with zero errors or holder conflicts.
- Formatting, diff checks, and release/schema metadata verification: passed.

A warm local `EXPLAIN (ANALYZE, BUFFERS)` of the actual page query over 100,000
retained catalog rows examined a 1,024-row page in 3.1 ms using the catalog and
session-root indexes. An unheld page after those rows took 3.9 ms. These are
query-plan sanity checks, not production throughput guarantees. Full workspace
validation also corrected one pre-existing LLM projection test that still
expected truncated message text despite the current full-message contract.

The hourly allowance update was verified with seven reaper tests and a fresh
server build. A live CLI fixture of 100,001 unheld inline blobs confirmed the
100,000-row cap: dry-run changed nothing, the first deletion pass reclaimed
exactly 100,000 in 5.2 seconds locally, and the next pass reclaimed the final
blob. Both passes reported zero errors; the temporary fixture was removed.
Regression tests cover multiple pages, live rows counting toward the cap,
resumption, stopping at traversal end, expired deadlines, and page failures.

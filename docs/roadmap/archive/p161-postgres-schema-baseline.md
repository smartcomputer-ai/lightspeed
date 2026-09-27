# P161 — PostgreSQL Schema Baseline

Status: implemented and verified, 2026-09-05.

Consolidate the greenfield runtime schema into the nine domain migrations.
Preserve the final relational schema while removing historical upgrade steps,
then simplify the derived retention deadline as described below.

- Fold MCP runtime columns into MCP/auth; checkpoints, session metadata,
  retention, and blob collection into core; registration and environment
  metadata indexes into environments.
- Write the final columns, defaults, indexes, and constraints directly. Remove
  obsolete backfills, duplicate alterations, and the dropped session-root table.
  Keep post-create foreign keys where table dependencies require them.
- Set the embedded and release schema revisions to 9 and remove obsolete SQL
  exports. Keep ledger validation, locking, checksums, and startup verification.
- Existing databases require recreation of the runtime schema and its ledger,
  or a separately designed migration if their data must survive. Do not bypass
  checksum validation or automatically reset an existing database.
- Compare catalog definitions before and after the authorized `./dev.sh reset`,
  including columns, generated expressions, constraints, and indexes. Run
  focused migration/store checks and live tests on the reset local stack.

Progress:

- [x] Original migration chain saved for schema comparison.
- [x] Consolidated baseline and release metadata updated.
- [x] Schema equivalence verified before the generated-deadline follow-up.
- [x] Focused checks and store live tests passed.

Verification:

- The old local ledger matched all 15 saved migration names and checksums.
- `./dev.sh reset` recreated the local runtime and Platform databases, applied
  the new runtime baseline, and cleared the configured Lightspeed MinIO prefix.
- Before/after catalog comparison matched all 27 tables, 312 columns (including
  types, defaults, nullability, and generated expressions), 312 constraints,
  and 77 indexes. Physical column ordering and the migration ledger are excluded;
  no relational behavior changed. The new ledger ends at revision 9.
- All 8 store unit tests and 32 store live tests passed. Live coverage includes
  migration locking/idempotency/checksums, foreign keys in isolated test schemas,
  session lifecycle/retention/clones/checkpoints, CAS/MinIO storage and collection,
  MCP/OAuth, environments, VFS, profiles, API keys, bots, and channels.
- Scoped Clippy with warnings denied, formatting, and release-metadata validation
  passed. The reset built the server successfully; `schema-version` reports both
  current and required revision 9. No provider or broad workspace suites ran.

## Generated retention deadline

Make `sessions.delete_at_ms` a stored generated column containing
`closed_at_ms + delete_after_close_ms`. Keep the reaper index and record/API
field. PostgreSQL now derives the deadline on every close-time or policy write;
append and policy-update SQL no longer assign it. Remove redundant deadline
checks and restrict only the retention duration to roots. The in-memory store
continues to derive the equivalent record projection.

This is an intentional change after the schema-equivalence check above. It
changes the generated column definition and removes two redundant constraints;
revision 9 remains the consolidated greenfield baseline. Reset the local schema
again to apply its updated checksum.

- [x] Generated column, simpler writers, and retention design updated.
- [x] Added live coverage for closing, reopening, changing/removing policy,
  descendant deadlines, due-root lookup, direct SQL writes, and overflow rollback.
- [x] Reset local schema and reran focused store checks/live tests.

Follow-up verification:

- `./dev.sh reset` succeeded; current and required schema revisions remain 9.
- Catalog comparison confirmed only the generated deadline and constraint
  changes: 27 tables, 312 columns, 310 constraints, and all 77 indexes retained.
- All 8 store unit tests and 33 store live tests passed, including the new
  retention regression. Store Clippy with warnings denied, workspace formatting,
  and release-metadata validation passed. Formatting also corrected one existing
  line wrap in a Temporal server test. No broad workspace or provider suites ran.

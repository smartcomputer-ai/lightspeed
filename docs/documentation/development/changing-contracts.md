# Changing contracts

A contract change is complete when both sides agree on the serialized shape
and on what that shape means. Generated types carry the declared API into
consumers. Compatibility fixtures and behavior tests establish that existing
callers still work and that a new field does what its documentation promises.

Lightspeed has several contract boundaries. A public request, a durable
workflow message, an environment handshake, and a database migration have
different owners and different compatibility rules. Identify the boundary
before choosing an exporter or changing a version.

## Find the authored source

| Boundary | Authored source | Outputs to keep aligned |
| --- | --- | --- |
| Public JSON-RPC API | DTOs, method metadata, and service interfaces in `crates/api/src/` | Rust-exported schema, method manifest, OpenRPC, and API reference; TypeScript client, Configurator tools, and profile-editor reference. |
| Workflow integration | Engine emission types, workflow recipes and recovery types, Channels DTOs, and `crates/temporal-workflow/src/workflow_contract.rs` | Workflow schema, manifest, reference, TypeScript types, and known-answer vectors used by client helpers. |
| Environment control and data | `crates/environment-protocol` | Rust serde fixtures, daemon and provider implementations, and explicit protocol versions. There is no schema exporter for this boundary today. |
| Runtime database | SQL migrations and their embedded registry in `crates/store-pg` | Migration ledger, required schema revision, table ownership list, and release metadata. |
| Platform database | Schema modules and Drizzle configuration in `platform/db` | SQL migrations, snapshots, journal, and the separate Platform release revision and upgrade baseline. |

Generated files are checked in so callers can use a reviewed, coherent
contract. Edit their source and run the generators; changes made directly to
generated output will be lost on the next export.

## Change the public API at its boundary

Public clients depend on `crates/api`, rather than reducer internals. Put wire
field descriptions on the Rust DTOs and operation behavior on the method
metadata. The exporter uses those definitions to produce the reference that
both repository readers and the documentation site see.

For a new operation, follow the neighboring DTO, method constant, service
interface, and manifest/dispatcher entries in `crates/api`. The
[RPC manifest](../../../crates/api/src/rpc.rs) ties ordinary dispatch and
method metadata together; operator methods have their corresponding
[operator manifest](../../../crates/api/src/operator.rs). Implement the
service behavior in the runtime and test the actual admission and result.

Choose the operation's scope deliberately. Universe, service, and deployment
methods cross different authority boundaries. The Configurator generator
selects universe-scoped methods and applies
[`tool-filter.json`](../../../platform/configurator-mcp/tool-filter.json).
A new universe method normally becomes an MCP tool unless excluded there;
review that exposure as part of adding the method. Service and operator
methods are not automatically exposed through Configurator.

### Preserve the meaning of an omitted field

Backward event pagination is an existing example. An older
`session/events/read` caller sends parameters like:

```json
{
  "sessionId": "session_1",
  "after": { "seq": 100 },
  "limit": 50
}
```

The current API also accepts an explicit backward direction:

```json
{
  "sessionId": "session_1",
  "direction": "backward",
  "before": { "seq": 100 },
  "limit": 50
}
```

In [the Rust DTO](../../../crates/api/src/sessions.rs), `direction` defaults
to `Forward` and serialization omits that default. `before` is optional and
omitted when absent. As a result, the old request still means a forward read,
and a serialized forward request retains its previous shape.

The contract also defines the cursor behavior: both directions return events
chronologically, while a backward page's `nextCursor` becomes the next
`before`. Its initial `headCursor` can seed a separate live forward read.
Those are operational semantics; optional fields alone cannot establish
them. The
[schema-artifact tests](../../../crates/api/tests/schema_artifacts.rs)
validate old and new payload shapes and the omitted forward default. Service
tests must establish the corresponding pagination behavior.

Use the same reasoning for any added field. Decide what an old caller's
omission means, what a new caller can request, and how an old consumer will
handle the response. A generated client compiling successfully answers only
part of that question.

## Regenerate the public API and consumers

From the repository root, export Rust first, then regenerate downstream
consumers:

```bash
cargo run -p api --bin export-schema
npm install
npm run generate --workspace @lightspeed-ai/agent-client
npm run generate --workspace @lightspeed/configurator-mcp
node platform/scripts/generate-config-reference.mjs
```

The API exporter writes four files under `crates/api/contract/`:
`api.schema.json`, `methods.json`, `openrpc.json`, and `api-reference.md`.
Downstream code generation reads the JSON Schema bundle and method manifest;
OpenRPC is an additional documentation format.

The TypeScript generator reads both the API and workflow exports. It writes
the generated types and method metadata under `clients/typescript/src/generated/`
and schema copies under `clients/typescript/schema/`. Configurator generates
`platform/configurator-mcp/src/generated/tools.ts`. The final command updates
`platform/web/src/lib/profile-config-reference.ts`, which the profile editor
uses to describe configuration fields. Generate the client before that
reference so its installed schema is current.

Keep the generated diff with the authored change. Then check the exported
contract and consumers:

```bash
cargo test -p api --test schema_artifacts
npm run check
```

The Rust freshness test compares the files directly with an in-memory export.
The npm generated checks run generators and then `git diff --exit-code` on
their output paths. They can report an intentional local diff as a failure.
Inspect whether that diff follows from your source change, and include the
reviewed outputs in the contribution. Don't discard expected output or stage
unreviewed files just to silence a check. Once the authored and generated
change is the committed baseline, regeneration should produce no further
diff.

The generated Markdown reference is published directly by the docs site. Run
`npm run check:docs` when its content changes, and update the relevant usage
or integration guide if readers need a different procedure.

## Change a workflow contract with its history in mind

Workflow integration includes signal and query names, start and recovery
payloads, tool emissions, reply envelopes, and identity derivations. Export
that boundary with:

```bash
cargo run -p temporal-workflow --bin export-workflow-contract
npm run generate --workspace @lightspeed-ai/agent-client
```

This writes `workflow.schema.json`, `workflow.json`, and
`workflow-contract.md` under `crates/temporal-workflow/contract/`, then updates
the TypeScript workflow types and copied artifacts. If a change affects both
public and workflow types, run both Rust exporters before generating the
client.

The workflow manifest also contains known-answer vectors for identity
derivations. A schema can describe a string without establishing that Rust
and TypeScript hash the same bytes in the same order. The client's authored
workflow helpers are therefore tested against the generated vectors:

```bash
cargo test -p temporal-workflow --test workflow_contract
npm run test --workspace @lightspeed-ai/agent-client -- test/workflow.test.ts
```

`WORKFLOW_CONTRACT_VERSION` versions the manifest layout. It does not
negotiate every workflow behavior or make existing Temporal histories
compatible with a code change. Review changes to stored events, checkpoint
or rollover state, workflow command ordering, signal names, and identity
derivations against retained histories and data. The current session workflow
uses Temporal patching at its active-run rollover boundary; follow the
existing implementation when a change needs that mechanism. New schemas
alone don't rewrite an old history.

[Agent loop and durability](../how-it-works/agent-loop-and-durability.md)
explains the two histories involved, and
[Workflow tools](../integrating-and-extending/workflow-tools.md) explains the
external receiver and controller contract.

Channel delivery and prepared-media DTOs are included in the workflow export
and reach connectors through `@lightspeed-ai/agent-client/workflow`. Some
checks remain authored on both sides: `CHANNEL_DELIVERY_VERSION` exists in
[`crates/channels/src/delivery.rs`](../../../crates/channels/src/delivery.rs)
and the connector's
[`delivery.ts`](../../../platform/connectors/src/providers/delivery.ts).
When changing delivery compatibility, check both validators and their tests;
regenerating TypeScript shapes does not prove they accept the same versions.

## Change the environment protocol explicitly

The environment protocol belongs to `crates/environment-protocol`. Its serde
fixtures establish the serialized controller and data-plane messages, and
`CURRENT_PROTOCOL_VERSION` defines the handshake version. Daemon and
controller handshakes check exact version equality.

A deliberate protocol-version change therefore affects which existing
daemons and providers can connect. Update the corresponding implementations
and fixtures, align `LIGHTSPEED_ENVIRONMENT_PROTOCOL_VERSION` in
`release/metadata.env`, and plan the rollout across those components. Product
version strings and matching Git revisions do not substitute for this
compatibility check.

Run the wire fixtures and the affected implementation tests:

```bash
cargo test -p environment-protocol --test serde
scripts/release/verify-metadata.sh
```

The [environment-provider guide](../integrating-and-extending/environment-providers.md)
covers the controller, daemon, and gateway responsibilities in more detail.

## Database migrations

Runtime and Platform data have separate owners and migration histories.
Runtime PostgreSQL holds sessions and domain records such as bots and channel
accounts. Platform owns people, authentication, organization/universe mapping,
and setup provenance. Add a table to the database that owns its behavior.

### Runtime migrations

Released migrations are immutable. The runtime records each migration's name
and checksum and rejects changed applied SQL. Add the next contiguous
migration under `crates/store-pg/migrations/`, register it in
`crates/store-pg/src/migrations.rs`, and update `REQUIRED_SCHEMA_REVISION`
and `LIGHTSPEED_SCHEMA_REVISION` in `release/metadata.env` together. Maintain
the `LIGHTSPEED_TABLES` ownership list when tables are added or removed.

Normal Rust startup verifies the ledger. Apply migrations explicitly before
starting the upgraded runtime:

```bash
cargo run -p temporal-server -- migrate
cargo run -p temporal-server -- schema-version
```

`schema-version` is diagnostic; it does not apply changes. The local launcher
already runs `migrate` as a startup preparation. Check the embedded registry
without services using:

```bash
cargo test -p store-pg --lib migrations::tests
scripts/release/verify-metadata.sh
```

Then use the ignored `migrations_live` suite against an appropriate test
database to exercise actual SQL and ledger behavior. See
[Testing and evaluation](testing-and-evaluation.md#run-live-tests-deliberately)
for its prerequisites.

### Platform migrations

Edit the appropriate schema module under `platform/db/src/schema/`, then
generate the migration:

```bash
npm run generate --workspace @lightspeed/platform-db
```

Review and commit the SQL, snapshot, and Drizzle journal together. Platform
applies this separate history on server startup. Set
`LIGHTSPEED_PLATFORM_SCHEMA_REVISION` to the journal length and maintain
`LIGHTSPEED_PLATFORM_UPGRADE_FROM` as the supported upgrade baseline.

The root `npm run generate` is an alias for this Drizzle command. It does not
regenerate API or TypeScript client contracts.

`npm run test:migrations` checks an empty installation and, when the journal
extends beyond its baseline, an upgrade from that baseline. It needs
`LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL` and permission to create and drop
scratch databases. A migration that succeeds on an empty database can still
fail on retained rows, which is why the upgrade path matters.

Resetting disposable local data is useful during development, but it does
not establish an upgrade path for valuable data. Explain any compatibility
constraint in the change and the relevant
[upgrade documentation](../deployment/upgrades-and-recovery.md).

## Review the complete boundary

Before submitting, read the authored and generated diff as one change.
Request defaults, authority scope, runtime behavior, wire examples, and
consumer exposure should tell the same story. For durable contracts, also
account for callers, histories, and data created before the new code exists.

Product versions, API protocol identity, environment protocol versions, and
database revisions describe different boundaries. Use
`scripts/release/verify-metadata.sh` to check the declared values agree with
their owners, then follow
[Contributing and releasing](contributing-and-releasing.md) to package and
submit the change.

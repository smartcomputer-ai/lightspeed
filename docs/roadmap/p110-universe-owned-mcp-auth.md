# P110: Universe-Owned MCP Authentication

**Status**

- Implemented 2026-07-31.
- Greenfield breaking change. Reset local infrastructure after changing the
  committed PostgreSQL migrations; do not add a compatibility migration,
  dual-read path, legacy wire alias, or feature-version transition.
- Builds on P95's declarative MCP feature and P108's rule that credentials
  configure universe resources rather than session links.

## Problem

MCP authentication currently has the wrong owner. A universe MCP server record
describes the endpoint and its authentication policy, but the selected auth
grant lives on each session declaration:

```text
features.mcp.servers[]
  server_id
  allowed_tools?
  approval?
  defer_loading?
  auth_grant_id?       // wrong owner
```

At config admission the gateway loads that grant, validates it against the MCP
server, and materializes its id into `RemoteMcpToolSpec.auth_ref`. The grant id
therefore appears in session config, event-sourced tool state, planned request
fingerprints, profile documents, and the public active-tool projection.

This differs from universe environments. An environment is configured once
with universe-scoped credentials, and every session selecting it receives that
configuration at execution time. Session state carries only the selected
environment id, never its credential ids.

The MCP model also makes credential rotation unnecessarily session-wide: every
profile or session referencing one server must be edited to select a replacement
grant. It permits two sessions to give the same catalog id different operational
identities even though the catalog record otherwise represents one configured
connection.

## Decision

An MCP server id identifies one fully configured, universe-scoped MCP
connection. The universe owns:

- endpoint and transport;
- provider-facing label and descriptive defaults;
- tool and approval defaults;
- authentication policy and OAuth discovery metadata; and
- the auth grant, when any, used to authenticate that connection.

A session only grants access to a server id and may narrow its behavior. It
does not choose, retain, or expose that server's auth grant.

The invariant is:

> A session selects an MCP server. The universe decides which identity that
> MCP server currently uses.

The same endpoint may be registered more than once under different server ids
when different identities are required:

```text
github_personal -> https://api.githubcopilot.com/mcp/ -> authgrant_personal
github_work     -> https://api.githubcopilot.com/mcp/ -> authgrant_work
```

`server_url` remains non-unique. If both records may appear in one session,
their `default_server_label` values must also differ because provider-facing
MCP labels are unique within an active toolset.

## MCP Catalog Shape

Keep authentication policy separate from credential selection. Policy
describes the remote protocol; credential selection identifies the local
universe resource used to satisfy it.

Conceptually, the public MCP server document becomes:

```text
McpServer
  server_id
  display_name?
  server_url
  transport
  default_server_label
  description?
  allowed_tools?
  approval_default
  defer_loading_default?
  auth_policy
  credential?                 // { type: authGrant, grantId }
  status
  revision
  created_at_ms
  updated_at_ms
```

The domain and storage representation may use one
`Option<AuthGrantId>`/`auth_grant_id` field. The API should present a named
credential object so the ownership is clear and secret material can never be
mistaken for an accepted value:

```json
{
  "authPolicy": {
    "type": "requiredOAuth",
    "resource": "https://api.githubcopilot.com/mcp/",
    "scopesDefault": []
  },
  "credential": {
    "type": "authGrant",
    "grantId": "authgrant_work"
  }
}
```

Do not accept a bearer token, refresh token, client secret, direct secret id,
or auth-provider credential on an MCP record. Static bearer tokens already
enter the auth subsystem as `static_bearer` grants, and OAuth access belongs to
refreshable `mcp_oauth` grants. Keeping the server bound only to a grant
preserves broker status, audience, refresh, and revocation behavior.

There is exactly one Authorization credential slot per configured MCP server,
so do not introduce an `mcp_server_credentials` table or a parallel bind/list/
unbind resource API. The credential is part of the existing revisioned,
whole-document MCP server record and changes through `mcp/servers/put` with
`expectedRevision`.

## Session Config And Profiles

Remove `authGrantId` from `McpServerLink` in both `engine` and `api`. The final
session declaration is:

```text
features.mcp.servers[]
  server_id
  allowed_tools?       // session may narrow the catalog default
  approval?            // session behavior override
  defer_loading?       // session behavior override
```

These overrides remain session-owned because they govern what the session may
expose to the model and how provider-hosted tool calls behave. They do not
select a security principal.

Profiles consequently reference only MCP server ids. Profile validation reads
each server and reports whether it exists and is usable; it does not read auth
grants named by the profile. Credential rotation no longer rewrites profiles,
session config, or config revisions.

Fork and clone continue to copy the declared server id and the derived
non-secret MCP tool description. They do not capture a grant id. Every copy
resolves the server's current universe credential when it next sends a provider
request.

## Durable Tool State

Removing the field from `SessionConfig` is insufficient because the current
gateway copies the grant into `RemoteMcpToolSpec.auth_ref`, and the API projects
that reference back through `session/read`.

Replace grant-bearing MCP tool state with a sanitized server reference and a
non-secret auth requirement snapshot:

```text
RemoteMcpToolSpec
  server_id
  server_label
  server_url
  description_ref?
  allowed_tools?
  approval
  defer_loading?
  auth_requirement     // none | optional | required
```

Delete `RemoteMcpToolSpec.auth_ref`. If `engine::SecretRef` and the public
`SecretRefView` have no remaining callers after this change, delete them too.
Do not expose an internal credential handle in `ToolKindView::RemoteMcp`.
Exposing `authRequirement` is acceptable because it is policy, not credential
identity.

`server_id`, URL, policy requirement, and behavioral defaults are the admitted
tool snapshot. The selected grant is live universe configuration and does not
participate in deterministic state or the planned request fingerprint. This is
the same boundary P108 uses for environment credentials: deterministic state
retains resource intent; the activity resolves credential material needed for
the side effect.

## Provider-Send-Time Resolution

Resolve MCP authorization immediately before the provider request is sent.
`llm-runtime` remains independent of the MCP and auth stores behind a narrow
runtime trait, conceptually:

```text
McpAuthorizationResolver.resolve(
  server_id,
  expected_server_url,
  auth_requirement,
) -> Optional<ResolvedSecretValue>
```

The hosted implementation:

1. reads the universe MCP server by `server_id`;
2. refuses to use it if its current URL differs from the admitted
   `expected_server_url`;
3. reads its current `auth_grant_id`, if any;
4. loads and validates the grant through the auth registry/token broker;
5. requests a bearer token for the admitted MCP URL as the audience; and
6. returns the token only in an in-memory redacting value.

The adapters apply these rules:

- `none`: do not resolve or send authorization;
- `optional`: resolve the server credential, but omit Authorization when the
  server is unbound;
- `required`: fail before provider I/O when the server is unbound or the grant
  cannot resolve; and
- resolved: inject the provider-native MCP authorization field into the sent
  request and persist only the existing `<redacted>` placeholder.

Credential rebinding therefore affects all sessions on their next provider
request without reconciling session config or tool state. Endpoint, label,
allowlist, and policy changes remain catalog-document changes that require a
session config re-put to refresh the admitted tool snapshot. The URL equality
check ensures a live credential is never silently applied to a stale endpoint.

If retaining the existing generic `SecretResolver` is materially simpler, its
reference must be `mcp_server:<server_id>`, never
`auth_grant:<grant_id>`, and optional versus required resolution must remain
explicit. The resulting session history must not contain a grant id under any
name.

## Validation And Lifecycle

Validate a supplied MCP credential at `mcp/servers/put` admission:

- the grant id parses and exists in the request universe;
- the grant is `active`;
- `static_bearer` grants are accepted only by optional/required bearer
  policies;
- `mcp_oauth` grants are accepted only by optional/required OAuth policies;
- a grant audience, when present, covers the server's OAuth resource and MCP
  URL according to the existing audience rules; and
- `auth_policy = none` rejects a credential.

Required-auth servers must be registrable before login. A required bearer or
OAuth policy may therefore omit `credential` only while the submitted record
is `needsAuthConfig`; it cannot be admitted as active and usable. Optional-auth
servers may be active without a credential and send unauthenticated requests.

At session config put, resolving a linked catalog record still fails fast for
disabled and `needsAuthConfig` servers. Admission validates any current grant
again so a new session cannot link a server whose grant was revoked after the
server document was written.

At provider-send time, resolution is authoritative. A grant may expire, enter
`needs_reauth`, be revoked, or fail refresh after config admission. Such a
change produces a typed pre-provider failure and never falls back to an
unauthenticated request for required auth. Optional auth may omit a genuinely
absent binding, but an explicitly bound broken grant is an error rather than a
silent downgrade.

Revoking a grant does not rewrite the MCP server record. Reading the record may
still show the bound grant id, while effective use fails until the server is
rebound. Deleting a grant is restricted while referenced. Deleting an MCP
server does not delete its grant because grants remain independent auth
resources and may be referenced elsewhere.

## PostgreSQL Changes In Place

Lightspeed is greenfield. Modify the committed table definitions in place and
reset local infrastructure. Do not add `008_mcp_server_credentials.sql` or any
backfill/compatibility logic.

Add one nullable column to `mcp_servers` in `003_mcp.sql`:

```sql
auth_grant_id text,
```

Add the structural constraint that a no-auth server cannot select a grant:

```sql
CONSTRAINT mcp_servers_no_auth_grant_without_policy
    CHECK (auth_policy <> 'none' OR auth_grant_id IS NULL)
```

Do not require a non-null grant for required auth in SQL because the
`needs_auth_config` pre-login state is valid. Do not attempt provider-kind,
status, or audience checks in SQL; they require auth-record interpretation and
belong at gateway/runtime validation boundaries.

`003_mcp.sql` runs before `004_auth.sql`, so it cannot declare the foreign key
inline. After `auth_grants` is created in `004_auth.sql`, add:

```sql
ALTER TABLE mcp_servers
    ADD CONSTRAINT mcp_servers_auth_grant_fk
    FOREIGN KEY (universe_id, auth_grant_id)
    REFERENCES auth_grants (universe_id, grant_id)
    ON DELETE RESTRICT;

CREATE INDEX mcp_servers_auth_grant_idx
    ON mcp_servers (universe_id, auth_grant_id)
    WHERE auth_grant_id IS NOT NULL;
```

The composite foreign key enforces same-universe ownership. PostgreSQL permits
the nullable reference needed by public and pre-login servers. Universe
deletion continues to cascade through both root tables. Update the migration
comments that currently describe the MCP catalog as policy-only and say that
runtime auth is selected by session handles.

No session table is added or changed. There is intentionally no
`session_mcp_links` table and no universe/session join table for MCP auth.

After editing the historical migrations, run `dev/reset.sh` before
PostgreSQL or Temporal live suites so SQLx migration checksums and the physical
schema match the committed greenfield definition.

## API And CLI

`mcp/servers/put`, `read`, `list`, and `delete` remain the control-plane API.
Put/read/list views include the non-secret credential reference; no endpoint
ever returns token material.

Remove `--auth-grant-id` from `lightspeed mcp link`. Link/unlink remain config
read-modify-put convenience commands and operate only on server ids and
behavioral overrides.

Add credential-focused server conveniences implemented as read-modify-put with
the existing revision guard:

```text
lightspeed mcp server auth set <server-id> --grant <grant-id>
lightspeed mcp server auth clear <server-id>
```

Add an OAuth convenience flow:

```text
lightspeed mcp server login <server-id>
```

It reuses lazy `mcp:<server_id>` OAuth client discovery, completes the generic
auth flow, and CAS-updates that MCP server with the returned grant. Generic
`auth login mcp:<server_id>` may continue to create an unbound grant; it must
not silently replace a universe server credential because that command does
not express rebinding intent.

An auth set/login race must not overwrite a concurrent server edit. Preserve
the full-document `expectedRevision` conflict and require the caller to retry
against the new document.

## Generated Contracts And Documentation

This is a breaking wire change:

- remove `authGrantId` from the session/config schema and every generated
  TypeScript/configurator tool shape;
- add the credential reference to MCP server input and view schemas;
- regenerate JSON Schema, method manifest, OpenRPC, and the human API
  reference with `cargo run -p api --bin export-schema`;
- run both TypeScript consumer checks; and
- update README/design text and P68/P69/P95 historical notes where they still
  prescribe per-session MCP grants, marking that behavior superseded by P110.

Do not retain a deprecated session field. Unknown-field rejection should make
stale clients fail clearly.

## Security, Determinism, And Concurrency

- Auth grants and tokens remain owned by `auth`; MCP stores only a foreign key.
- Tokens resolve only in the worker immediately before provider I/O and never
  enter engine events, Temporal inputs/history, request blobs, logs, Debug
  output, or API views.
- The MCP server id and admitted endpoint may remain in session history; the
  selected grant id may not.
- Rotation is intentionally live external state, like environment credential
  rebinding. Deterministic replay reconstructs which server was selected, not
  which token happened to be current when an external call executed.
- The broker remains authoritative for active status, refresh, expiry, and
  audience enforcement. MCP-specific code must not read token secrets directly.
- Server document CAS protects administrative rebinding. Request-time reads may
  observe either complete server revision, never a partially updated binding.

## Remove Old Behavior

Delete or rewrite:

- `McpServerLink.auth_grant_id` in `engine` and `api`;
- config validation for non-empty link grant ids;
- API projection and config conversion of the link field;
- gateway loading of grants from session MCP links;
- `auth_ref_for_link` and link-oriented grant compatibility naming;
- direct `auth_grant` refs in `RemoteMcpToolSpec` and its fingerprint tests;
- `ToolKindView::RemoteMcp.auth_ref` and `SecretRefView` if otherwise unused;
- CLI `mcp link --auth-grant-id`;
- profile checks that read grants from MCP links; and
- tests, examples, generated schemas, and docs containing session
  `authGrantId`.

Retain and relocate the useful compatibility validation to MCP server put and
provider-send-time resolution.

## Implementation Order

1. Change the MCP domain/API record and edit `003_mcp.sql`/`004_auth.sql` in
   place; reset infrastructure and cover the PostgreSQL round trip.
2. Move grant compatibility validation to MCP server put and return the bound
   credential in MCP server views.
3. Remove the grant from engine/API session config, projections, profiles, and
   CLI link behavior.
4. Replace grant-bearing durable MCP auth refs with server-scoped runtime
   resolution and remove the public active-tool auth ref.
5. Add server auth set/clear/login conveniences and exercise revision races.
6. Regenerate contracts and TypeScript consumers; update documentation.
7. Run unit/integration suites, then serial Temporal and provider-backed MCP
   live coverage using the reset local stack.

## Non-Goals

P110 does not add:

- per-session, per-run, or per-tool-call MCP identities;
- automatic selection among several grants for one server id;
- principal-aware credential switching inside one universe server record;
- direct secrets or provider credentials as MCP credential sources;
- uniqueness constraints on MCP URLs;
- provider-hosted OAuth consent triggered implicitly by session config put;
- a new MCP credential table or binding API; or
- migration compatibility for existing local data.

Callers needing another identity register another server id. More advanced
principal-aware routing requires an explicit future authorization model rather
than reintroducing a grant selector on sessions.

## Done When

- [x] MCP server records own an optional universe auth-grant credential and
      same-universe referential integrity is enforced in PostgreSQL.
- [x] Required, optional, and no-auth policy/binding combinations enforce the
      stated admission rules.
- [x] Two server ids may use the same URL with different grants, and both may
      coexist in one session when their labels differ.
- [x] `SessionConfig`, profiles, engine events/state, request fingerprints, and
      public active-tool views contain no auth grant id for MCP.
- [x] Credential rotation changes an already-linked session's next provider
      request without changing that session's config or config revision.
- [x] Missing, revoked, expired, wrong-kind, wrong-audience, and failed-refresh
      credentials fail before provider I/O without leaking or silently
      downgrading required authentication.
- [x] Public and optional-unbound MCP servers continue to omit authorization.
- [x] OAuth login can explicitly bind its resulting grant to the target MCP
      server with revision-conflict protection.
- [x] `mcp link` and profile validation operate only on server ids and
      behavioral overrides.
- [x] Historical migrations are edited in place, local infrastructure is reset,
      and no compatibility migration or MCP credential table exists.
- [x] Rust tests, PostgreSQL tests, generated contract checks, TypeScript
      checks, serial Temporal tests, and OpenAI/Anthropic MCP live tests cover
      the new ownership and resolution boundaries.

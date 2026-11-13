# P200 — Security hardening

**Status:** Implemented on `security-hardening`, 2026-11-13, uncommitted.
Unit tests, the CI Clippy command and the TypeScript and Python gates pass.
The live suites ran before the last follow-up changes and are run once more
at the end; see [Verification](#verification). The documentation edits wait
for the owner's validation. Not yet done: the version bump, the release note,
and a first run on Windows and against a TLS-protected Temporal frontend.

Greenfield: no compatibility shims and no decoding of earlier shapes. Where a
cheap data migration keeps existing rows working, it is part of the change.

## Goal

Close the places where authority or a secret was granted by omission, was
readable by more callers than intended, or was reachable by a path nobody had
chosen. Each change removes a default that grants, or narrows who can read a
value, without adding a new security feature. The rules that result:

- A key holds exactly the method groups someone wrote down.
- The gateway reads nothing from a caller it has not authenticated.
- A record names the key that acted, not a fragment of its secret.
- A plain credential value reaches a machine only when the credential was
  created for that.
- A secret is never a readable setting of a provider or a trigger.
- File tools never reach the daemon's own secrets or the kernel's view of
  running processes.
- A switch that says a feature is off refuses the feature.

## What changed

### API keys

**Groups are always stated.** `groups` is required on
`deployment/api-keys/create` and `deployment/api-keys/put`. It is a non-empty
list of method groups or the literal `"all"` (`ApiKeyGroups`). `all` is
resolved to the groups the key's scope allows when the key is written, by
`auth::import_api_key`, so the stored set stays concrete and an upgrade never
widens a stored key. Declaring the key again with `all` is what gives it a
group a later release adds. The runtime binary (`api-key create`,
`provision`, `bootstrap`) and the `lightspeed` CLI (`api-key create`, `put`)
take `--group` (repeatable) or `--all-groups`; exactly one is required.
Configuration documents require `groups` in every `apiKeys` entry; `diff`
compares `groups: all` with the stored set by expanding it for the document's
scope, and `export` always writes the concrete list. The Platform admin route
requires a list or `"all"`, and the admin page sends the ticked groups.

**`api-key provision` prints only a secret it generated.** With a secret
supplied through `LIGHTSPEED_BOOTSTRAP_API_KEY` the output has no `secret`
field. The development launcher keeps the value it supplied.

**`/rpc` authenticates before it reads the body.** Authentication is split
into `resolve_caller` (headers only) and `authorize` (per method, pure). In
`authenticated` mode a request without a valid key gets HTTP 401 with
`WWW-Authenticate: Bearer` and a JSON-RPC `unauthenticated` error with a null
id; its body is never buffered or parsed. A key store failure is HTTP 500 the
same way. Everything decided after the body is read is unchanged: HTTP 200
with a JSON-RPC error and the request id. `single` mode and `/hooks` are
untouched. The method name in refusal log lines is cut at 128 characters. Both
SDKs raise the 401 as the typed `unauthenticated` error.

**A key's own requests are attributed to its id.** `Attribution::Key` carries
`keyId` and `scopeKind` (`deployment` or `universe`) where it carried the
display prefix of the secret, which changed whenever the secret was replaced.
The harness mirror, the request context, run start, steering, cancel and
approval events, the `created_by` columns and the transcription owner all
follow. The prefix is kept nowhere in durable records. `initialize` reports
the caller's `keyId`.

**Key management is documented as full authority.** A key that holds
`deployment/api-keys` can declare a key with any authority and replace any
key's secret, so it is equivalent to the bootstrap key. Documentation only.

### Credentials

**The stored-secret binding source is gone.** An environment binding names a
credential or a provider's credential; `directSecret`, the `secret_id` column
and the CLI's `--secret-id` no longer exist.

**Only two things can be bound into an environment:** a credential created as
retrievable, and the API key of a model API key provider. Both rules live in
the `environments` domain crate (`require_bindable_grant`,
`require_bindable_provider`) and are applied when a binding is made and each
time it is resolved for injection. A GitHub App provider is refused with a
pointer to binding an installation credential, which supplies a short-lived
token. The Platform creates environment secrets, imported coding-agent
subscriptions and GitHub installation credentials as retrievable, and the
environment picker lists only what can be bound and says what was left out.
The CLI gained `--retrievable` on `credential put`, `credential login` and
`credential github installation grant`. In configuration documents `exposure`
is a fixed field, and a document that binds a brokered credential or a
non-model provider's credential is refused before anything is written.

**Model endpoint headers are typed, and a configured header is one concept.**
Each extra header of a model endpoint is `literal { value }` or
`credential { grantId }`, the shape MCP server headers already had. Both
documents now use one wire type, `RequestHeader`, and one domain type in
`auth` that holds what the two share: the limits (32 headers, 128-character
names, 4,096-character values), the name and value syntax, the rule a
credential must meet to be named by a header (an active static token whose
audience, when it has one, covers the target URL), and the resolution of a
credential into a sensitive header value. Each owner keeps what is its own:
its table (`mcp_server_headers`, and `auth_provider_headers` with the same
deferrable foreign key to grants), its reserved names, passed into the shared
validation as policy, and its path to the resolver. An MCP tool call goes
through the secret resolver, which reads the server's live record again; a
model request resolves next to the endpoint key, for generation,
transcription and model discovery alike.

`api-key`, `x-api-key` and `proxy-authorization` accept only a credential, on
both owners (an MCP server reserves `proxy-authorization` outright): a literal
is returned to every reader. The rule is enforced in the shared validation and
by a check constraint on each table. A credential named by a header does not
need to be retrievable: the runtime attaches it to its own request. The model
provider CLI gained `--header-credential`; the Platform has one header editor
with a credential picker, used by the MCP server dialog and the Models form.

**MCP OAuth follows the private-network list.**
`LIGHTSPEED_MCP_OAUTH_ALLOW_PRIVATE_NETWORKS` is removed. Metadata, client
registration, token exchange and refresh use `LIGHTSPEED_MCP_PRIVATE_NETWORKS`
through the pinned client, decided for each URL by its own host: public HTTPS
only, unless the host or its resolved address is listed; plain HTTP only
toward a listed network. Two token brokers that built a public-only refresh
client now use the list too, so a refresh behaves the same in a session, in
the default runtime and in an environment lease.

### Agent instructions

**Skill fields are bounded.** A skill's name is at most 128 characters, its
description 2,048 and its short description 256, and none may hold a control
character. A skill outside the limits fails to parse with a typed error and
the other skills still load. The catalog prints every name and description
on one line, so an entry is always exactly three lines, on the workspace path
and the environment path alike.

**The editor no longer switches sourcing on.** Enabling Virtual File System
or Environments in the profile and session editor writes only that feature;
**Prompt loading** and **Skill discovery** are explicit switches. Saved
configurations, the CLI chat defaults and the configuration reference are
unchanged.

### Environment daemon

**File tools refuse the daemon's own places under any root.** Besides the
state directory, the daemon refuses the registration key file it was started
with, a volume mounted to hold that file, and on Linux the `/proc` and `/sys`
filesystems. The set is held by `filesystem::Protected` and applied at every
site that refused the state directory: lookup, read, write, list, walk,
search, glob, capture and publish. Kernel filesystems are recognized by
device, the key file by identity at the time of the request, so another
spelling, a link or a hard link reaches the same refusal and a rotated key is
refused as soon as it is in place. The existing `permissionDenied` /
`daemonState` reason is reused; no reason was added and neither protocol
number moved. What a started command reads is unaffected.

**The daemon loads no `.env` file.** Its settings come from the process
environment and the command line only.

The Kubernetes manifests in the registered-daemon guide set
`automountServiceAccountToken: false`.

### Platform and operations

**The Bots and Channels switches are enforced.** Both features default to
off. The Platform's member client refuses `bots/*` while Bots is off and
`channels/*` while Channels is off, for every role, with the error shape of
the other gate refusals. No migration pins existing universes: a universe
that never stored a choice follows the new default, so its Bots and Channels
are off after the upgrade until an Admin switches them on in the universe
settings. Bots that are already running keep running in the runtime, because
the switch governs only what members do through the Platform. The deployment
guide recommends that a deployment without bots also runs without the `bots`
and `channels` roles.

**The web app shows the real error when an answer is not JSON** (GitHub issue
#155). The `api()` helper parsed every body as JSON before it looked at the
status, so a gateway timeout or a proxy's error page surfaced as "is not valid
JSON". A failed answer that is not JSON now raises `ApiError` with the status
and the shortened text, such as `504: upstream request timeout`; a JSON error
body is handled as before. The session history reader and the sign-in
settings reader use the same guard. The issue's second suggestion, letting
`bots/delete` answer early while the page polls, was left out.

**Remote images are links.** The Markdown renderer never produces an `<img>`
for a source other than a recorded file or media handle; such an image
renders as a link showing its alt text and host. One component covers
transcripts, reasoning and file and blob previews.

**The Temporal connection can be authenticated.** One connection
configuration serves the runtime and the connector host: target, namespace,
optional TLS (server CA file, server name, client certificate and key) and an
optional API key, from `TEMPORAL_TLS`, `TEMPORAL_TLS_SERVER_CA_FILE`,
`TEMPORAL_TLS_SERVER_NAME`, `TEMPORAL_TLS_CLIENT_CERT_FILE`,
`TEMPORAL_TLS_CLIENT_KEY_FILE`, `TEMPORAL_API_KEY` and
`TEMPORAL_API_KEY_FILE`. TLS options are set explicitly on the client.
Inconsistent settings are startup errors, and neither the options nor the key
are logged. With nothing set the connection is what it was.

**Dependencies.** `Cargo.lock` moved `rustls`, `rustls-webpki`, the
`webpki-roots` crates, `tokio` and `ctutils` to current patch releases. No
manifest range changed.

### Model requests

**OpenAI Responses requests are not stored by the provider.** Every request
now carries `store: false`. Each request already holds its whole input and
reasoning comes back encrypted for replay, so nothing depended on the stored
copy. A stored request also cannot repeat a compaction item: the provider
answers the second one with HTTP 409, which failed the second run after a
compaction. The refusal was reproduced directly against the provider's API.
The adapter still passes `store: true` through when its caller sets it; no
session or profile setting does. Chat Completions requests are unchanged.

### Bots

**Trigger secrets left the read views.** `BotTriggerView` no longer has
`ingestPath` or `pairingCode`, so `bots/triggers/read`, `list` and `delete`
cannot carry them. The new `bots/triggers/secrets/read { botId, triggerId }`
returns them and requires bot management (Operator in the Platform's role
table). `bots/triggers/put` answers with `secrets` and `bots/create` with
`triggerSecrets`. The Platform's trigger cards read secrets through the new
route, only for members who may manage the bot, and an edit no longer sends a
pairing code back.

**The trigger tools do not list secrets.** `bot_trigger_list` never returns
an ingest URL or a pairing code. `bot_trigger_put` returns one only when that
call minted it and the calling session is the bot's main session.
`BOT_TOOLS_REVISION` is 13.

## Breaks, and what an installation must do

### Wire

| Change | Who is affected |
| --- | --- |
| `groups` required on key create and put; `"all"` or a list | Scripts, jobs and documents that omitted it |
| `api-key provision` output has no `secret` for a supplied secret | Start-up jobs that read it back |
| HTTP 401 (and 500) before the body, null id | Raw `/rpc` callers; SDK builds from before this change see a transport error |
| `Attribution` key variant: `keyId`, `scopeKind` instead of `prefix` | Readers of `createdBy`, `requestedBy`, `decidedBy` |
| `directSecret` binding source removed | Callers of `environments/credentials/bind` |
| Model endpoint headers are tagged values | Callers of `auth/providers/create` and `put`; documents |
| MCP server headers `api-key` and `x-api-key` accept only a credential | Callers of `mcp/servers/put`; documents |
| Header type named `RequestHeader` in both documents; the JSON is unchanged | Code that imports the generated type by name |
| OpenAI Responses requests carry `store: false` | Anyone who looked up a session's responses at the provider |
| `ingestPath`, `pairingCode` removed from trigger views; new `bots/triggers/secrets/read` | Clients that read them from a trigger |
| `bot_trigger_list` and `bot_trigger_put` result shapes | Bots that configure themselves |

The public API has 148 methods. The API contract, both SDKs, the Configurator
tools, the Platform role table, the workflow contract and the
configuration-document schema were regenerated. The environment protocol
contract was regenerated for one description text; its two protocol numbers
are unchanged.

### Schema

Runtime schema revision 21 (`REQUIRED_SCHEMA_REVISION` and
`LIGHTSPEED_SCHEMA_REVISION`), three migrations:

- **019** deletes stored-secret bindings, drops `secret_id`, and turns every
  brokered credential that an environment binding names into `retrievable`.
- **020** adds `auth_provider_headers` and a check constraint on
  `mcp_server_headers` that refuses a plain value under `api-key`,
  `x-api-key` or `proxy-authorization`. It converts no data: headers of an
  earlier shape in `auth_providers.config_json` are not read, and the
  constraint fails the migration if an MCP server stores a plain value under
  one of those names.
- **021** rewrites stored key attributions in `api_keys`, `auth_grants`,
  `auth_flows`, `sessions`, `bots` and in the five session event kinds that
  record who asked, by looking up the key that holds each recorded prefix. A
  record whose key cannot be found becomes an internal attribution that
  quotes the old prefix.

The Platform schema is unchanged at revision 4. The new feature defaults are
code only; no stored switch is rewritten.

### What to reset or migrate

- **Run `migrate`** for the runtime. The Platform has no new migration. No
  table needs a reset.
- **Switch Bots and Channels on** under **Settings → General → Features** in
  every universe that uses them. Until then their pages are hidden and
  members' bot and channel requests are refused; running bots are not
  stopped.
- **Move to a new Temporal namespace.** Histories are not rewritten. A
  session workflow that received a run request, steering, cancellation or
  approval decision from a key acting without an actor, and a transcription
  job started by such a key, do not replay. Session checkpoints that hold an
  approval decided by a key are rejected at load and rebuilt from the log on
  their own.
- **Before upgrading,** remove the extra headers of model providers, replace
  any plain `api-key` or `x-api-key` header of an MCP server with a token
  credential, and list bindings that name a stored secret directly.
- **After upgrading,** add the model providers' headers again, as credentials
  where they carry a secret; bind a retrievable credential where a stored-secret
  binding was deleted; remove bindings to a non-model provider's credential,
  which block their environment until removed; add `exposure: retrievable`
  to the entry of every credential a configuration document binds (the stored
  credential already is, so no new id is needed).
- **Scripts and manifests:** add `--all-groups` or `--group`; stop reading
  `secret` from `provision` when the secret was supplied; replace
  `LIGHTSPEED_MCP_OAUTH_ALLOW_PRIVATE_NETWORKS` with entries in
  `LIGHTSPEED_MCP_PRIVATE_NETWORKS`; pass `LIGHTSPEED_ENVD_*` settings to the
  daemon explicitly where a `.env` file supplied them; write
  `TEMPORAL_ADDRESS` as host and port.
- **Transcription ids change** for key-owned jobs, so a retry of a job
  started before the upgrade starts a new transcription.
- **Development state:** take a dump before the first start on this branch,
  or reset; point `TEMPORAL_NAMESPACE` at a new namespace either way.

The upgrade guide lists each of these under "Check compatibility before
reusing state".

## Judgement calls

Decisions the implementation made where the plan left room. Each can be
turned without touching the rest.

**API keys**

- `--refresh-groups` was not reintroduced: the flag had been removed before
  this work, and declaring a key again with `all` plays its role.
- The schema of `ApiKeyGroups` has no minimum length, because a minimum makes
  the generated TypeScript type a tuple; the empty list is refused by
  decoding. It declares `uniqueItems` although the server deduplicates a
  repeated group instead of refusing it.
- `groups: all` in a document is expanded with the CLI's own group list, so a
  CLI and runtime of different releases show a pending change.
- `initialize` reports `keyId`, an addition, so a client holding only its
  secret can recognize its own records.
- An unknown method is now reported only to a caller with a valid key, and a
  repeated or non-UTF-8 universe or actor header is part of the 401.
- A key attribution names the key's own scope, not the scope the request
  addressed, and carries no universe id: every attributed record belongs to
  one universe.

**Credentials**

- Migration 019 follows "bound to an environment" literally, so a credential
  bound only by a closed environment also becomes retrievable, and a flipped
  credential can from then on be leased by trusted services as well as
  injected.
- Bindings to a non-model provider's credential are kept by the migration
  and refused at resolution, which fails every command of that environment
  until the binding is removed. `config export` leaves such a binding out.
- GitHub installation credentials granted through the Platform are created
  retrievable, beyond the two cases planned, because the refusal for a GitHub
  App provider points at exactly that credential.
- Migration 020 converts nothing. No installation holds model endpoint
  headers of the earlier shape, so the conversion would only have been code to
  keep.
- The constraint on `mcp_server_headers` is validated against existing rows,
  so a stored plain secret stops the migration instead of staying readable.
- Only the three named headers are credential-only; any other name stays a
  valid literal.
- Header storage stays two tables with their own store code. What is shared
  is the rule, not the rows.
- A credential's token must meet the rule of a written value when it is
  resolved (printable ASCII, no surrounding whitespace, at most 4,096
  characters); a token that does not fails the request without being echoed.
- MCP OAuth does not consult a server record's `allowPrivateNetwork`; the
  list alone decides. A leftover value of the removed switch is ignored
  without a startup error.

**Agent instructions**

- Skill fields fold whitespace and line breaks to one space before the
  control-character check, because a YAML block scalar always carries a line
  break and strict refusal would drop every skill written that way.
- Control characters in a skill's path are printed escaped rather than
  folded, so a catalog entry stays three lines and the path stays readable.

**Environment daemon**

- The key file is recognized by device and inode read when a request starts,
  not by comparing path strings, which a case-insensitive disk or a hard
  link would defeat. A key replaced during one long walk is not seen by that
  walk.
- A volume is refused whole only when the key's directory is a mount of its
  own, and never when it also holds the filesystem root or the working
  directory.
- `/proc` and `/sys` are refused only where each is a mount point. A second,
  separately mounted instance at another path is not recognized.
- A command's working directory follows the rule, as it follows the state
  directory rule; what the command reads does not.
- A capture of a tree that contains the key file is refused, not pruned.
- The description of the `daemonState` reason was widened to name the
  registration key and kernel filesystems, and the contract regenerated.

**Platform and operations**

- Stored feature switches are read key by key, so one unknown entry no
  longer discards the whole stored choice now that the default is off.
- Every universe without a stored choice has Bots and Channels off after the
  upgrade: the ones that exist, and the ones created or adopted later,
  including a universe that already runs bots. Existing universes are not
  pinned to on by a data migration; the project is greenfield and an Admin
  switches the features on where they are used.
- A remote image inside a Markdown link renders as text inside that link.
- Whether an answer is JSON is decided by parsing it, not by its content
  type, which a proxy can mislabel either way. Text that is not JSON is cut
  to one line of at most 300 characters.
- A successful answer that is not JSON is an error naming the status, not an
  empty result: every backend route answers with JSON, so such text comes
  from something in between, such as a sign-in portal.
- TLS toward Temporal is on when `TEMPORAL_TLS=true`, or, with it unset, when
  any TLS setting or an API key is present or the address is an `https://`
  URL. `TEMPORAL_TLS=false` with an API key is allowed and sends the key
  unencrypted, for a link encrypted by other means.
- `TEMPORAL_ADDRESS` is read the same way by both hosts: host and port only,
  a bare host uses port 7233, a URL its scheme's port.
- The runtime has flags for the non-secret Temporal settings; the API key is
  environment-only so it never shows in a process listing.

**Model requests**

- `store: false` is the default of the adapter and not a setting. Provider
  parameters are not part of a session's stored configuration, so there is
  nothing a session could set; the default is stated in the adapter and in
  the context and storage page.

**Bots**

- `bots/create` answers with `triggerSecrets`, since it is the only way a
  caller that creates a bot with triggers in one request learns the values.
- The main session is recognized by the shape of its session id.
- A retried `bot_trigger_put` activity that finds the secret already stored
  shows nothing to the model; Operators still read it.
- A universe key that holds the `bots` group can call
  `bots/triggers/secrets/read`: key groups do not tell reading from managing.

## Verification

**Unit and static checks.**

- `cargo test --workspace --locked`: 3,351 passed, none failed, before the
  final fixes and the follow-up. After the follow-up the crates it touched
  were run again (`api`, `auth`, `mcp`, `store-pg`, `llm-clients`,
  `llm-runtime`, `cli`, `temporal-runtime`): 1,560 passed, none failed. The
  captured provider requests in `llm-runtime` gained `store: false` in the
  six OpenAI Responses entries.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` and
  `cargo fmt --all --check` are clean.
- TypeScript, after the follow-up: `npm run typecheck`; SDK 88,
  Configurator 21, backend 425, web 1,169 tests. Connectors (74) and the
  Platform database (15) ran before it and were not touched since.
- Python client: 43 tests, `mypy`, and the generator's staleness check.
- `scripts/release/verify-metadata.sh` and `npm run check:docs` pass. The
  generated artifacts were regenerated after the follow-up, and the Rust and
  Python staleness tests pass against them.

**Live suites.** Two runs. The suites listed below ran, each serialized,
against a database and a Temporal namespace created for this work, before the
follow-up that unified request headers, reduced migration 020 and changed the
OpenAI request. After the follow-up, a short final run on a freshly reset
development database covered exactly what the follow-up touched, and all of
it passed: the migration ledger with migration 020 in its final form, the
header round trips for `auth_provider_headers` and `mcp_server_headers`, the
MCP custom-header test (a credential header reaches the server, a plain
`X-Api-Key` is refused), the OpenAI standalone-compaction test that failed
with the provider's 409 before requests stopped being stored, and the OpenAI
Responses and Completions tool-call round trips, which show that reasoning
replay works without stored responses. The two Anthropic session tests that
had failed on an unfunded account pass as well. The configuration-document
suite was not repeated after the follow-up.

- Store: the migration ledger to 21, an upgrade suite for migrations 019 and
  021, keys, grants, providers and endpoint headers, MCP server headers,
  environments and bindings, bots, sessions.
- Runtime: authentication and authorization over real HTTP, declared keys,
  universes and tenancy, runs, bots, channels, MCP and MCP OAuth, environment
  providers and registration, file transfer, Code Mode capabilities,
  transcriptions, session restart, sessions.
- CLI: `config_live`, run again after the final fixes.
- By hand against a started runtime binary: the `provision` output, the 401
  for a large non-JSON body with a bad key, attribution of a CLI-made
  credential, a brokered credential refused at bind and a retrievable one
  injected into a job on a real daemon, a credential header reaching a
  recording server on model discovery.

Three tests failed in that run, all in `sessions_live`. Two Anthropic tests
failed because the provider account had no credit. The OpenAI
standalone-compaction test got HTTP 409 from the provider on the run after
compaction. That one has a cause in the request and not in this work: a
stored request cannot repeat a compaction item. It is what `store: false`
answers, and the final run is its proof.

The Platform migration check (`npm run test:migrations`) passes with the four
migrations the Platform has.

**Other platforms.** The daemon's tests ran once in a Linux container
(aarch64, Debian, Rust 1.99.1): 166 unit and 6 round-trip tests pass,
including the Linux-only test that `/proc` and `/sys` are refused under any
root. The daemon type-checks for `x86_64-pc-windows-gnu` with all targets.

**Not verified.**

- Nothing ran on Windows, and the release target there is MSVC, which was
  not built. No Linux run on x86-64 or under musl.
- A real Kubernetes Secret volume: mount detection through its link layout,
  and rotation.
- TLS, a client certificate or an API key against a Temporal frontend, in
  either host. Only the unencrypted path is proven. The server-name override
  together with DNS load balancing is unexercised.
- Refusal of a GitHub App provider's credential at bind and at resolution,
  and a brokered credential at resolution: unit tests only.
- The migrations ran on an empty database and on synthetic rows, not on an
  installation's data. The pass of migration 021 over `session_events` is
  sequential and was not timed on a large table.
- That a runtime started without the `bots` and `channels` roles runs no bot
  work is taken from the role wiring, not from a running stack. A bot
  created through the API in such a deployment leaves a workflow start
  queued.
- `runs_live_slow`, the Azure, TLS PostgreSQL and Incus suites, and the
  provider suites of the model client crates.
- The request header editor in a browser, in either form that uses it.

## Left out

Decided before the work started and not reopened:

- `single` mode is not restricted further.
- Internal error text stays in API responses.
- Registration keys keep an optional daemon limit.
- The daemon's filesystem root stays `/` by default.
- Bot self-configuration, event scoping and pairing codes are unchanged
  beyond the two trigger-secret changes.
- The Configurator is unchanged. Its regenerated tools follow the API.
- Prompt and skill sourcing semantics are unchanged; only the editor default
  and the field limits moved.
- No new security feature was added.

## Open

For the owner to decide or do; none blocks the code.

- **Release.** `LIGHTSPEED_PRODUCT_VERSION` is still 0.8.0, which is tagged.
  The version bump, `scripts/release/workflow-compat.sh` and a release note
  for a breaking release with a new Temporal namespace are release-time
  steps.
- **Windows.** Run the Windows daemon build against this branch before
  merging; pull-request CI builds Linux and macOS only.
- **Trigger secrets and self-configuration.** A member who may send a bot an
  event can have a self-configuring bot delete and re-create a trigger, and
  the new URL or code is then shown once in the Main conversation, which
  every member can read. Either never show a secret for a trigger name the
  bot had before, or state the consequence in the bots guide.
- **Configurator.** It now generates a tool for
  `bots/triggers/secrets/read`. Excluding it is one line in
  `tool-filter.json` and a regeneration; the answers of `bots/triggers/put`
  and `bots/create` reach it either way.
- **Non-MCP OAuth token requests** (custom and model OAuth) use a plain
  client: no address pinning and no private-network list. Earlier behaviour,
  outside this change.
- **Working directories.** To exempt a command's `cwd` from the new daemon
  refusals, resolve it with a lookup that applies only the state-directory
  rule.
- **Installation credentials.** The Credentials page could let the creator
  choose exposure for GitHub installation credentials instead of always
  creating them retrievable.
- **Key flags.** With neither flag given, the usage line names only
  `--group`. A clap argument group would name both.
- **Small cleanups.** `uniqueItems` on `ApiKeyGroups`;
  `HttpOAuthTokenClient::new()` has no caller left; the web's local
  `BotTriggerSecrets` type duplicates the SDK's; the zero-size read branch of
  the daemon's `read_file` has no test; the refusal message for a refused
  key volume does not say which of the two cases applies; the `code` role is
  missing from the `LIGHTSPEED_ROLES` reference row and the self-hosting
  example.
- **GitHub issue #155.** The fix for non-JSON answers is in; the issue also
  suggests that `bots/delete` answer early and the page poll, which was not
  done. Closing the issue or keeping it open for that part is the owner's
  call.
- **Test state left in place.** Database `lightspeed_sechardening`, Temporal
  namespace `lightspeed-sechardening` (workflows without a worker) and the
  object-store prefix of the same name are no longer used and can be deleted.
  The development database was reset for the final run and is at revision 21.

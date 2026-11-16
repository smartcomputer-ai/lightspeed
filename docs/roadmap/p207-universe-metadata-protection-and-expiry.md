# P207 — Universe metadata, protection and expiry

**Status:** Runtime mechanism and Platform protection policy implemented,
2026-11-16. Compilation is passing; behavioral and live tests remain on hold.

This document began as the design for creators and self-service universes,
with a creator level built into the runtime as a third kind of API key. That
part has been taken out. Accounts contain universes, and creation and lifecycle authority are now
scoped roles, designed in
[Platform authentication and authorization](p209-platform-authentication-and-authorization.md),
after the runtime's own key model is removed in
[runtime access simplification](p208-runtime-access-simplification.md).

What remains here is the part that belongs to the runtime whoever is
allowed to use it: three facts about a universe. It builds on
[universe deletion](p206-universe-deletion.md).

## Problem

A deployment is used as a platform. People spin up a universe for a test and
remove it again, run tests that create dozens of universes, keep a universe
for a team for a while, and deploy long-lived universes from configuration
documents.

The universe record has no place for what this needs: what a universe is
for, whether it should go away by itself, and whether it must not be deleted
casually. A script that cleans up test universes cannot tell them from the
one deployed from a document.

## The fields

| Field | Meaning | Set with |
| --- | --- | --- |
| `metadata` | A bounded string map for marks such as team, purpose or pipeline run. | Creation, `configuration/put`, documents |
| `protected` | The universe cannot be deleted while this is set. | Creation, `configuration/put`, documents |
| `expiresAtMs` | When the universe is deleted by itself. Absent means never. | Creation, `expiry/put` |

`deployment/universes/read` and `list` return all three, and `list` gains a
`metadata` containment filter. The record of a deleted universe keeps its
metadata.

The runtime holds no account or principal ownership. Platform records the
containing account and grants the Owner role to one or more principals; there
is no unique principal owner.

### Metadata

The same map as on sessions and environments, with the same validator and
bounds: 32 entries, keys up to 64 bytes, values up to 256 bytes, and no
`lightspeed.` prefix for callers. A put replaces the whole map. The list
filter is containment, as on `session/list`.

Metadata is for people and their tools to read and filter by. Nothing in the
runtime acts on it.

### Protection

`protected` is a runtime resource guard. Platform separately authorizes who
may change it; the flag itself grants no permission.

- `deployment/universes/delete` is refused with `rejected` while it is set.
  The refusal is in the runtime, so it holds for the CLI, the web app, an
  SDK script that loops over universes, and expiry alike.
- Through Platform, universe Owners may delete unprotected universes and
  enable protection, but cannot clear protection. Only an Admin of the
  containing account or a Platform Admin may clear it, subject to account
  policy. Even administrators first clear the flag explicitly; deletion
  has no bypass option.
- Account policy can default to or require protection. Mandatory protection
  must be changed at the account-policy level before unprotection is allowed.
- Direct runtime callers retain full infrastructure authority and may clear
  the flag; the runtime never duplicates Platform role checks.
- A protected universe cannot have an expiry, and an expiring universe
  cannot be protected. One of the two has to be cleared first.

Universes deployed from documents are the ones this exists for:

- A document may state `protected: true` or `protected: false`, and `apply`
  then keeps the universe that way only if the caller and account policy
  permit it. Clearing protection through a document requires the same
  administrative authority as clearing it through the UI or SDK. An explicit
  false on creation is refused when account policy requires protection.
- When a document leaves it out, `apply` protects a universe it creates and
  otherwise leaves the field alone. A universe that was created first and
  configured from a document afterwards, such as an expiring test universe,
  is therefore not protected by accident.

### Expiry

A universe with `expiresAtMs` is ephemeral. The deletion pass from
[universe deletion](p206-universe-deletion.md#the-deletion-pass) also looks
for universes whose time has passed and starts deleting them, with the
request attributed to the runtime.

- Activity does not extend it. It is extended or cleared with `expiry/put`.
- A new expiry must lie in the future.
- The record of the deleted universe shows that it expired.

Expiry is opt-in. A universe without it stays until someone deletes it.

## Command line

```bash
# A test run: create, configure, use, remove.
lightspeed universe create --handle "t-$RUN" --expires-in 2h --metadata purpose=ci
lightspeed --universe "t-$RUN" config apply -f test-universe.yaml
# … run the test …
lightspeed universe delete "t-$RUN" --yes
```

If the run crashes before the last line, the universe expires.

| Command | Does |
| --- | --- |
| `universe create [--handle H] [--expires-in D] [--metadata k=v]… [--protected]` | Creates a universe. |
| `universe list [--metadata k=v]… [--deleted]` | Lists universes with protection and expiry. |
| `universe protect U`, `universe unprotect U` | Sets or clears protection. |
| `universe set-expiry U <duration\|never>` | Sets or clears the expiry. |
| `universe metadata put U k=v…` | Replaces the metadata. |
| `universe delete U` | From universe deletion. Refuses a protected universe and names `unprotect`. |

## Who may use them

The runtime does not decide caller roles. With a direct connection, a caller
holding a runtime token may set all three, subject to resource invariants.
Through Platform, universe Owners and scoped administrators manage metadata,
expiry and enabling protection; only the containing Account Admin or a
Platform Admin may clear protection, subject to account policy. See
[Platform accounts and authorization](p209-platform-authentication-and-authorization.md#deletion-protection).
The same rules apply to the web app, CLI, SDK and configuration documents.

## Storage

Runtime, one migration after the one for universe deletion, with
`release/metadata.env` aligned:

- `universes`: `metadata_json` with the containment index used on sessions,
  `protected` (false by default), `expires_at_ms`.
- `universes_deleted`: `metadata_json`.

The API contract, the configuration document schema and every generated
consumer are regenerated.

## Slices

1. **The three fields,** their methods and the list filter.
2. **Expiry in the deletion pass,** and delete refusing a protected
   universe.
3. **The CLI commands and the document fields,** including `apply`
   protecting a universe it creates.
4. **Documentation,** for review before it lands: managing universes, the
   CLI guide, configuration as code.

## Tests

- Metadata bounds, replacement and the containment filter.
- Protection and expiry: neither can be set while the other is, and delete
  is refused while protected.
- An expired universe is deleted by the pass and its record says so. A
  protected universe is neither deleted nor expirable.
- `config apply` protects a universe it creates when the document does not
  say otherwise, keeps an explicit value, and leaves an existing universe's
  protection alone when the document is silent.
- Through Platform, an Owner cannot clear protection using configuration
  updates or `config apply`; Account Admin authority is scoped to the
  containing account. Mandatory account protection refuses explicit false.
- Administrative deletion has no protected-flag bypass; it requires the
  explicit authorized unprotection step.

## Decisions

Decided, 2026-11-16:

- **The universe record gains `metadata`, `protected` and `expiresAtMs`.**
  Metadata reuses the map sessions and environments have.
- **Protection is enforced by the server,** not only by the CLI.
- **`config apply` protects a universe it creates,** unless the document
  says `protected: false` and account policy permits it.
- **Platform controls unprotection:** only the containing Account Admin or a
  Platform Admin may clear it; universe Owner is insufficient. This supersedes
  the earlier rule that anyone allowed to delete could unprotect first.
- **Expiry is a time on the universe.** There is no maximum lifetime
  attached to a credential.
- **Creator keys and an owner in the runtime are dropped.** Creators are
  rebuilt on roles.

## Not doing

- Rules driven by metadata. Metadata is read and filtered, never acted on.
- An owner, a creator level, or limits in the runtime.

## Validation and progress

- 2026-11-16: written as the design for creators and self-service
  universes. Reduced the same day to the universe fields, after the decision
  to decide access in one place outside the runtime. No code changed.

- 2026-11-16: aligned with the agreed account hierarchy and multiple universe
  Owners. Retained the runtime boolean and document-creation default; added
  the Platform administrative restriction on unprotection and account policy.
  Documentation only; implementation and tests remain pending.

- 2026-11-16: implemented metadata, protection and expiry in runtime DTOs,
  storage, HTTP bindings, generated clients and CLI commands. Runtime migration
  028 and release schema revision 28 add the fields and constraints. Protection,
  expiry extension and deletion admission share a row lock. Automatic expiry
  uses standard deletion and retains metadata with internal expiry attribution.
  Configuration apply protects new universes by default and preserves an
  existing setting when omitted. Platform enforces Account Admin unprotection
  and mandatory account policy. Unit and live scenarios were authored; none
  were executed. `cargo check --workspace --all-targets --locked` passed.

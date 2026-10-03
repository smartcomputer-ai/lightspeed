# P133 — Retrievable Grants: Service Leases for Trusted Workers

**Status**

- In progress 2026-08-25. Slices 1 and 2 are implemented: fields folded into
  the pre-release `004_auth` baseline,
  immutable exposure on grants and OAuth flows, lease audit counters,
  `auth/grants/lease` through the existing refresh/mint broker, service method
  scope and HTTP-edge caller gating, generated Rust/TypeScript contracts,
  Configurator exclusion, and principal-header documentation. Bots now keep
  only grant references for HTTP poll authentication and webhook HMAC,
  validate references through core, lease at the execution boundary with a
  process-local single-flight cache, and retry once after poll 401/403. The
  Secrets UI creates retrievable grants explicitly and shows lease audit
  metadata. Existing CLI creation paths explicitly remain brokered. Channels
  and the later CLI commands remain open.
- Proposed 2026-08-24 from the bots secret-sealing discussion. P130 deferred
  sealing webhook secrets; P131 ruled "broker-backed credentials … do not
  build a parallel secret store" for poll auth. Direction from Lukas: a
  grant may be tagged **at creation** as retrievable, trusted service
  workers read it through the API and cache it in memory, and the pattern
  is expected to recur (Channels credentials, the email trigger, pollers,
  future integrations).
- Builds on P69 (grants, encrypted secret store, token broker — whose own
  rule already permits "a short-lived lease to a trusted runtime boundary"),
  P90 (principals, `trusted-header` / `api-key` modes), P110
  (universe-owned auth), and the P125 credential-injection precedent, where
  resolved credentials already leave core into environment jobs.

## Why

Credentials leave core today in exactly two ways: inside worker activities
(LLM and MCP calls) and by injection into environment jobs. Platform-tier
workers have neither, so they keep their own plaintext: webhook HMAC secrets
and poll headers in `bot_triggers.spec` jsonb, and a `credential_ref` column
on channel accounts with nothing behind it. Every new source repeats the
problem.

Two shapes were weighed:

- **Proxy primitives** ("fetch this URL with grant X"): core acts on the
  worker's behalf and the value never leaves. Strictest, but one primitive
  per protocol — HTTP now, IMAP/SMTP for email, then whatever comes next.
  Right for *model-facing* use, where the model must never see the value;
  wrong economics for first-party workers.
- **Leases**: the worker asks for the current token of a grant it is
  allowed to read, uses it directly, caches until expiry. One method,
  every protocol. Correct for *service-facing* use.

The trust reality favours leases: the platform is first-party and already
holds these values, so a lease is a strict improvement — one encrypted
store, one revocation switch, centralized OAuth refresh, one audit trail —
provided the widening is contained: creation-time only, service callers
only, never through a model-facing surface.

## Design

### 1. `exposure` on the grant

`AuthGrantRecord.exposure: brokered | retrievable`, default `brokered`.
Set at creation, never updated: there is no grant put/update method today
and P133 does not add one; a brokered grant that needs to become readable
is re-created. Persisted as `auth_grants.exposure text NOT NULL DEFAULT
'brokered'` with a CHECK in the pre-release `004_auth` baseline; surfaced on
`AuthGrantView`.

Creation surfaces gain an optional `exposure` param: `auth/grants/import`
(static tokens — the main case), `auth/flows/start` (carried on the flow
and stamped on the grant it mints) and `auth/github/installations/grant`.
Model-credential grants created through `lightspeed auth model add|bind`
stay brokered and take no param.

### 2. `auth/grants/lease`

```text
auth/grants/lease { grantId, audience? }
  -> { token, expiresAtMs?, grantId, providerKind }
```

- Rejected (`rejected` kind) unless `exposure = retrievable`.
- Resolves through `AuthTokenBroker::bearer_token`, so status, expiry and
  audience enforcement, per-grant single-flight OAuth refresh and GitHub
  App minting apply unchanged. Static grants return the stored value; OAuth
  grants return the access token — never the refresh token.
- `expiresAtMs` is the grant expiry for static grants (absent when
  unbounded) and the token expiry for refreshed or minted ones.
- `audience` follows the broker's rules: required where the provider kind
  demands one (GitHub API base, MCP resource), otherwise optional; a
  mismatch maps to `rejected` like other broker errors.
- The response DTO redacts `token` in `Debug` (the
  `OperatorApiKeyCreateResponse` pattern); gateway logging never echoes
  lease results — the rule P69 set for import params, applied outbound.

### 3. Caller class: a `service` method scope

This is the genuinely new piece of the auth model. The manifest gains
`MethodScope::Service` beside `universe` and `operator`: universe-scoped,
but admitted only for service callers.

| Auth mode | Caller | Lease |
|---|---|---|
| `single` | no identity — one trust domain | allowed |
| `trusted-header` | `x-lightspeed-principal: service_account:<id>` | allowed |
| `trusted-header` | `user:<id>`, bare id, or no principal | rejected |
| `api-key` | key principal kind `service_account` | allowed |
| `api-key` | key principal kind `user` / `universe_default` | rejected |

Fail closed, gated at the HTTP edge beside `authorize_operator_call`. The
`method_names_carry_their_scope_prefix` contract test narrows to operator
methods so `auth/grants/lease` keeps its natural name.

Consequences that fall out structurally:

- **Configurator MCP** generates tools only from `scope === "universe"` and
  already asserts operator methods never leak; the assertion extends to
  `service`. A model can never call lease.
- **Browsers** go through the platform proxy, which carries either no
  principal or `user:<id>` (`engineClientFor(ctx, universe, principal)`);
  neither leases. The Secrets page shows a badge, never a value.
- **Platform workers** identify themselves: the Bots and Channels activity
  workers and the platform backend send `service_account:lightspeed-bots` /
  `-channels` / `-platform` (audit identity, not a credential). In
  `api-key` deployments the platform mints service keys through
  `operator/api-keys/create` with a `service_account` principal, which it
  already accepts.

### 4. Audit

`auth_grants.last_leased_at_ms` and `lease_count`, bumped per lease and
shown on `AuthGrantView` and the Secrets page. Tracing logs grant id and
principal, never the token. This is the one path where secrets leave on
request, so it must be the most visible one.

### 5. Caching contract (documented on the method)

Callers cache **in memory only** until `expiresAtMs − margin`, or a bounded
TTL (5 minutes) for static grants without expiry so revocation propagates;
re-lease on a 401/403 from the target; never persist a token; never place
one in a Temporal payload — workflow code holds grant ids, activities lease
at use time.

### 6. Consumers

- **Bots poll (`http`)**: `source.auth?: { grantId, header?, scheme?, audience? }`
  (default `authorization: Bearer <token>`) replaces credential-bearing
  `headers`; non-secret headers remain. The fire activity leases with a
  per-process cache. `bot_trigger_put` accepts `grantId` references only —
  a session references, never creates or leases.
- **Bots webhook**: `verification: { scheme: "hmac-sha256", grantId,
  header, prefix? }` replaces the inline `secret`; the ingress route leases
  as `service_account:lightspeed-platform`. The URL `token` stays an address,
  not a credential (P130's assessment), but gains rotation; hashing it
  (shown once, like an API key) is optional and cheap.
- **Greenfield**: plaintext `secret` and credential-bearing `headers` fields
  are removed, not migrated; existing triggers are re-entered. Non-secret
  poll headers remain, and legacy credential headers fail closed at runtime.
- **No Platform credential shadow catalog**: triggers reference core grant ids
  directly. Creating or editing a trigger performs a metadata-only core read
  and requires an active `retrievable` grant in the same universe. The
  creation-time exposure choice is the authorization boundary: the Bots
  service principal may lease any retrievable grant in its universe. If
  finer-grained policy is needed later, add it to core rather than duplicating
  grant ownership in a `bot_credentials` table.
- **Channels**: `channel_accounts.credential_ref` becomes a grant id and
  connectors lease it (nothing in `platform/channels/src` reads that column
  today; confirm before wiring).
- **Secrets page**: an explicit "retrievable by service workers" choice at
  creation with a warning; badge and last-leased column in the list. CLI:
  `lightspeed auth grant import --retrievable` and `lightspeed auth grant
  lease <id>` for operator debugging.

## Threat model, stated plainly

The lease widens where plaintext can reach from "core process and
environment jobs" to "any service principal with universe access". That is
acceptable because the platform already holds these values, and contained
because: exposure is chosen once, at creation, per grant; `brokered` is the
default and the only option for model credentials; user- and model-facing
callers are structurally excluded; every lease is counted. The residual
risk is drift — a future caller reaching for `retrievable` because it is
easier than injection. Review a new `retrievable` creation site the way you
would review a new `unsafe` block.

## Slices

1. **Core** (1–2 days): column, record/view, creation params; `lease`
   through the broker; `Service` scope and edge gating; configurator
   assertion; contract regeneration and TypeScript client; document
   `x-lightspeed-principal` in `docs/variables.md` (undocumented today).
2. **Platform** (complete 2026-08-25): service principals at the poll and
   webhook lease boundaries; bots poll `auth` and webhook `grantId`; UI and
   `bot_trigger_put`; plaintext fields removed. Legacy credential headers fail
   closed at execution and existing plaintext triggers must be re-entered.
3. **Later**: Channels credentials on leases; CLI subcommands.

## Tests

- Gating matrix: every (mode × principal kind) row above, fail-closed
  default; `single` mode allowed.
- Exposure: default brokered from every creation path; a brokered grant
  leases to `rejected`; no code path flips exposure.
- Lease path: static grant value; OAuth grant refreshed through the
  broker's single-flight lock (existing broker tests extended); revoked,
  expired and audience-mismatch map to typed errors; the refresh token
  never appears in any response.
- Redaction: lease response `Debug` output contains no token.
- Contract: `cargo test -p api` with the new scope; configurator generation
  fails if a `service` method reaches the toolset.
- Platform: Bots poll fire against an injected `fetch` asserting the leased
  header; webhook verification via `grantId`; the integration suite asserts
  no secret material in `bot_triggers.spec`.

## Non-goals

- A raw `secrets/read` — the secret store stays internal; leases are on
  grants, which carry status, expiry, revocation and refresh.
- Flipping exposure after creation, or a grant update method.
- Proxy primitives for model-facing use (a session tool fetching with a
  grant) — a separate item if wanted; unrelated to service leases.
- Per-universe master keys / KMS envelopes (P90 non-goal, unchanged).
- Exposing leases to sessions, profiles, or Configurator MCP in any form.

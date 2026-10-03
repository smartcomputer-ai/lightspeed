# P143b — MCP OAuth on `rmcp`

**Status**

- Implemented 2026-08-31. The protocol, persistence, contract, UI, broker, and
  database-backed live acceptance suites pass. A public third-party OAuth MCP
  remains an optional compatibility smoke test rather than a correctness
  dependency.
- Builds on P69 (the generic auth broker), P110 (universe-owned MCP
  credentials), and [P143](p143-mcp-tool-discovery.md) (the bounded `rmcp`
  Streamable HTTP client and live inventory UX).
- Should land before [P145](p145-native-mcp-execution.md), whose native MCP
  client will need the same challenge, scope-upgrade, refresh, and
  reauthorization behavior. It is independent of
  [P144](p144-mcp-approvals.md).

## Implementation Record

The implementation landed as the greenfield breaking refactor described here:

- `crates/auth/src/safe_http.rs` is the shared DNS-pinned, redirect-validating,
  bounded HTTP seam used by both MCP inventory and MCP OAuth. The existing
  private-network development switch applies consistently to both paths.
- `crates/auth/src/mcp_oauth.rs` now uses `rmcp` 3.1.4 for protected-resource
  and authorization-server metadata, challenge parsing, CIMD/DCR, PKCE and
  authorization state, code exchange, and refresh. Legacy endpoint fallback
  and the replaced MCP wire parsers are gone.
- Lightspeed still owns durable clients, one-time flows, encrypted secrets,
  grants, audience checks, refresh single-flight, rotation, and status. The SDK
  receives transient credentials only for the operation the broker admits.
- Migration 011 retains authorization-server issuer, RFC 9207 support, and
  advertised scopes on clients, plus the expected issuer requirement on each
  in-flight flow. Raw state remains hashed and PKCE verifier/client/token
  secrets remain in `SecretStore`.
- MCP discovery maps SDK-parsed `invalid_token` and `insufficient_scope`
  challenges to `grantNeedsReauth` and `additionalConsentRequired`; suggested
  scopes are diagnostics and never mutate authored consent.
- The generated API/TypeScript contracts and the Platform backend editor expose
  current advertised scopes and typed additional-consent diagnostics. Manual
  preregistration remains available through the API and CLI.

Verification completed locally:

- `cargo test -p auth` (85 passed);
- `cargo test -p api` (82 passed, including generated-contract freshness);
- `cargo test -p store-pg` (unit suite passed; database tests remain opt-in);
- `cargo test -p temporal-runtime --lib` (249 passed, one unrelated ffmpeg
  smoke test ignored), plus the scope-upgrade transport fixture;
- `cargo test -p temporal-runtime --test mcp_oauth_live -- --ignored
  --test-threads=1` (database-backed OAuth/MCP round trip passed);
- `cargo check --workspace --tests`;
- TypeScript generation, typechecking, 206 tests, and live/demo production
  builds.

The self-contained live suite uses real Postgres persistence and a local
standards-shaped OAuth/MCP server. It covers migration 011, SDK discovery and
DCR, PKCE and resource binding, cross-instance callback completion, RFC 9207
issuer refusal, callback replay refusal, audience refusal, concurrent broker
refresh with refresh-token rotation, authenticated tool discovery,
`insufficient_scope`, and terminal refresh rejection transitioning the grant
to `needs_reauth`.

## Decision

Use the official Rust MCP SDK's OAuth implementation as Lightspeed's **MCP
OAuth protocol engine**, behind Lightspeed's existing durable auth stores and
token broker.

Do not replace the generic Lightspeed OAuth subsystem with `rmcp`, and do not
let an SDK-owned in-memory credential store become an authentication
authority. The split is:

| `rmcp` owns | Lightspeed owns |
| --- | --- |
| MCP protected-resource and authorization-server discovery | universe and principal ownership |
| `WWW-Authenticate` challenge interpretation | durable clients, flows, grants, and encrypted secrets |
| MCP client selection and registration rules | management API, consent UI, and callback routing |
| PKCE and MCP authorization/token wire behavior | one-time flow consumption and cross-instance reconstruction |
| MCP resource, issuer, scope, and refresh semantics | audience checks, status, revocation, leases, and audit |
| future MCP OAuth spec evolution | cross-process refresh single-flight and rotation-safe secret replacement |

This removes hand-maintained MCP OAuth protocol code without creating a second
credential lifecycle.

## Why Lightspeed OAuth Remains

`crates/auth` is not an MCP-specific implementation. Its stores and broker are
also used for:

- custom OAuth clients and flows;
- model-provider OAuth bindings;
- static bearer and GitHub App token sources;
- environment credential injection;
- encrypted access, refresh, client, and PKCE secrets;
- universe/principal scoping, audience enforcement, grant status, revocation,
  retrievable leases, and audit;
- cross-process single-flight refresh and atomic secret rotation.

Replacing this with `rmcp::transport::auth::OAuthState` would lose the
product's durable, multi-instance security boundary. The SDK's default
credential and state stores are useful for a resident MCP client, but they do
not directly model Lightspeed's split records and encrypted secrets. In
particular, an auth flow must survive a process restart and its callback may
reach a different gateway instance from the one that started it.

The goal is therefore composition: use `rmcp` for facts defined by the MCP and
OAuth specifications; retain Lightspeed for facts defined by the hosted
product.

## Current Baseline and Gaps

`crates/auth/src/mcp_oauth.rs` currently hand-maintains protected-resource
metadata discovery, authorization-server metadata discovery, pre-registered
client selection, Client ID Metadata Documents, Dynamic Client Registration,
and related response validation. `oauth.rs`, `flow.rs`, and `broker.rs` provide
the generic durable flow and token lifecycle around it.

That split has worked, but continuing to grow the MCP-specific half would make
Lightspeed responsible for tracking a fast-moving protocol. The current
implementation also lacks or incompletely models several behaviors already
covered by the SDK or required by the current MCP OAuth profile:

- reactive discovery from `WWW-Authenticate` and later scope-upgrade
  challenges;
- authorization-response issuer (`iss`) validation;
- the complete MCP resource/audience binding across authorize, exchange, and
  refresh;
- advertised-scope retention and deliberate scope escalation;
- current client-selection and registration details, including DCR
  `application_type`;
- consistent bounded response handling across every OAuth metadata request.

There is also an immediate transport concern: the existing OAuth metadata
client follows redirects but does not share P143's DNS-pinned private-network
policy or decoded-response byte limits. P143b must close that gap before
adding more discovery behavior.

## Target Architecture

### One safe outbound HTTP seam

Extract the P143 outbound protections into a reusable runtime-owned HTTP
component and adapt it to `rmcp`'s OAuth HTTP-client seam:

- validate `https` by default, with the existing explicit local-development
  exception;
- resolve and pin DNS for each allowed hop;
- reject loopback, link-local, private, multicast, unspecified, and other
  deployment-forbidden addresses before connecting;
- revalidate every redirect rather than delegating redirects to `reqwest`;
- bound redirect count, total wall time, header bytes, body bytes, and decoded
  JSON depth;
- redact credentials and remote bodies from logs and public errors;
- keep deployment policy capable of explicitly allowing private MCP egress
  later for P145 without weakening the public control-plane default.

Both P143 tool discovery and P143b OAuth discovery should use this component.
It remains outside `harness`, workflows, and the provider adapters.

### Discovery and client registration

Replace Lightspeed's MCP-specific PRM/authorization-server parsing and client
registration decisions with `rmcp::transport::auth::AuthorizationManager` (or
the narrowest stable SDK layer that exposes the same protocol behavior).

The selection order remains current MCP behavior:

1. an operator-authored pre-registered client when present;
2. Client ID Metadata Documents when supported;
3. Dynamic Client Registration when supported.

Keep the existing manual registered-client path. It is required for
authorization servers that disable dynamic registration and is a product
configuration choice, not protocol code.

Do not retain the legacy authorization-endpoint fallback. Lightspeed is still
greenfield and supports current MCP OAuth only; incomplete discovery should
produce a typed configuration error rather than silently switching to an old
profile.

If the SDK does not expose enough of the selected client or protected-resource
metadata to persist and reconstruct a flow safely, contribute the smallest
general-purpose API upstream. Do not fork `rmcp` or copy its parser back into
Lightspeed merely to reach private fields.

### Durable authorization flow

`OAuthFlowService` remains the lifecycle coordinator. Starting an MCP OAuth
flow should:

1. resolve the server record and validate the requested MCP resource;
2. run SDK-owned discovery and client selection through the safe HTTP adapter;
3. ask `rmcp` to produce the authorization request, PKCE material, issuer
   expectation, resource, and selected scopes;
4. persist only the durable representation needed to finish the flow;
5. keep verifier, client secret, and any other secret material in
   `SecretStore`; store only hashes/indexes where the callback lookup requires
   them;
6. redirect the user through the existing management API and consent UX.

The callback should atomically consume the Lightspeed flow, validate state and
the authorization-response issuer, reconstruct the SDK operation from durable
state, exchange the code, and write the resulting grant and encrypted token
secrets. Replays must fail even when two gateway instances receive the callback
concurrently.

Do not persist an opaque SDK struct solely because it is convenient. Persist a
small versioned Lightspeed DTO containing the protocol outputs that must
survive restart. At minimum, review whether the current records need:

- the expected authorization-server issuer;
- protected-resource and registration provenance;
- selected token-endpoint authentication method;
- the effective requested scopes and resource;
- a versioned encrypted SDK authorization-state payload, only if the SDK
  cannot reconstruct from the explicit fields above.

Prefer one additive greenfield migration. Remove superseded MCP-specific
fields and fallback states rather than maintaining compatibility aliases.

### Token exchange, refresh, and broker use

Adapt SDK token responses into the existing `AuthGrantRecord` and secret
records. The broker remains the only runtime path to a usable bearer token and
continues to enforce:

- the MCP server URL as the admitted audience;
- grant status and revocation;
- single-flight refresh across workers;
- refresh-token rotation without exposing either token;
- `needs_reauth` when refresh is no longer possible;
- leases and audit appropriate to the caller.

Use `rmcp` for MCP-specific refresh request construction and response parsing,
including `resource` and any required scope behavior. Do not create an SDK
client at tool-call time that reads or refreshes credentials independently of
the broker.

P143 discovery receives a broker-resolved bearer exactly as it does today.
Provider-proxied model execution also remains unchanged: the provider-native
adapter receives a short-lived broker resolution immediately before I/O.
P145 native execution will use the same broker plus SDK challenge handling.

### Challenges and scope upgrades

Treat an MCP authorization challenge as data, not as a generic 401 string.
The SDK should parse the challenge and determine the current protected
resource, authorization server, and requested scope set. Lightspeed then
decides what product action is allowed:

- an expired token may be refreshed automatically through the broker;
- a revoked or unusable refresh token marks the grant `needs_reauth`;
- a challenge requiring new scopes never silently expands consent;
- management discovery reports a typed `additional_consent_required` result
  with the suggested scopes;
- a future P145 run parks only through an explicit auth/approval design; P143b
  does not introduce interactive model-time consent.

Advertised scopes are hints. They may be shown in the server editor beside
the authored requested scopes, but discovery must not rewrite policy.

## API and UX

Keep the existing MCP server, OAuth start, and callback concepts. P143b is
primarily an internal correctness refactor, not a second OAuth API.

The UI should continue to communicate:

- which MCP server and resource the user is authorizing;
- the requested scopes;
- whether the client is pre-registered, CIMD-derived, or dynamically
  registered when that distinction helps diagnose setup;
- typed reauthorization and additional-consent states;
- that completing OAuth authenticates the connection but does not prove that
  the identity or workspace is allowed to list or call tools.

Initially preserve explicit protected-resource metadata and authorization
server overrides for deployments that require them. After live compatibility
coverage exists, separately decide whether current standard discovery makes
either field unnecessary. Do not combine that product-surface decision with
the protocol migration.

Public failures must remain typed and bounded. Distinguish at least:

- protected-resource discovery failure;
- authorization-server discovery failure;
- unsupported client-registration path;
- invalid issuer or resource binding;
- callback state mismatch or already-consumed flow;
- token exchange/refresh failure;
- additional consent required;
- grant needs reauthorization;
- network-policy or response-limit rejection.

No failure may include an access token, refresh token, authorization code,
client secret, PKCE verifier, raw callback state, or unbounded remote body.

## Implementation Slices

### Slice 1 — Shared secure HTTP adapter

- Extract P143's DNS pinning, redirect validation, deadlines, and byte limits
  behind a narrow reusable runtime component.
- Implement `rmcp`'s OAuth HTTP-client trait on that component.
- Move MCP OAuth metadata I/O off the current redirect-following client.
- Add hostile DNS, redirect, oversized-body, timeout, and redaction fixtures.

### Slice 2 — SDK discovery and registration

- Replace hand-written PRM and authorization-server parsing with `rmcp`.
- Replace hand-written CIMD/DCR protocol handling with `rmcp` while preserving
  manual pre-registration.
- Persist issuer, resource, scope, client-selection, and registration facts
  needed for deterministic callback reconstruction.
- Remove the legacy endpoint fallback and superseded parsers.

### Slice 3 — Durable start and callback bridge

- Adapt SDK authorization requests to `OAuthFlowService` and the existing API.
- Prove restart and cross-gateway callback completion.
- Atomically consume callback state, validate `iss`, and bridge the token
  result into the existing grant and secret stores.
- Keep all sensitive state encrypted or hashed according to its lookup role.

### Slice 4 — Refresh and challenge behavior

- Use the SDK's MCP token request/response behavior from the broker refresh
  path without giving the SDK independent credential authority.
- Map challenge, scope-upgrade, revocation, and reauthorization outcomes to
  typed broker/API states.
- Reuse the result from P143 discovery; leave P145 execution wiring for P145.

### Slice 5 — Cleanup and acceptance

- Delete replaced MCP OAuth wire DTOs, parsers, and tests from `crates/auth`.
- Retain generic OAuth, store, flow, broker, and provider behavior.
- Update generated contracts and Platform consumers for any additive
  diagnostics.
- Run the local, database-backed, and low-privilege live acceptance suites.

## Tests

### Unit and protocol fixtures

- protected-resource metadata at every current well-known location;
- authorization-server issuer and endpoint validation;
- pre-registered, CIMD, and DCR selection order;
- public-client and confidential-client token endpoint authentication;
- DCR `application_type` and returned registration metadata;
- PKCE, resource, `offline_access`, authored scopes, and advertised scopes;
- callback with correct, missing, mismatched, and malformed `iss`;
- `WWW-Authenticate` discovery and `insufficient_scope` challenge parsing;
- refresh with resource binding and refresh-token rotation;
- redirect-to-private-address, DNS rebinding, oversized response, timeout,
  invalid JSON, and secret-redaction failures.

### Store and broker tests

- raw state and PKCE verifier never appear in ordinary record columns;
- tokens and client secrets remain encrypted;
- callback state is consumed once under concurrent completion;
- a flow started before process restart can finish afterward;
- a different gateway instance can finish the flow;
- universe and principal isolation cannot be crossed;
- concurrent refresh performs one upstream refresh and rotates secrets safely;
- audience mismatch is rejected before a credential leaves the broker;
- terminal refresh failure produces `needs_reauth`.

### Live acceptance

Use a low-privilege OAuth MCP fixture; no paid model is required:

1. register the MCP server and begin authorization;
2. complete login and discover tools through P143;
3. repeat with a gateway restart between start and callback;
4. complete the callback on a different gateway instance;
5. expire and refresh the access token, including refresh-token rotation;
6. prove wrong issuer, wrong resource/audience, and replayed callback refusal;
7. provoke insufficient scope and verify the additional-consent diagnostic;
8. revoke refresh authority and verify `needs_reauth`;
9. prove tokens, codes, verifier, client secret, and raw remote errors are
   absent from API responses and logs.

## Completion Criteria

P143b is complete when:

1. no Lightspeed code hand-parses current MCP protected-resource,
   authorization-server, CIMD, DCR, challenge, or token wire responses where
   `rmcp` provides the behavior;
2. every MCP OAuth network request uses the bounded, DNS-pinned HTTP adapter;
3. start, callback, exchange, and refresh survive process boundaries while
   retaining one durable Lightspeed flow/grant lifecycle;
4. issuer, resource, audience, state, scope, and one-time-consumption rules are
   covered by negative tests;
5. P143 discovery works with no auth, static bearer, and SDK-backed OAuth;
6. generic custom OAuth, model OAuth, GitHub App, static bearer, environment
   injection, leases, and broker tests remain green;
7. the low-privilege live acceptance passes, including restart, rotation,
   scope-upgrade, and reauthorization cases;
8. the remaining custom code is visibly product policy, persistence, or
   adaptation—not a parallel MCP OAuth implementation.

## Non-goals

- Replacing the generic Lightspeed auth subsystem with `rmcp`.
- Moving credential or grant selection back into sessions or profiles (P110).
- Native MCP tool execution (P145), elicitation, resources, or prompts.
- MCP tool-call approvals (P144).
- Changing provider-proxied MCP request/response semantics.
- Introducing model-time interactive OAuth consent.
- Redesigning external user identity or the management login system.
- Supporting legacy HTTP+SSE or legacy MCP OAuth endpoint fallbacks.
- Forking the Rust SDK or snapshotting its internal in-memory state as a new
  authority.

## Risks and Mitigations

- **SDK API churn.** Keep the adapter narrow, pin the SDK normally through the
  workspace lockfile, and upstream missing durability-oriented accessors.
- **Loss of a currently supported confidential-client variant.** Cover both
  `client_secret_basic` and `client_secret_post` before deleting the old
  parser; do not assume the SDK's preferred public-client registration is the
  only valid server behavior.
- **Incomplete durable reconstruction.** Make restart and cross-instance
  callback tests a slice-3 gate, not final polish.
- **Two refresh authorities.** The broker always coordinates refresh and
  persists rotation; `rmcp` only constructs and interprets MCP OAuth traffic.
- **SSRF regression through OAuth redirects.** One shared HTTP seam validates
  every resolved address and hop; no SDK or `reqwest` default redirect path is
  reachable in production.
- **Accidental consent expansion.** Advertised or challenged scopes are
  presented as diagnostics until the user explicitly authorizes them.

The implementation should track the official SDK's
[OAuth support documentation](https://github.com/modelcontextprotocol/rust-sdk/blob/main/docs/OAUTH_SUPPORT.md)
and prefer upstream contributions when the hosted/durable integration needs a
small additional seam.

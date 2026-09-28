# P183 — First-class runtime CLI

**Status:** Proposed, 2026-09-28. Implementation has not started.
Builds on [core universes, keys and actors](p179-core-universes-keys-and-actors.md)
and [CLI session model routing](cli-session-model-routing.md).

## Outcome

> An operator can bootstrap, configure and use a single-universe or
> multi-universe runtime entirely from a terminal, then add Platform to that
> same runtime without recreating its universes or resources.

Lightspeed is one runtime with additive clients and capabilities. Platform
adds accounts, membership, roles and the web application. It is optional;
its presence does not replace or disable direct runtime access.

The CLI is a supported interface for personal session and agent use,
automation, and production administration. An administrator on a Linux
terminal must be able to work with an authorized deployment or universe key
without a browser, Platform login, or desktop credential service.

## Current foundation and gaps

- `./dev.sh runtime` already starts the runtime and its infrastructure without
  Platform. Its default is local `single` mode; authenticated runtime access
  also exists independently of Platform.
- The server executable can migrate stores, create universes and mint or
  revoke gateway keys without Platform. These host-side bootstrap operations
  remain available before a remote client has a credential.
- The full development launcher already bootstraps authenticated access for
  Platform, passing its service key to child processes. Runtime-only startup
  does not currently perform that bootstrap, and neither path prepares a
  saved CLI connection. The existing bootstrap command revokes previous
  launcher keys on each invocation; a persistent CLI connection needs its
  own credential lifecycle.
- The CLI already manages sessions, profiles, model credentials, MCP,
  environments, skills and VFS resources through the public runtime API.
- Connection details currently come from command options and environment
  variables. Authentication headers are defaults on the shared HTTP client,
  rather than selected according to each method's scope.
- `initialize` already reports server identity, protocol information, key
  prefix and allowed method groups. It does not report the key's scope or
  bound universe. It is currently universe-scoped, so a deployment key needs
  a universe selection before it can inspect its own access.
- Provider setup has CLI gaps: `auth model add` stores a key but does not
  expose the custom endpoint configuration available through Platform.

This work connects and completes those capabilities. It does not introduce
a second runtime, storage model, or set of deployment editions.

## Decisions

### 1. Direct key access remains a complete interface

Use the existing runtime authority model:

| Connection authority | Universe behavior | Deployment operations |
| --- | --- | --- |
| Explicit local `single` mode | Ordinary calls use the server's configured universe; send no identity headers. | Available under existing single-mode rules, without universe headers. |
| Universe key | The key selects its universe; send no universe header. | Refused. |
| Deployment key | Select a universe for universe operations. | Send no universe header; require the relevant method group. |

A deployment key is not necessarily an unrestricted super key: its method
groups still apply. Universe keys also carry method-group restrictions.
The gateway remains authoritative on every request.

Keys used by Platform are runtime keys with the same rules. The CLI can use
an appropriately provisioned key against a runtime with or without Platform.
It does not extract Platform's service secret or require that particular key.

Direct runtime key access does not acquire Platform membership restrictions.
For example, a key with session access can reach private sessions within its
scope. Platform continues to check user roles and visibility on its own
requests. Actor assertions retain their existing attribution semantics; the
CLI must not invent a logged-in person from a local username.

### 2. Save named connections and resolve them once

A saved connection contains a runtime endpoint, credential source, and an
optional selected universe UUID for a deployment key. Keep local connection
names distinct from server-side agent profiles.

All commands use one shared resolver. Explicit command options override
environment configuration, which overrides the selected saved connection.
Document the mapping of existing environment variables, including `.env`
loading. Changing the endpoint must not silently reuse a saved credential
for another server; require a matching connection or an explicit credential
source. Credential replacement must revalidate any saved universe selection.

Support protected local credential storage and explicit environment, file,
or stdin sources for headless use. Desktop keychains may be supported but
cannot be required. Do not place secrets in ordinary connection metadata,
status output, logs, or command-line arguments. Noninteractive commands must
work without prompts and return actionable errors when configuration is missing.

Use one `connect` command group with explicit subcommands. This is the agreed
interface for implementation; these commands do not exist yet:

```bash
lightspeed connect add production --url https://runtime.example/rpc
lightspeed connect use production
lightspeed connect list
lightspeed connect status
lightspeed connect remove production
```

- `add` verifies and saves a named connection without changing the active
  selection. Prompt for the key or accept an explicit noninteractive
  credential source.
- `use` selects an existing saved connection.
- `list` lists saved connections and marks the selected one.
- `status` reports the effective connection and verified runtime authority.
- `remove` forgets the local connection and its owned local credential
  material. It does not revoke the runtime key or delete externally supplied
  credential files. Removing the selected connection clears the selection;
  do not silently select another server.
- `dev` imports, verifies and selects the current checkout's launcher
  connection in one step, as described below.

Do not add a separate top-level `context` or `status` command. Listing is a
subcommand rather than a `--list` flag, and positional arguments do not
alternate between endpoint URLs and saved connection names.

Allow a global connection override for one invocation without changing the
saved default:

```bash
lightspeed --connection production --universe staging session list
```

Local unauthenticated access is explicit; authentication failure must never
trigger a fallback to single mode. Connection setup does not silently modify
server configuration or create universes.

### 3. Inspect authority before selecting a universe

Prefer extending the existing `initialize` handshake over adding a parallel
status API. Return the authenticated credential's scope, its bound universe
when applicable, allowed groups, and whether the connection is local single
mode. Distinguish key scope from the target of an individual request.

Make this handshake callable before universe selection, including with a
deployment key lacking universe-list permissions. This requires an explicit
contract and gateway scope change; adding response fields alone is not
sufficient. Keep the exception narrow: ordinary universe operations still
require the existing target selection, and authenticated deployments still
require a valid key. The handshake grants no resource-listing authority.

`lightspeed connect status` shows the effective endpoint, connection name,
authentication mode, key prefix and scope, selected or bound universe, and
allowed groups. It distinguishes stored settings from verified server state.
Clear errors distinguish invalid credentials, insufficient groups, missing
universe selection, and unavailable runtime. Discovery informs the client;
it does not replace per-request authorization.

### 4. Make universe selection explicit and scoped to a connection

Proposed command shape:

```bash
lightspeed universe list
lightspeed universe use personal
lightspeed --universe staging session list
```

- Universe listing and creation use existing deployment APIs and require
  their method groups. Name/slug selection resolves against core universes,
  not Platform organizations, display names or browser URLs. Reject ambiguous
  matches and persist the resolved UUID.
- A deployment key without universe-list permission can still select an
  explicit UUID for operations its groups allow. Do not require listing as
  a prerequisite to using such a key.
- `universe use` changes only that connection's saved selection. A per-command
  override does not change it. Missing selection produces a clear error for
  universe operations; deployment operations remain usable.
- A universe key uses its bound universe. Reject conflicting overrides and
  stale selection explicitly; never send a universe header with that key.
  Single mode similarly uses the server's configured universe.
- Show the active universe in the TUI. Changing the connection or universe
  clears active session selection and session-local model overrides. An
  existing command/run keeps its original target; changing a saved default
  cannot retarget an in-flight operation or cancel its work.

Construct identity headers per request from the resolved authority and the
public method metadata. A saved universe selection must not leak into
deployment operations. Keep this logic shared by all CLI commands rather
than reproducing it in chat or individual administration commands.

### 5. Complete the terminal-only setup and administration path

The documented journey is:

1. Start the runtime with PostgreSQL, Temporal and its configured storage;
   apply migrations using the supported launcher or server command.
2. Choose local single mode or bootstrap an authenticated universe and key
   from the server host.
3. Connect the CLI and select a universe where necessary.
4. Configure a model provider, discover models and choose a complete route.
5. Create a profile or session, chat, reopen history and manage runs.
6. Add other runtime resources through their CLI commands as needed.

Close specific gaps needed for this journey: custom provider endpoints,
API kinds and headers, credentialless endpoints, model discovery, universe
administration, and key creation/listing/revocation through the existing
authorized APIs. Preserve host-side bootstrap for the first credential.
Expose structured output and noninteractive inputs for administration and
automation; retain readable terminal output.

Do not make an interactive setup wizard a prerequisite. Command composition
and a concise walkthrough are enough for the initial delivery. Audit existing
commands before adding a second configuration interface. Model selection
continues to pin provider identity and API kind for each session, while
allowing model changes within that route.

### 6. The development launcher prepares the initial CLI connection

Use the same local connection handoff whether Platform is running or absent.
Launcher profiles choose processes; they do not define different CLI access
models. Change runtime-only startup to default to authenticated mode, matching
the full profile. Automatically provision the development connection on first
setup in both profiles. Support all three starting points:

| Launcher invocation | Runtime authentication | Prepared CLI connection |
| --- | --- | --- |
| `./dev.sh runtime` with default settings | Authenticated, without Platform | Runtime endpoint, dedicated development CLI key and initial universe UUID. |
| `./dev.sh` or `./dev.sh full` with default settings | Authenticated, with Platform | The same CLI handoff, independently of Platform's service credential. |
| `LIGHTSPEED_AUTH_MODE=single ./dev.sh runtime` | Explicit local `single` mode | Runtime endpoint and explicit unauthenticated mode; no key or universe header. |

Provide a launcher command-line opt-out, `--no-api-key-bootstrap`, for
developers managing their own API keys:

```bash
./dev.sh runtime --no-api-key-bootstrap
./dev.sh --no-api-key-bootstrap
```

This disables automatic API-key provisioning for the CLI and Platform. It
does not change the authentication mode, revoke existing keys, or disable
migrations, universe setup or other startup preparation. Existing valid
connection material can still be used. When a
requested process such as Platform needs an absent service credential,
report the missing configuration instead of provisioning around the opt-out.
When no CLI handoff is available, direct the user to `connect add` with
their own credential. Use this CLI option rather than an environment-variable
toggle for automatic API-key bootstrap.

The proposed user flow is to start one of those profiles, then run from the
checkout in another terminal:

```bash
lightspeed connect dev
lightspeed connect status
lightspeed chat
```

For an authenticated development connection, deployment-key authority also
allows the universe workflow:

```bash
lightspeed universe list
lightspeed universe use personal
```

The named universe must already exist or be created through the universe
administration command. The initial handoff selects the bootstrapped universe
so the first chat does not require creating or choosing another one. Model
provider configuration is still required before a provider-backed run.

With bootstrap enabled, after migrations the launcher uses its existing
host-side store access to
ensure the initial universe exists and provision a dedicated development CLI
deployment key with all method groups, without actor assertion. Platform
keeps its separate service key and actor assertion. Provisioning the CLI key
must not depend on whether a Platform key was supplied. No unauthenticated
HTTP key-minting endpoint is introduced.

Use the same key provisioning implementation for development and real
authenticated deployments. Accept an operator-supplied bootstrap secret from
protected provisioning input, including the proposed
`LIGHTSPEED_BOOTSTRAP_API_KEY`, or generate one on first development setup.
The environment variable supplies a secret; it is not a bootstrap enable/disable
switch. Register the key's hash and metadata in the ordinary runtime key
store. Subsequent authentication, listing and revocation use that store; the
gateway must not accept the environment secret through a separate bypass.
Provisioning with the same active key is idempotent and must not restore a
revoked key or broaden an existing key's authority.

Write the endpoint, mode, initial universe and a credential reference into
a known checkout-local handoff location under the launcher's ignored state
directory. Store the secret separately in an owner-only file, with a private
parent directory. Do not print the secret or write it into `.env`. Single
mode produces no credential reference. Identify handoffs by checkout so
multiple worktrees cannot overwrite one another's saved connections.

Once the runtime is ready, print the endpoint and `lightspeed connect dev`
instruction. That command reads the handoff as data, verifies the connection
and authority, then imports and selects a named connection. The launcher
itself does not change the user's globally selected connection. Preserve an
existing valid CLI universe selection on reconnect; the bootstrap universe
is the initial default, not a reset on every start.

Keep restarts predictable. Reuse a valid launcher-managed CLI key and its
protected secret across ordinary restarts and transitions between runtime-only
and full profiles. Do not run the existing revoke-and-mint Platform bootstrap
against the CLI key. Handle revocation, missing local credentials and database
resets through explicit replacement, reporting that reconnect is required.
Never rotate externally supplied credentials. Credential replacement must
not leave a saved connection silently using the obsolete secret.

Adding Platform to an already authenticated runtime changes neither the CLI
endpoint nor its key or universe IDs. Moving from single mode to authenticated
mode does require importing the new authenticated connection; this is a
change in runtime authentication, not a dependency on Platform.

For production, retain the same bootstrap boundary without requiring the dev
launcher: an operator with server access explicitly provisions a supplied or
generated deployment key, then supplies the endpoint and key to
`lightspeed connect add` through a protected prompt, stdin or file, then selects
the connection with `lightspeed connect use`. The initial
deployment key does not require an existing universe; create universes and
additional keys through authenticated APIs. Unlike development, ordinary
production server startup does not automatically provision credentials.
An operator can supply an existing universe key instead when deployment
administration is unnecessary. The development file is a local handoff
convenience, not a requirement for production provisioning.

### 7. Platform remains additive

CLI-created universes and resources remain runtime records with stable IDs.
When Platform is added, provide or verify an explicit way to associate those
existing universes with Platform's organizations and memberships. This must
not copy runtime resources, replace their IDs, or silently grant user access
or share private sessions. Platform-owned organization and membership setup
is separate from runtime universe creation.

Direct CLI access continues to work alongside Platform and when Platform is
unavailable. Platform outages do not break runtime key authentication or CLI
administration. The same resource mutations remain visible through both
clients where the caller has access.

## Scope limits

- No browser/SSO CLI login, delegated user tokens, or new core user/role model
  in this slice. Those can be added later without replacing direct key access.
- No embedded database/runtime alternative or removal of Temporal and
  PostgreSQL. Platform independence does not mean infrastructure independence.
- No provider abstraction or conversion of provider-native session history.
- No wholesale CLI rewrite or obligation to mirror every Platform screen.
  Focus on connection handling, universe selection and concrete configuration
  gaps in the supported terminal workflow.

## Delivery and progress

- [x] Record the agreed additive runtime/Platform direction and initial scope.
- [ ] Extend the handshake and public contract for authority inspection before
  universe selection; regenerate API artifacts and consumers.
- [ ] Implement the `connect add/use/list/status/remove` subcommands, global
  `--connection` override, shared connection resolution, headless credential
  handling and method-aware request headers.
- [ ] Unify supplied/generated bootstrap key provisioning through the normal
  key store, preserving revocation and explicit replacement.
- [ ] Default both runtime-only and full startup to authenticated mode with
  automatic development bootstrap; add the `--no-api-key-bootstrap` opt-out.
- [ ] Add the checkout-local launcher handoff and `connect dev`, including
  explicit single mode; keep CLI and Platform credentials independent across
  restarts.
- [ ] Add universe selection and the necessary deployment administration
  commands; isolate TUI/session state by connection and universe.
- [ ] Complete provider configuration and model discovery from the CLI.
- [ ] Verify Platform association with existing core universes and close any
  adoption gap needed to preserve their IDs and resources.
- [ ] Publish terminal-only and CLI-with-Platform walkthroughs and validate
  the acceptance scenarios below.

## Acceptance and validation

Use focused CLI and gateway tests for connection precedence, credential
scope, method groups and headers. Include failures, not only successful
commands. Regenerate and check contracts for handshake changes.

End-to-end acceptance must cover:

1. With Platform absent, configure a local single-mode runtime entirely from
   the terminal, start a session and reopen its history.
2. With Platform absent, bootstrap authenticated access, create two universes,
   configure a provider and chat using only the server and client CLIs.
3. Switch a deployment-key connection between those universes; verify no
   session selection, transcript or model override carries across.
4. Run deployment operations while a universe is selected; assert that no
   universe header is sent. Check restricted deployment keys both with and
   without universe-list permission.
5. Use a universe key without selecting a universe; reject conflicting
   selection and unauthorized deployment operations. Verify single mode
   sends no identity headers and auth failures never downgrade access.
6. Exercise credential replacement, revocation, environment overrides and
   endpoint changes without leaking credentials or reusing invalid targets.
7. Configure a compatible provider with a custom endpoint, then discover and
   select a model; also cover an endpoint without provider authentication.
8. From a headless Linux terminal, administer an authenticated runtime with
   saved connections and with noninteractive credential sources. No browser
   or desktop keychain is available.
9. Add Platform to the existing runtime, associate an existing universe and
   access its resources under explicit Platform permissions. Verify CLI and
   web changes agree, IDs persist, and CLI access survives stopping Platform.
10. Exercise `connect dev` after each of the three launcher starting points.
    Verify no secret copying or browser is required, handoff files have the
    expected permissions, and launcher startup does not change the globally
    selected connection.
11. Restart and switch between authenticated runtime-only and full profiles;
    verify the CLI key and selected universe persist independently of Platform
    key rotation. Cover a supplied Platform key, multiple worktrees, explicit
    CLI key replacement, database reset and single-to-authenticated transition.
12. Verify `--no-api-key-bootstrap` leaves keys untouched in both profiles,
    keeps authenticated mode and ordinary startup preparation, and reports
    missing service credentials without silently provisioning them. Explicit
    single mode remains usable.
13. Provision an environment-supplied production bootstrap key without
    Platform or an existing universe, connect and create a universe. Repeat
    provisioning to verify idempotency; revoke the key and verify that neither
    restart nor repeated provisioning restores its authority, even while the
    environment variable remains set.
14. Exercise connection add, use, list, status and remove. Verify adding does
    not switch the active connection, removing does not revoke server keys,
    and `--connection` with `--universe` overrides only the current invocation.

Provider-backed acceptance and credentialed infrastructure tests are explicit
live checks, separate from ordinary unit and mock HTTP suites. Follow the
repository's serialization rules for Temporal live tests.

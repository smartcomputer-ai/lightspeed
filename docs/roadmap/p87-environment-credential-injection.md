# P87: Environment Credential Injection

**Status**
- Proposed 2026-06-25.
- Implemented v1 2026-06-25.
- Builds on **P86 (Durable Environment Jobs)**, which deliberately deferred
  secrets: *"Secret injection into the VM is deferred. P86 assumes the
  environment already has the credentials it needs."*
- Builds on **P69 (Generic Auth, Secret Store, And Token Broker)** for
  encrypted `auth_secrets`, grant/token resolution, stored provider
  credentials, and GitHub App token minting.
- Changes direction after P86 review: credentials are bound to the
  **session environment**, not selected by individual tool calls.

## Goal

Let a session environment receive credentials it needs for work launched by
Lightspeed without requiring `run_process`, `job_start`, public job APIs, or
future wrappers to mention secrets.

The model:

```text
environment credential binding:
  { session_id, env_id, env_name } -> credential source
```

Every Lightspeed-started process on that session environment receives the
resolved credentials automatically:

- model-visible `run_process`;
- model-visible `job_start`;
- public `session/jobs/create`;
- future environment wrappers that launch guest processes.

Credential binding is an out-of-band environment/session setup operation. It is
not part of the model-visible tool surface.

## Design Decision

Bind credentials to the session environment by **environment variable name**.

For example:

```text
session sess_1, env env_1:
  GITHUB_TOKEN -> auth_grant authgrant_repo_rw
  OPENAI_API_KEY -> auth_provider_credential model:openai
```

Then a job simply says:

```json
{ "argv": ["gh", "repo", "view"] }
```

The runtime injects `GITHUB_TOKEN` automatically when it starts the process. The
job spec does not carry `secret_env`, aliases, secret ids, grant ids, or
provider ids.

This is environment-wide from Lightspeed's perspective, but it should still be
**spawn-time injection**, not permanent VM mutation:

- do not write secrets into shell startup files, images, workspace files, or
  bridge job records;
- resolve credentials immediately before spawning a Lightspeed-launched
  process/job;
- pass resolved values to the bridge only as transient process env;
- redact captured output before persisting or surfacing it.

## Why Not Tool-Call Aliases?

Aliases only make sense when the caller chooses credentials per execution. That
is the wrong boundary here.

The session/environment setup layer already knows what credentials the
environment should have. Once bound, every tool that launches guest code should
see the same credential environment automatically. This keeps model-visible
tools simple and avoids teaching every tool/API a new credential selection
surface.

The model should not see `secret_id`, `grant_id`, `provider_id`, or a
credential alias in tool args. Those are setup/control-plane concerns.

## Storage

Add the table to `crates/store-pg/migrations/006_environments.sql`, beside
`session_environment_bindings`, because the binding is scoped to one
session-visible environment. Do not add columns to `004_auth.sql`: the auth
tables remain the source of truth for credential material:

- `auth_secrets` stores encrypted values;
- `auth_grants` stores grant lifecycle metadata and token secret refs;
- `auth_providers` stores provider config plus `credential_secret_id`.

P87 adds only the scoped permission/mapping from a session environment env var
to one existing credential source.

### Normalized Polymorphic Table

Use typed columns, not `source_json`, because credential source references are
security-sensitive and finite. The existing schema uses JSONB for extensible
documents or host/provider DTOs, but uses columns and FKs for load-bearing
credential references (`auth_providers.credential_secret_id` is the precedent).

Migration table:

```sql
CREATE TABLE IF NOT EXISTS session_environment_credentials (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    env_id text NOT NULL,
    env_name text NOT NULL,
    source_kind text NOT NULL,
    grant_id text,
    auth_provider_id text,
    secret_id text,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, session_id, env_id, env_name),

    FOREIGN KEY (universe_id, session_id, env_id)
        REFERENCES session_environment_bindings (universe_id, session_id, env_id)
        ON DELETE CASCADE,
    FOREIGN KEY (universe_id, grant_id)
        REFERENCES auth_grants (universe_id, grant_id)
        ON DELETE RESTRICT,
    FOREIGN KEY (universe_id, auth_provider_id)
        REFERENCES auth_providers (universe_id, provider_id)
        ON DELETE RESTRICT,
    FOREIGN KEY (universe_id, secret_id)
        REFERENCES auth_secrets (universe_id, secret_id)
        ON DELETE RESTRICT,

    CONSTRAINT session_environment_credentials_env_name_format
        CHECK (env_name ~ '^[A-Za-z_][A-Za-z0-9_]{0,127}$'),
    CONSTRAINT session_environment_credentials_source_kind_known
        CHECK (source_kind IN (
            'auth_grant',
            'auth_provider_credential',
            'direct_secret'
        )),
    CONSTRAINT session_environment_credentials_source_exactly_one
        CHECK (
            (source_kind = 'auth_grant'
                AND grant_id IS NOT NULL
                AND auth_provider_id IS NULL
                AND secret_id IS NULL)
            OR (source_kind = 'auth_provider_credential'
                AND grant_id IS NULL
                AND auth_provider_id IS NOT NULL
                AND secret_id IS NULL)
            OR (source_kind = 'direct_secret'
                AND grant_id IS NULL
                AND auth_provider_id IS NULL
                AND secret_id IS NOT NULL)
        ),
    CONSTRAINT session_environment_credentials_created_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT session_environment_credentials_updated_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT session_environment_credentials_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);
```

`direct_secret` should be operator/API-only. Most product flows should bind
either `auth_grant` or `auth_provider_credential`.

## Population

Bindings are created out of band by gateway/control-plane code, not by the
model and not by job tools.

API surface:

```text
session/environments/credentials/bind
session/environments/credentials/list
session/environments/credentials/unbind
```

Inputs are explicit:

```json
{
  "sessionId": "sess_1",
  "envId": "env_1",
  "envName": "GITHUB_TOKEN",
  "source": {
    "type": "authGrant",
    "grantId": "authgrant_repo_rw"
  }
}
```

Profiles or environment-create flows can call the same binding service during
setup, so a profile can materialize "repo write token" into `GITHUB_TOKEN`
without storing any secret in the profile document.

## Runtime Resolution

At process/job spawn time, the runtime loads bindings for
`{session_id, env_id}` and resolves each source:

- `auth_grant`: call `AuthTokenBroker::bearer_token(...)`, using the grant's
  own audience rules and provider kind. GitHub App grants mint installation
  tokens on demand; OAuth grants refresh inside the expiry margin; static bearer
  grants read the encrypted stored value.
- `auth_provider_credential`: read the active provider row, validate export
  policy, then decrypt its `credential_secret_id` through `SecretStore`.
- `direct_secret`: decrypt through `SecretStore` after explicit policy
  validation.

Resolution happens only in runtime request handling or activity execution, never
inside deterministic workflow code. Resolved values must not be written to
Temporal history, session events, CAS blobs, registry rows, or provider request
hashes.

### Burst Reuse And Caching

"Resolve at spawn time" means the runtime obtains a current value immediately
before constructing the guest process environment. It must not mean "mint a new
token for every process."

For a single durable job that runs many commands, injection happens once at job
start. A job that executes 30 `git` commands receives one process environment
snapshot unless the job itself launches new Lightspeed-managed child jobs.

For bursts of separate `run_process` or `job_start` calls, the resolver should
be cache-aware:

- `auth_grant` uses `AuthTokenBroker::bearer_token(...)`. The broker already
  enforces audience/status, uses expiry margins, and single-flights renewal.
  GitHub App installation tokens are cached in process memory per grant until
  their expiry margin; 30 short-succession GitHub calls should reuse the cached
  installation token.
- OAuth grants should reuse the stored current access token until it enters the
  refresh margin; refresh remains single-flight per grant.
- Static provider credentials and direct secrets may be decrypted per spawn in
  v1, but the resolver can add a small process-local cache if needed. Such a
  cache must be bounded by source id, source version/updated timestamp when
  available, export policy, and a short TTL; it must never be persisted or sent
  to the bridge except as the transient env for one spawn.
- Within one spawn, if multiple env vars reference the same credential source,
  resolve it once and fan out the value in memory.

The database table records only the binding. Token reuse, refresh margins, and
burst coalescing belong in the runtime resolver/broker layer, not in
`session_environment_credentials`.

If an explicit process/job `env` override tries to set an env var that is bound
as a credential, reject the request. The bound credential wins by policy, but
failing is clearer than silently shadowing caller input.

## Host Protocol And Bridge

`run_process` and `job_start` have an internal resolved-secret-env path
between `temporal-server` and the active `JobExecutor`/`ProcessExecutor`. This
path is not exposed in model-visible or public API DTOs.

The host data-plane request can carry:

```rust
secret_env: BTreeMap<String, SecretString>
```

where `SecretString` is a protocol-local wrapper with redacted `Debug`. The
bridge merges `env` plus `secret_env` at `Command` construction time and never
persists `secret_env`.

P87-bound credential risks in the bridge:

- `host-bridge` persists job records under `.lightspeed/jobs/*.json`;
- credential values must not be added to those records;
- credential values must not enter job spec hashes;
- stdout/stderr chunks are persisted as they are read, so credential values
  must be redacted first.

Explicit public/model `env` and `stdin` remain ordinary job inputs and are not
treated as secret-safe by P87. Bound credentials use a separate `secret_env`
path specifically so callers do not put secrets there.

P87 rules:

1. Secret-bound env values are held only in memory until spawn.
2. Persisted job records may remember that a job required credential env names,
   but never values.
3. Provider/job idempotency hashes use credential env names, never resolved
   values.
4. Captured output is redacted against resolved values before it is persisted or
   returned.
5. If the bridge restarts and a persisted running job required credentials, it
   must not respawn the job without re-resolution. Mark it terminal
   `interrupted`, and let Lightspeed re-issue.

## JSONB Or Normalized?

Use normalized columns for P87 v1.

Existing schema pattern:

- JSONB is used for extensible documents and protocol DTOs:
  `auth_providers.config_json`, environment `connection_json`,
  `capabilities_json`, profile `document_json`, VFS `source_json`.
- Credential references and durable routing keys are columns:
  `auth_grants.access_token_secret_id`,
  `auth_grants.refresh_token_secret_id`,
  `auth_providers.credential_secret_id`,
  `environment_jobs.session_id/env_id/job_id`.

The P87 binding is a security-sensitive FK mapping with only three source kinds.
Normalized nullable source columns plus an `exactly_one` check give:

- database-enforced referential integrity;
- clear delete behavior (`ON DELETE RESTRICT` for credential sources);
- simple list/read queries without JSON indexing;
- a natural Rust DTO with explicit validation.

JSONB would be acceptable only if we expected many provider-specific source
shapes. We do not. The source-specific variability belongs in the existing auth
records, not in the environment binding row.

## Implementation Plan

### G1. Registry And Store

- [x] Add DTOs and a store trait in `environment-registry` for
  environment credential bindings.
- [x] Add `session_environment_credentials` to `006_environments.sql` with the
  normalized table above.
- [x] Implement the store in `store-pg` and in-memory tests.

### G2. Public Control-Plane API

- [x] Add `session/environments/credentials/bind|list|unbind`.
- [x] Validate that the session environment exists and is not detached for
  binding.
- [x] Validate the source exists and is usable.
- [x] Apply explicit policy for provider credentials and direct secrets.

### G3. Runtime Injection

- [x] Load credential bindings when constructing runtime environments or immediately
  before process/job spawn.
- [x] Resolve bindings in `temporal-server` runtime code, outside workflow replay.
- [x] Merge resolved credential env into environment process/job starts.
- [x] Reject caller `env` entries that collide with bound credential env names.

### G4. Host Protocol And Bridge Safety

- [x] Add a redacted `SecretString` wrapper to host-protocol.
- [x] Add an internal secret env channel for process and job starts.
- [x] Keep resolved values out of bridge `JobRecord` and bridge spec hashes.
- [x] Redact stdout/stderr chunks before persistence.

### G5. Tests

- [x] Store tests for credential binding behavior.
- [x] API tests for bind/list/unbind JSON-RPC routing.
- [x] Runtime wiring covered by temporal-server and tools tests.
- [x] Bridge tests that process/job starts receive hidden env vars and redacted
  output does not persist resolved values.
- [ ] Runtime tests that `run_process` and `job_start` receive bound env vars
  without mentioning credentials in tool args.
- [x] Collision handling is implemented in resolver and bridge paths.
- [x] Ignored live test for stored provider credential binding injected into a
  host-bridge environment job and redacted from job output.
- [ ] Ignored live test for GitHub App grant binding injected as `GITHUB_TOKEN`.

## Non-Goals

- No model-visible credential selection surface.
- No permanent VM mutation or credential file provisioning.
- No P69 token-lease records in v1.
- No reverse channel from bridge to worker for re-minting.
- No unrestricted export of model-provider API keys; provider credentials need
  explicit policy.
- No sandbox-create credential injection beyond process/job spawn-time env.

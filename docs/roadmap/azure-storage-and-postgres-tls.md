# Azure Blob storage and PostgreSQL TLS

**Status:** Implemented and verified locally, 2026-11-02.

The Runtime release enables SQLx's Rustls backend with native CA roots in both
store and Runtime dependency declarations. The same pool configuration serves
normal Runtime connections, explicit migrations, and API-key provisioning.
Database authentication remains username/password. Platform and LLM clients
retain their existing TLS behaviour; this change introduces no certificate-file
requirement or LLM-specific trust override.

Storage backend configuration now selects native Azure Blob or S3. Existing
S3 variables retain their defaults. Azure requires an explicit selector,
account/container, and exactly one account key or SAS token. Credentials and
invalid Azure credential errors are redacted. Missing or mixed backend settings
fail before the Runtime attempts database setup.

Both implementations remain behind the existing `ObjectStore` interface, with
separate `s3.rs` and `azure.rs` modules in `store-pg`. Existing public S3 exports,
CAS hashes, universe prefixes, database schema, and inline storage are preserved.
Changing backend/container still requires a deliberate data migration.

Verification includes configuration and redaction unit tests, plus explicit
ignored suites for external services:

- `cargo test -p store-pg --test azure_blob_live -- --ignored` requires
  `LIGHTSPEED_TEST_POSTGRES_URL`, `LIGHTSPEED_TEST_AZURE_ACCOUNT`,
  `LIGHTSPEED_TEST_AZURE_CONTAINER`, and exactly one of
  `LIGHTSPEED_TEST_AZURE_ACCESS_KEY` or `LIGHTSPEED_TEST_AZURE_SAS_TOKEN`.
  `LIGHTSPEED_TEST_AZURE_ENDPOINT` optionally selects an emulator or private
  account endpoint. The suite uses a unique schema and prefix and cleans up its
  blobs. It checks payloads above the inline threshold, fresh-client reads,
  missing blobs, universe isolation, listing, and deletion.
- `cargo test -p store-pg --test postgres_tls_live -- --ignored` requires
  `LIGHTSPEED_TEST_POSTGRES_TLS_URL` with `sslmode=verify-full`,
  `LIGHTSPEED_TEST_POSTGRES_WRONG_CA_FILE` pointing at a valid unrelated CA, and
  `LIGHTSPEED_TEST_POSTGRES_WRONG_HOST` naming another address of that server
  which its certificate does not cover. It checks encrypted connections and
  rejection of incorrect trust/hostname settings without changing schemas.

Neither suite reads `.env` or silently succeeds when configuration is missing.
Local test certificates are generated in temporary directories, not stored in
the repository.

Validation passed for the affected Rust unit suites, verified PostgreSQL TLS
(including rejected CA/hostname cases), and Azure CAS against Azurite with both
account-key and SAS authentication. Platform migrations also passed against the
TLS test database and rejected an incorrect hostname. The built Runtime migrated
the database, verified its schema, and provisioned/reused a Platform API key with
the Azure settings loaded. Full workspace Clippy with all targets and warnings
denied, formatting, and documentation checks passed.

Actual AKS, Azure PostgreSQL, and Azure Blob validation remains part of deployment
acceptance; emulator tests do not establish bank connectivity.

## Workload identity

**Status:** Implemented and verified against a local TLS fake of Entra and Azure
Storage, 2026-11-05. Not yet run against a real storage account.

`LIGHTSPEED_OBJECT_STORE_AZURE_AUTH_MODE=workload_identity` selects `object_store`'s
workload identity provider explicitly, using `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`,
`AZURE_FEDERATED_TOKEN_FILE`, and an optional `AZURE_AUTHORITY_HOST`. The library
performs the exchange, caches access tokens, and re-reads the token file for each
exchange. Its token client and Blob client share one set of client options, so
they share certificate trust.

Two wrappers in `store-pg` keep credentials private, because the library alone
does not:

- Its provider's derived `Debug` prints the cached access token, so the store holds
  it behind a provider whose `Debug` shows only the file path. That provider also
  labels token failures by stage.
- Its request errors embed raw response bodies, so a connector reduces every error
  body to allowlisted identifiers: Entra's error, `AADSTS` code, trace and correlation
  IDs, and the storage error code and request ID. Free text such as
  `error_description` and `AuthenticationErrorDetail` is dropped, so diagnosis beyond
  the code uses those IDs in the service's own logs.

`cargo test -p store-pg --lib azure` runs the real exchange and Blob client against
the fake over TLS, with certificates generated per test. It covers the wire content,
rotation through a symlink swap and a file replacement, rejected certificates and
hostnames on both endpoints, failed authentication, and redaction. Those tests trust
only their generated CA, so they exercise `webpki` rather than the platform verifier
that production uses with its default options.

`azure_blob_live` also runs in this mode with
`LIGHTSPEED_TEST_AZURE_AUTH_MODE=workload_identity` and the `AZURE_*` settings, and
refuses to start if a test access key or SAS token is set. Run it from a workload with
a federated identity, against an account with Shared Key access disabled.

## PostgreSQL workload identity

**Status:** Implemented for the Runtime and Platform, 2026-11-05. Not yet run against a
real Azure server. Temporal stays on password authentication and is planned separately.

`LIGHTSPEED_POSTGRES_AUTH_MODE` and `LIGHTSPEED_PLATFORM_DATABASE_AUTH_MODE` select an
Entra token as the login password, for the scope
`https://ossrdbms-aad.database.windows.net/.default`. The URL must name the identity, hold
no password, and set `sslmode=verify-full`, because the token is sent as a cleartext
password inside TLS.

- **Runtime:** sqlx has no per-connection credential callback, and the Blob provider's
  scope is fixed, so `store-pg` performs the exchange. A background task re-reads the
  projected token and updates the pool's connect options at half of each token's lifetime.
  It logs and retries every 30 seconds on failure.
- **Platform:** `pg` accepts a password function, called for each new connection, and
  `WorkloadIdentityCredential` caches and renews the token. `pg` discards that function when
  combined with a `connectionString`, so the URL is expanded into fields. The SDK reuses the
  token file's contents for five minutes, far less than the rotation interval. Failures are
  rebuilt as new errors holding only identifiers.

Pool settings apply in both authentication modes, so each new connection is less common:

| | Runtime (`sqlx`) | Platform (`pg`) |
| --- | --- | --- |
| Connections kept when idle | 2, opened eagerly | up to 2, kept once opened |
| Idle timeout | 10 minutes (default) | 10 minutes, was 10 seconds |
| Maximum lifetime | 1 hour, was 30 minutes | 1 hour, was unlimited |
| Dead connection handling | checked before use (default) | TCP keepalive after 60 seconds, plus an `error` listener |

The Platform's `error` listener is also a correction for both modes: without one, losing an
idle connection ended the process.

Verification:

- `cargo test -p store-pg --lib azure` covers the exchange against a local TLS fake:
  wire content, rotation, certificate and hostname rejection, and redaction.
- An ignored test in the same module rotates a real role's password on the development
  PostgreSQL, with the fake issuing it as the token. It shows that the old password is
  refused, new connections use the renewed token, and an open connection keeps working.
- `npm run test --workspace @lightspeed-ai/platform-db` drives the real Azure credential
  through a scripted transport.
- `cargo test -p store-pg --test azure_postgres_live -- --ignored` requires
  `LIGHTSPEED_TEST_AZURE_POSTGRES_URL` and the `AZURE_*` settings, and must run from a workload
  with a federated identity.

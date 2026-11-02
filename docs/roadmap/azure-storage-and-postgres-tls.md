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

# Environment variables

This is the authoritative reference for environment variables read by
Lightspeed services, command-line tools, development helpers, tests, and
release automation. Variables are grouped by the component that owns them so
deployment configuration does not accidentally mix the Rust runtime with the
TypeScript Platform plane.

Unless stated otherwise, an unset variable uses the listed default. A variable
marked **required** must be non-empty for the relevant component or role.
Secrets should be supplied by the deployment secret manager and must not be
committed to `.env` files.

The Rust server, CLI, evaluator, and Incus provider load a root `.env` file when
present. The unified development supervisor inherits the caller's environment
and then applies the defaults from `dev/env.sh`.

| Namespace | Owner |
| --- | --- |
| `LIGHTSPEED_*` | Core runtime and shared client/deployment settings. Check the owning section because a few, such as `LIGHTSPEED_API_URL`, are client-side rather than server-side. |
| `LIGHTSPEED_PLATFORM_*` | TypeScript Platform management plane and its shared Platform/Channels database. |
| `LIGHTSPEED_CHANNELS_*` | Channels roles and connectors. |
| `LIGHTSPEED_CONFIGURATOR_MCP_*` | Configurator MCP service. |
| `LIGHTSPEED_ENVD_*` | Environment daemon. |
| `OPENAI_*` / `ANTHROPIC_*` | Provider transport and live-test overrides. |
| Unprefixed infrastructure names | Local Docker Compose only unless their component section says otherwise. |

## Core runtime

These variables configure `lightspeed-server`, including its JSON-RPC gateway,
Temporal worker, PostgreSQL stores, CAS, provider clients, and preprocessing.
They do not configure the TypeScript Platform server.

### Gateway, Temporal, and storage

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_POSTGRES_URL` | **Required**; falls back to `LIGHTSPEED_TEST_POSTGRES_URL` | PostgreSQL connection URL used by the runtime, migration commands, and schema diagnostics. Production must use this name. |
| `LIGHTSPEED_PG_UNIVERSE_ID` | **Required in `single` auth mode** | UUID of the sole universe in a single-tenant deployment. Not used to auto-create universes in multi-tenant modes. |
| `LIGHTSPEED_TASK_QUEUE` | `lightspeed-agent` | Temporal task queue shared by the runtime gateway and worker. Deployments sharing a Temporal namespace should use distinct queues. |
| `TEMPORAL_ADDRESS` | `localhost:7233` | Temporal frontend address. Shared with Platform workflow components. |
| `TEMPORAL_NAMESPACE` | `default` | Temporal namespace. Shared with Platform workflow components. |
| `LIGHTSPEED_GATEWAY_BIND` | `127.0.0.1:18080` | JSON-RPC and environment-gateway listener address. |
| `LIGHTSPEED_GATEWAY_MAX_REQUEST_BODY_BYTES` | `67108864` | Maximum gateway request body size in bytes. |
| `LIGHTSPEED_PUBLIC_BASE_URL` | `http://{LIGHTSPEED_GATEWAY_BIND}` | Externally reachable gateway base URL used for OAuth callbacks and as the combined-mode environment route base. Hosted deployments should set it explicitly. |
| `LIGHTSPEED_AUTH_MODE` | `single` | Tenant/auth resolution: `single`, `trusted-header`, or `api-key`. Configurator MCP must use the same mode. |
| `LIGHTSPEED_SECRETS_MASTER_KEY` | Unset | Base64-encoded 32-byte AES key for encrypted grants and secrets. Required before encrypted secret material can be persisted or resolved. Keep stable across restarts. |
| `LIGHTSPEED_BLOB_CACHE_BYTES` | `268435456` | Per-process CAS blob-cache budget. `0` disables the cache. |
| `LIGHTSPEED_ALLOW_UNLEDGERED_SCHEMA` | `false` | Allows runtime startup against externally managed Lightspeed tables without a migration ledger. It does not relax `migrate` or schema diagnostics. |
| `LIGHTSPEED_LOG_FORMAT` | `compact` | Log renderer: `compact`, `pretty`, or `json`. |
| `RUST_LOG` | Built-in service filter | Standard tracing filter override, for example `temporal_server=debug`. |

`LIGHTSPEED_UNIVERSE_AUTO_CREATE` is retired. Setting it is an error; create
universes explicitly through the operator API or `lightspeed-server universe
create`.

### Default model and provider transport

Provider keys in the environment are deployment-wide fallback credentials.
They may be omitted when every request resolves a stored, universe-scoped
provider credential.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_CHAT_PROVIDER` | `openai` | Default provider ID for new runtime and CLI chat configuration. |
| `LIGHTSPEED_CHAT_MODEL` | `gpt-5.5` | Default model for new runtime and CLI chat configuration. |
| `OPENAI_API_KEY` | Conditional | Default OpenAI Responses and audio-transcription credential. |
| `OPENAI_BASE_URL` | `https://api.openai.com/v1` | OpenAI-compatible API base URL. |
| `OPENAI_ORG_ID` | Unset | Optional `OpenAI-Organization` header. |
| `OPENAI_PROJECT_ID` | Unset | Optional `OpenAI-Project` header. |
| `ANTHROPIC_API_KEY` | Conditional | Default Anthropic Messages credential. |
| `ANTHROPIC_BASE_URL` | `https://api.anthropic.com/v1` | Anthropic-compatible API base URL. |
| `ANTHROPIC_VERSION` | `2023-06-01` | Anthropic API version header. |
| `ANTHROPIC_BETA` | Unset | Comma-separated Anthropic beta headers. |

### Object storage

Object storage is optional. If any `LIGHTSPEED_OBJECT_STORE_*` variable is set,
`LIGHTSPEED_OBJECT_STORE_BUCKET` becomes required. Without this group, blobs
remain in PostgreSQL-backed storage.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_OBJECT_STORE_BUCKET` | Conditional | S3 bucket for CAS payloads. |
| `LIGHTSPEED_OBJECT_STORE_ENDPOINT` | Provider default | Optional S3-compatible endpoint such as MinIO. |
| `LIGHTSPEED_OBJECT_STORE_REGION` | `us-east-1` | S3 region. |
| `LIGHTSPEED_OBJECT_STORE_PREFIX` | Unset | Key prefix inside the bucket. |
| `LIGHTSPEED_OBJECT_STORE_FORCE_PATH_STYLE` | `false` | Boolean path-style addressing override; use `true` for the local MinIO stack. |
| `AWS_ACCESS_KEY_ID` | Credential-chain dependent | S3 access key used when explicit object-store credentials are needed. |
| `AWS_SECRET_ACCESS_KEY` | Credential-chain dependent | S3 secret key. |

### Audio preprocessing

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_AUDIO_TRANSCODER` | `none` | Set to `ffmpeg` to enable audio transcoding; `none` or unset disables it. |
| `LIGHTSPEED_FFMPEG_PATH` | `ffmpeg` | Executable used when the FFmpeg transcoder is enabled. |
| `LIGHTSPEED_AUDIO_TRANSCODE_TIMEOUT_MS` | `30000` | Positive transcoding timeout in milliseconds. Invalid/non-positive values fall back to the default. |

### Split environment routing

Combined gateway/worker mode derives a local environment route automatically.
Split deployments must provide both values to the worker.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_ENVIRONMENT_GATEWAY_URL` | **Required for a separate worker** | Stable gateway base URL used by workers for environment data routes. |
| `LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN` | **Required for a separate worker** | Shared deployment bearer token for worker-to-gateway routing. |

## Rust CLI

These variables configure the `lightspeed` CLI, not the server. Command-line
flags override their corresponding environment values.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_API_URL` | **Required unless `--api-url` is supplied** | Lightspeed JSON-RPC endpoint, normally ending in `/rpc`. Also used as the Platform server's fallback gateway URL. |
| `LIGHTSPEED_API_KEY` | Unset | Bearer key sent to an `api-key` mode gateway. |
| `LIGHTSPEED_UNIVERSE` | Unset | Value sent as `x-lightspeed-universe` for trusted-header development/proxy flows. |
| `LIGHTSPEED_CHAT_PROVIDER` | `openai` | Default chat provider ID. |
| `LIGHTSPEED_CHAT_API_KIND` | `openai:responses` | Default provider API kind used by the CLI's new-session draft. |
| `LIGHTSPEED_CHAT_MODEL` | `gpt-5.5` | Default chat model. |
| `LIGHTSPEED_CHAT_REASONING_EFFORT` | `high` | Default effort: `low`, `medium`, `high`, or `none`. Invalid values fall back to `high`. |
| `LIGHTSPEED_CHAT_MAX_TOKENS` | Unset | Optional positive integer maximum output-token setting for new sessions. |

CLI options such as `--api-key-env`, `--private-key-env`, `--token-env`, and
`--client-secret-env` intentionally accept an arbitrary environment-variable
name. Those caller-chosen secret names are not Lightspeed configuration keys.

## Environment services

### Environment daemon

`lightspeed-envd` is passive and requires no Lightspeed identity or credential.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_ENVD_LISTEN` | `127.0.0.1:19091` | WebSocket listener address. |
| `LIGHTSPEED_ENVD_CWD` | Current directory | Default working directory exposed to jobs. |
| `LIGHTSPEED_ENVD_FS_ROOT` | Native filesystem root containing the working directory | Filesystem boundary exposed by the daemon. |
| `LIGHTSPEED_ENVD_STATE_DIR` | `<cwd>/.lightspeed-envd` | Durable daemon state directory; relative paths resolve under the working directory. |

### Incus provider

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_INCUS_PROVIDER_CONFIG` | **Required unless `--config` is supplied** | Path to the provider's JSON configuration. Incus credentials, templates, network policy, and ingress settings live in that file rather than separate environment variables. |

## Platform server

These variables configure the TypeScript management API/web server under
`platform/server`. Its database and authentication are separate concerns from
the Rust runtime database and gateway authentication.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_PLATFORM_DATABASE_URL` | **Required** | Platform/Channels PostgreSQL connection URL. |
| `LIGHTSPEED_PLATFORM_AUTH_SECRET` | **Required** | Better Auth signing/encryption secret. Use a strong, stable deployment secret. |
| `LIGHTSPEED_PLATFORM_BASE_URL` | `http://localhost:3000` | Public Platform origin used by authentication and trusted-origin checks. |
| `PORT` | `3000` | Platform HTTP listen port. |
| `LIGHTSPEED_PLATFORM_ADMIN_EMAIL` | Unset | Bootstrap administrator email. Applied only with the password and only while the users table is empty. |
| `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD` | Unset | Bootstrap administrator password. Applied only with the email and only while the users table is empty. |
| `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID` | Unset | GitHub login client ID. GitHub login is enabled only when both GitHub variables are present. |
| `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET` | Unset | GitHub login client secret. |
| `LIGHTSPEED_API_URL` | Per-universe gateway URL, otherwise unset | Fallback Lightspeed JSON-RPC endpoint for universes without their own `gatewayUrl`. |
| `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL` | Unset | Public Configurator MCP endpoint installed by the Configurator setup. The setup is unavailable when omitted. |
| `LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS` | Empty list | Comma-separated internal connector health base URLs aggregated for Platform administrators. |

The Platform administration CLI additionally accepts
`LIGHTSPEED_PLATFORM_CONFIG_DIR`; it defaults to
`~/.config/lightspeed-platform` and stores its URL and bearer token in
`config.json`.

## Channels

The single Channels image starts one role (`workflows`, `activities`,
`telegram`, or `whatsapp`) or the combined `all` role. Requirements below apply
only to roles that use the setting.

### Role and shared connectivity

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_CHANNELS_ROLE` | `all` | Image/process role. A positional command argument takes effect when this is unset. |
| `LIGHTSPEED_CHANNELS_CONNECTORS` | `telegram` for the image's `all` role | Comma-separated connectors included by `all`: `telegram`, `whatsapp`, or both. The development supervisor treats connectors as opt-in and defaults to none. |
| `TEMPORAL_ADDRESS` | `localhost:7233` | Temporal frontend address. |
| `TEMPORAL_NAMESPACE` | `default` | Temporal namespace. |
| `LIGHTSPEED_CHANNELS_WORKFLOW_TASK_QUEUE` | `lightspeed-channels-workflows-v1` | Workflow-worker task queue override. |
| `LIGHTSPEED_CHANNELS_ACTIVITY_TASK_QUEUE` | `lightspeed-channels-activities-v1` | Shared Lightspeed/control-plane activity-worker task queue override. |
| `LIGHTSPEED_ENDPOINT` | **Required by activity and connector roles** | Lightspeed JSON-RPC endpoint. |
| `LIGHTSPEED_PLATFORM_DATABASE_URL` | **Required by activity and connector roles** | Shared Platform/Channels PostgreSQL connection URL. |
| `LIGHTSPEED_CHANNELS_INGRESS_MAX_PER_MINUTE` | `120` | Positive per-sender ingress rate limit used by Telegram and WhatsApp. |

### Telegram

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_CHANNELS_TELEGRAM_BOT_TOKEN` | **Required** | Telegram bot token. |
| `LIGHTSPEED_CHANNELS_TELEGRAM_ACCOUNT_ID` | **Required** | Stable Platform channel-account identifier for this connector. |

### WhatsApp

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_CHANNELS_WHATSAPP_ACCOUNT_ID` | **Required** | Stable Platform channel-account identifier. |
| `LIGHTSPEED_CHANNELS_WHATSAPP_AUTH_DIR` | **Required** | Directory containing persistent Baileys authentication state. |
| `LIGHTSPEED_CHANNELS_WHATSAPP_MEDIA_LOCATOR_KEY` | **Required** | Base64-encoded 32-byte key used to protect media locators. |
| `LIGHTSPEED_CHANNELS_WHATSAPP_PRINT_QR` | `true` | Set to `false` to suppress terminal QR output. |

### Health and metrics

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_CHANNELS_HEALTH_HOST` | `0.0.0.0` | Connector health-server bind host. |
| `LIGHTSPEED_CHANNELS_HEALTH_PORT` | Connector-specific | Shared health-port override. |
| `LIGHTSPEED_CHANNELS_TELEGRAM_HEALTH_PORT` | `8091` | Telegram-specific health port; takes precedence over the shared port. |
| `LIGHTSPEED_CHANNELS_WHATSAPP_HEALTH_PORT` | `8092` | WhatsApp-specific health port; takes precedence over the shared port. |
| `LIGHTSPEED_CHANNELS_METRICS_HOST` | `0.0.0.0` | Temporal Prometheus exporter host. |
| `LIGHTSPEED_CHANNELS_METRICS_PORT` | Role-specific | Metrics port override. Defaults: workflows/all `9090`, Telegram `9091`, WhatsApp `9092`, activities `9093`. |

## Configurator MCP

Configurator is a separate deployable service. Its auth mode must match the
upstream Lightspeed gateway.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_AUTH_MODE` | `single` | `single`, `trusted-header`, or `api-key`. |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_HOST` | `127.0.0.1` | HTTP bind host. |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT` | `18081` | HTTP bind port. |
| `LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL` | `http://127.0.0.1:18080/rpc` | Upstream Lightspeed JSON-RPC endpoint. |
| `LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_HOSTS` | Loopback hosts | Comma-separated HTTP `Host` allow-list; **required** when binding beyond loopback. |
| `LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_ORIGINS` | Empty list | Comma-separated browser `Origin` allow-list. |
| `LIGHTSPEED_CONFIGURATOR_MCP_MAX_BODY_BYTES` | `67108864` | Maximum MCP JSON request size. |
| `LIGHTSPEED_CONFIGURATOR_MCP_UPSTREAM_TIMEOUT_MS` | `60000` | Per-probe and per-tool upstream timeout. |
| `LIGHTSPEED_CONFIGURATOR_MCP_SHUTDOWN_TIMEOUT_MS` | `10000` | Grace period before open HTTP connections are closed. |

## Foundry candidate

Foundry is mechanically preserved but is not a supported release component.
These variables exist only for its current development workers.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `TEMPORAL_ADDRESS` | `localhost:7233` | Temporal frontend address. |
| `TEMPORAL_NAMESPACE` | `default` | Temporal namespace. |
| `FOUNDRY_WORKFLOW_TASK_QUEUE` | `lightspeed-foundry-workflows-v1` | Foundry workflow queue override. |
| `FOUNDRY_ACTIVITY_TASK_QUEUE` | `lightspeed-foundry-activities-v1` | Foundry activity queue override. |
| `LIGHTSPEED_ENDPOINT` | **Required by the activity worker** | Lightspeed JSON-RPC endpoint. |
| `LIGHTSPEED_PLATFORM_DATABASE_URL` | **Required by the activity worker** | Shared Platform database URL. |

## Local development

`npm run dev` and the helpers under `dev/` provide development-only defaults.
Never reuse their credentials in a deployed environment.

### Supervisor overrides

| Variable | Default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_PLATFORM_DEV_REAL_GATEWAY` | `0` | In the focused `platform` profile, use `LIGHTSPEED_API_URL` instead of the stub gateway. |
| `STUB_GATEWAY_PORT` | `19999` | Focused Platform stub-gateway port. |
| `LIGHTSPEED_CHANNELS_CONNECTORS` | Empty | Connectors started by the `full` development profile. Values: `telegram`, `whatsapp`, or both. |
| `PORT` | `3000` | Platform server port. |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT` | `18081` | Configurator port used by the supervisor. |

The supervisor also honors all runtime, Platform, Channels, and Configurator
variables documented above.

### Docker infrastructure

| Variable | Default | Purpose |
| --- | --- | --- |
| `COMPOSE_PROJECT_NAME` | `lightspeed-dev` | Compose project name. |
| `POSTGRES_IMAGE` | `postgres:17` | PostgreSQL image. |
| `POSTGRES_CONTAINER_NAME` | `lightspeed-postgres` | PostgreSQL container name. |
| `POSTGRES_USER` | `lightspeed` | Local database user. |
| `POSTGRES_PASSWORD` | `lightspeed` | Local database password. |
| `POSTGRES_DB` | `lightspeed` | Local database name. |
| `POSTGRES_PORT` | `15432` | Host PostgreSQL port. |
| `PGADMIN_IMAGE` | `dpage/pgadmin4:8` | pgAdmin image. |
| `PGADMIN_CONTAINER_NAME` | `lightspeed-pgadmin` | pgAdmin container name. |
| `PGADMIN_DEFAULT_EMAIL` | `admin@lightspeed.dev` | Local pgAdmin account. |
| `PGADMIN_DEFAULT_PASSWORD` | `lightspeed` | Local pgAdmin password. |
| `PGADMIN_PORT` | `15080` | Host pgAdmin port. |
| `MINIO_IMAGE` | `quay.io/minio/minio` | MinIO server image. |
| `MINIO_MC_IMAGE` | `quay.io/minio/mc` | MinIO client image. |
| `MINIO_CONTAINER_NAME` | `lightspeed-minio` | MinIO container name. |
| `MINIO_ROOT_USER` | `minioadmin` | Local MinIO access key. |
| `MINIO_ROOT_PASSWORD` | `minioadmin` | Local MinIO secret key. |
| `MINIO_API_PORT` | `29000` | Host MinIO API port. |
| `MINIO_CONSOLE_PORT` | `29001` | Host MinIO console port. |
| `TEMPORAL_IMAGE` | `temporalio/admin-tools:latest` | Local Temporal development-server image. |
| `TEMPORAL_CONTAINER_NAME` | `lightspeed-temporal` | Temporal container name. |
| `TEMPORAL_PORT` | `7233` | Host Temporal frontend port. |
| `TEMPORAL_UI_PORT` | `8233` | Host Temporal UI port. |

`dev/env.sh` derives and exports the runtime variables from these values. It
also supplies a fixed local `LIGHTSPEED_SECRETS_MASTER_KEY`; that key is public
development material and is unsafe for any shared or production deployment.

## Tests and evaluations

These variables opt into external integration tests or select live-provider
fixtures. Ordinary unit tests do not require them.

| Variable | Purpose |
| --- | --- |
| `LIGHTSPEED_TEST_POSTGRES_URL` | PostgreSQL URL for Rust live store/runtime tests and the development fallback. |
| `LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL` | Scratch PostgreSQL URL used by the Platform empty-install and upgrade migration test. |
| `LIGHTSPEED_CHANNELS_TEMPORAL_INTEGRATION` | Set to `1` to enable the Channels Temporal integration suite. |
| `LIGHTSPEED_CHANNELS_DELIVERY_TASK_QUEUE` | Required only by the Channels fake delivery worker used in integration tests. |
| `FOUNDRY_TEMPORAL_INTEGRATION` | Set to `1` to enable the unsupported Foundry Temporal integration test. |
| `LIGHTSPEED_OPENAI_MODEL` | First-choice model override in hosted runtime live tests. |
| `OPENAI_LIVE_MODEL` | Shared fallback model for OpenAI live suites. |
| `OPENAI_RESPONSES_MODEL` | OpenAI Responses live-test model. |
| `OPENAI_RESPONSES_WEB_SEARCH_MODEL` | Web-search-specific Responses model; falls back to the Responses live model. |
| `OPENAI_RESPONSES_COMPACTION_MODEL` | Responses compaction live-test model. |
| `OPENAI_RESPONSES_PROMPTS_MODEL` | Responses prompts live-test model. |
| `OPENAI_COMPLETIONS_API_KEY` | Credential override for OpenAI-compatible Completions tests; falls back to `OPENAI_API_KEY`. |
| `OPENAI_COMPLETIONS_BASE_URL` | Base URL override for Completions tests; falls back to `OPENAI_BASE_URL`. |
| `OPENAI_COMPLETIONS_MODEL` | Completions live-test model. |
| `OPENAI_AUDIO_TRANSCRIPTION_MODEL` | Audio transcription live-test model; defaults to `gpt-4o-transcribe`. |
| `OPENAI_AUDIO_TRANSCRIPTION_FIXTURE` | Repository-relative local audio fixture; overrides the remote fixture URL. |
| `OPENAI_AUDIO_TRANSCRIPTION_FIXTURE_URL` | Remote audio fixture URL. |
| `OPENAI_AUDIO_TRANSCRIPTION_EXPECT` | Optional case-insensitive text expected in the transcription. |
| `ANTHROPIC_LIVE_MODEL` | Shared fallback model for Anthropic live suites. |
| `ANTHROPIC_MESSAGES_MODEL` | Anthropic Messages live-test model. |

Provider live tests also use the production provider transport variables from
the core-runtime section. Most Rust live suites read either the process
environment or the repository root `.env`.

## Build and release automation

These are build inputs, not runtime configuration. Normal deployments should
consume the resulting release manifest and image digests instead of setting
them on services.

| Variable | Purpose |
| --- | --- |
| `LIGHTSPEED_RELEASE_VERSION` | Release version override used by local/CI builds. |
| `LIGHTSPEED_GIT_SHA` | Full source revision embedded in binaries and image labels. |
| `LIGHTSPEED_RELEASE_BUILD_IMAGE` | Digest-pinned build environment recorded in the manifest. |
| `SOURCE_DATE_EPOCH` | Reproducible SBOM/archive timestamp. |
| `LIGHTSPEED_BINARY_URL_SERVER` | Published server archive URL recorded in the manifest. |
| `LIGHTSPEED_BINARY_URL_PROVIDER_INCUS` | Published Incus-provider archive URL. |
| `LIGHTSPEED_BINARY_URL_ENVD` | Published environment-daemon archive URL. |
| `LIGHTSPEED_BINARY_URL_CLI` | Published CLI archive URL. |
| `LIGHTSPEED_RUNTIME_IMAGE` | Digest-pinned runtime image recorded in the manifest. |
| `LIGHTSPEED_PLATFORM_IMAGE` | Digest-pinned Platform image recorded in the manifest. |
| `LIGHTSPEED_CHANNELS_IMAGE` | Digest-pinned Channels image recorded in the manifest. |
| `LIGHTSPEED_CONFIGURATOR_MCP_IMAGE` | Digest-pinned Configurator image recorded in the manifest. |

`release/metadata.env` additionally owns these release-source values. They are
consumed by build scripts and should not be used as deployment overrides.

| Variable | Purpose |
| --- | --- |
| `LIGHTSPEED_RELEASE_TARGET` | Rust release target triple. |
| `LIGHTSPEED_PRODUCT_VERSION` | Product version used when no explicit release version is supplied. |
| `LIGHTSPEED_RELEASE_RUST_VERSION` | Pinned release Rust toolchain. |
| `LIGHTSPEED_RELEASE_BUILD_BASE_IMAGE` | Digest-pinned base used to construct the build environment. |
| `LIGHTSPEED_API_PROTOCOL_VERSION` | API protocol identifier recorded in release metadata. |
| `LIGHTSPEED_SCHEMA_REVISION` | Required Rust runtime database revision. |
| `LIGHTSPEED_PLATFORM_SCHEMA_REVISION` | Required Platform database revision. |
| `LIGHTSPEED_PLATFORM_UPGRADE_FROM` | Oldest Platform migration baseline exercised by release checks. |

The release-info build script generates `LIGHTSPEED_BUILD_VERSION`,
`LIGHTSPEED_BUILD_GIT_SHA`, `LIGHTSPEED_BUILD_RUST_VERSION`, and
`LIGHTSPEED_BUILD_TARGET` as compile-time inputs. Do not set them on running
services.

GitHub publication uses the `NPM_TOKEN` environment secret for npm and the
built-in `GITHUB_TOKEN` for repository/GHCR operations. Snapshot notification
uses the `LIGHTSPEED_DEPLOYMENT_DISPATCH_TOKEN` repository secret and the
`LIGHTSPEED_DEPLOYMENT_REPOSITORY` repository variable; these are GitHub
configuration, not container environment variables.

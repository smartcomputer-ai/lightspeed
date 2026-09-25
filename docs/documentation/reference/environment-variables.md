# Environment variables

This is the authoritative reference for environment variables read by
Lightspeed services, command-line tools, development helpers, tests, and
release automation. Variables are grouped by the component that owns them so
deployment configuration does not accidentally mix the Rust runtime with the
TypeScript Platform plane.

For configuration choices and examples, read
[Configure a deployment](../deployment/configuration.md).

Unless stated otherwise, an unset variable uses the listed default. A variable
marked **required** must be non-empty for the relevant component or role.
Secrets should be supplied by the deployment secret manager and must not be
committed to `.env` files.

The Rust server, CLI, evaluator, and Incus provider load a root `.env` file when
present. The unified development supervisor inherits the caller's environment
and then applies the defaults from `scripts/dev/env.sh`.

| Namespace | Owner |
| --- | --- |
| `LIGHTSPEED_*` | Core runtime and shared client/deployment settings. Check the owning section because a few, such as `LIGHTSPEED_API_URL`, are client-side rather than server-side. |
| `LIGHTSPEED_PLATFORM_*` | TypeScript Platform management plane and its database. |
| `LIGHTSPEED_CONNECTOR_*` | The connector host (Telegram and WhatsApp bridges over the core API). |
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
| `LIGHTSPEED_ROLES` | `gateway,environment-gateway,sessions,bots,channels` | Roles this process runs, a comma-separated subset of `gateway` (JSON-RPC, OAuth callbacks, webhook hooks), `environment-gateway` (worker environment routes, the public daemon registration routes, the lifecycle reconciler, and the power reaper), `sessions` (session workflows plus the promise-repair reaper, the session-retention reaper, both every five minutes, and the hourly CAS blob sweeper), `bots`, `channels` (also `--roles`). Each worker role polls its own task queue with that subsystem's workflows, activities, and background loops. Run exactly one `environment-gateway` process per deployment. |
| `LIGHTSPEED_ENVIRONMENT_PUBLIC_URL` | Unset | Base URL outbound daemons are told to dial for data connections when it differs from `LIGHTSPEED_PUBLIC_BASE_URL`, for example a dedicated hostname in front of the environment gateway. |
| `LIGHTSPEED_WORKER_TASK_TYPES` | `all` | What the worker roles poll: `all`, `workflows`, or `activities` (also `--task-types`). |
| `LIGHTSPEED_TASK_QUEUE` | `lightspeed-sessions` | Sessions task queue shared by the gateway and the `sessions` role. Deployments sharing a Temporal namespace must use distinct queues. |
| `LIGHTSPEED_TASK_QUEUE_BOTS` | `lightspeed-bots` | Task queue of the `bots` role (bot controllers, trigger fires, bot activities). |
| `LIGHTSPEED_TASK_QUEUE_CHANNELS` | `lightspeed-channels` | Task queue of the `channels` role (conversation workflows and core channel activities). Connector activities run on the per-account `lightspeed-connector-*` queues served by the connector host. |
| `TEMPORAL_ADDRESS` | `localhost:7233` | Temporal frontend address. Shared with the connector host. |
| `TEMPORAL_NAMESPACE` | `default` | Temporal namespace. Shared with the connector host. |
| `LIGHTSPEED_GATEWAY_BIND` | `127.0.0.1:18080` | JSON-RPC and environment-gateway listener address. |
| `LIGHTSPEED_GATEWAY_MAX_REQUEST_BODY_BYTES` | `67108864` | Maximum gateway request body size in bytes. |
| `LIGHTSPEED_PUBLIC_BASE_URL` | `http://{LIGHTSPEED_GATEWAY_BIND}` | Externally reachable gateway base URL used for OAuth callbacks, bot webhook ingest URLs (`/hooks/bots/…`), and as the environment route base when the process runs the `gateway` role. Hosted deployments should set it explicitly. |
| `LIGHTSPEED_MCP_PRIVATE_NETWORKS` | Empty (`localhost,127.0.0.1,::1` in `dev.sh` profiles) | Comma-separated exact hosts and CIDRs that outbound requests may reach on private networks: native MCP discovery and execution (where the MCP server record must also opt in with `allowPrivateNetwork`) and bot URL polls. All other targets retain public-only HTTPS/SSRF policy. |
| `LIGHTSPEED_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER_URL` | Unset | Retired; a nonempty value fails startup. Use a scoped service key. |
| `LIGHTSPEED_MCP_OAUTH_ALLOW_PRIVATE_NETWORKS` | `false` (`true` in `dev.sh` profiles) | Allows MCP OAuth metadata, registration, exchange, and refresh to use HTTP or private endpoints. This does not authorize discovery or tool execution; those use `LIGHTSPEED_MCP_PRIVATE_NETWORKS` plus the record opt-in. |
| `LIGHTSPEED_AUTH_MODE` | **Required** | `single` (one universe, every request acts as an unauthenticated deployment administrator; local development only) or `authenticated` (canonical scoped bearer keys). There is no default: the process refuses to start without it. Configurator uses the same mode. |
| `LIGHTSPEED_SECRETS_MASTER_KEY` | Unset | Base64-encoded 32-byte AES key for encrypted grants and secrets. Required before encrypted secret material can be persisted or resolved. Keep stable across restarts. |
| `LIGHTSPEED_BLOB_CACHE_BYTES` | `268435456` | Per-process CAS blob-cache budget. `0` disables the cache. |
| `LIGHTSPEED_CAS_SWEEP_GRACE_MS` | `604800000` (seven days) | Minimum time since a blob's last put or API admission before an unheld blob is eligible for collection; reads do not refresh it. `0` disables background collection. One elected `sessions` process examines up to 100,000 old catalog rows per universe per hourly pass in pages of at most 1,024 rows, resuming its cursor next time. Passes have a soft 10-minute budget between pages; database statements have a five-second timeout and a one-second lock timeout. `lightspeed-server cas-sweep [--dry-run]` runs one bounded pass; deletion yields to an active background leader. Profiles and uncommitted workflow handoffs do not retain blobs; a handoff stalled beyond grace may require resubmission. |
| `LIGHTSPEED_LLM_DEBUG_DUMPS` | `false` | Store every generation's raw provider request (credentials redacted) and raw response as unreferenced CAS blobs and log their refs at debug level. Nothing references the dumps, so they are collected after one grace period. Each request carries the whole context; leave this off outside debugging. |
| `LIGHTSPEED_ALLOW_UNLEDGERED_SCHEMA` | `false` | Allows runtime startup against externally managed Lightspeed tables without a migration ledger. It does not relax `migrate` or schema diagnostics. |
| `LIGHTSPEED_LOG_FORMAT` | `compact` | Log renderer: `compact`, `pretty`, or `json`. |
| `RUST_LOG` | Built-in service filter | Standard tracing filter override, for example `temporal_server=debug`. |

`LIGHTSPEED_UNIVERSE_AUTO_CREATE` is retired. Setting it is an error; create
universes explicitly through the operator API or `lightspeed-server universe
create`.

In authenticated mode, bearer keys resolve canonical active principals. A
universe key selects its own universe; a deployment key selects one through
`x-lightspeed-universe`. A user assertion in `x-lightspeed-principal` requires
scoped `assert_user` and never adds the service's rights to the user's rights.
Single mode rejects identity headers. See [authentication and access](../deployment/authentication-and-tenancy.md).

### Default model and provider transport

Provider keys in the environment are deployment-wide fallback credentials.
They may be omitted when every request resolves a stored, universe-scoped
provider credential.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_CHAT_PROVIDER` | `openai` | Default provider ID for new runtime and CLI chat configuration. |
| `LIGHTSPEED_CHAT_MODEL` | `gpt-5.5` | Default model for new runtime and CLI chat configuration. |
| `OPENAI_API_KEY` | Conditional | Default OpenAI Responses, Chat Completions, and audio-transcription credential. |
| `OPENAI_BASE_URL` | `https://api.openai.com/v1` | Deployment fallback URL for the built-in `openai` provider, shared by Responses, Chat Completions, and audio transcription. Custom universe providers use their stored endpoint instead. |
| `OPENAI_ORG_ID` | Unset | Optional `OpenAI-Organization` header. |
| `OPENAI_PROJECT_ID` | Unset | Optional `OpenAI-Project` header. |
| `ANTHROPIC_API_KEY` | Conditional | Default Anthropic Messages credential. |
| `ANTHROPIC_BASE_URL` | `https://api.anthropic.com/v1` | Anthropic-compatible API base URL. |
| `ANTHROPIC_VERSION` | `2023-06-01` | Anthropic API version header. |
| `ANTHROPIC_BETA` | Unset | Comma-separated Anthropic beta headers. Anthropic provider-mode MCP requires `mcp-client-2025-11-20`; native MCP does not. |

### Object storage

If any `LIGHTSPEED_OBJECT_STORE_*` variable is set,
`LIGHTSPEED_OBJECT_STORE_BUCKET` becomes required. Blobs up to 64 KiB remain
inline in PostgreSQL; larger blobs require object storage. Without this group,
writes above that inline limit fail.

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

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_ENVIRONMENT_GATEWAY_URL` | **Required unless the process runs `environment-gateway`** | Stable environment-gateway base URL used by workers and `gateway`-only processes for environment data routes. |
| `LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN` | **Required unless the process runs `environment-gateway`** | Shared deployment bearer token for worker-to-gateway routing. |

A process running the `environment-gateway` role derives a local environment
route automatically. Every other process, including one that runs only the
`gateway` role, must provide both values.

The `environment-gateway` role serves the worker route above plus the public
registration routes `/environment-gateway/connect` and
`/environment-gateway/data` that outbound `lightspeed-envd` daemons dial, and
it runs the lifecycle reconciler and power reaper. A registered daemon's
control connection lives in the process that accepted it, and the worker
route for that environment must reach the same process: **run exactly one
`environment-gateway` process per deployment** until multi-replica owner
routing is implemented. `gateway`-only processes and workers may scale
freely; they hold no daemon connections and do not answer the registration
routes at all. Expose the two public routes through TLS; the daemon refuses
plain `ws://` toward anything but loopback.

## Rust CLI

These variables configure the `lightspeed` CLI, not the server. Command-line
flags override their corresponding environment values.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_API_URL` | **Required unless `--api-url` is supplied** | Lightspeed JSON-RPC endpoint, normally ending in `/rpc`. Also used as the Platform server's fallback gateway URL. |
| `LIGHTSPEED_API_KEY` | Unset | Bearer key sent to an authenticated gateway. |
| `LIGHTSPEED_UNIVERSE` | Unset | Value sent as `x-lightspeed-universe` for universe selection with a deployment key. |
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

`lightspeed-envd` runs in one of two transports, or both. Passive: it listens
and Lightspeed (or a provider) dials it; it needs no identity or credential.
Outbound: given a gateway URL, it dials Lightspeed, proves an Ed25519 identity
it keeps in its state directory, and registers as an environment through a
registration key the first time. Every `LIGHTSPEED_ENVD_*` variable is removed
from the environment of each process and job the daemon starts.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_ENVD_LISTEN` | `127.0.0.1:19091` unless a gateway URL is set | WebSocket listener address for the passive transport. Set it explicitly to listen while also registering outbound. |
| `LIGHTSPEED_ENVD_GATEWAY_URL` | Unset | Public connect route to dial, `wss://<host>/environment-gateway/connect`. Plain `ws://` is accepted only toward loopback. |
| `LIGHTSPEED_ENVD_DISCOVERY_URL` | `https://<gateway-host>/.well-known/lightspeed-envd` | Override the deployment discovery document used by `lightspeed-envd upgrade` and automatic protocol-mismatch upgrades. Must use HTTPS, except that HTTP is accepted toward loopback for development. |
| `LIGHTSPEED_ENVD_AUTO_UPGRADE` | `false` | Opt a registered outbound daemon into installing and re-executing the build in the deployment discovery document when the gateway challenges it with a different protocol number. Accepts boolean values including `1`/`0`; automatic upgrade is attempted once per process lineage. |
| `LIGHTSPEED_ENVD_REGISTRATION_KEY` | Unset | Registration key admitting a first-seen daemon identity. Read once and dropped from the process environment; on Linux the initial environment stays readable through `/proc`, so use the file form for untrusted workloads. |
| `LIGHTSPEED_ENVD_REGISTRATION_KEY_FILE` | Unset | File holding the registration key; mutually exclusive with the direct form. Delete the file once the receipt appears. |
| `LIGHTSPEED_ENVD_REGISTRATION_NAME` | Unset | Display-name hint recorded on the environment at first registration. |
| `LIGHTSPEED_ENVD_REGISTRATION_METADATA` | Unset | JSON object of string correlation metadata (at most 32 entries; keys up to 64 bytes, values up to 256; the `lightspeed.` prefix is reserved). Descriptive only. |
| `LIGHTSPEED_ENVD_REGISTRATION_RECEIPT` | Unset | Path the daemon writes `{environmentId, incarnationId, daemonId, connectionId, identityMode}` to once admitted. |
| `LIGHTSPEED_ENVD_CA_FILE` | Unset | PEM bundle of additional TLS trust anchors for a gateway behind a private CA. |
| `LIGHTSPEED_ENVD_CWD` | Current directory | Default working directory exposed to jobs. |
| `LIGHTSPEED_ENVD_FS_ROOT` | Native filesystem root containing the working directory | Filesystem boundary exposed by the daemon. |
| `LIGHTSPEED_ENVD_STATE_DIR` | `<cwd>/.lightspeed-envd` | Durable daemon state directory; relative paths resolve under the working directory. Holds the daemon key (`daemon-key`, mode `0600`) and persisted job records/output. Keep it on storage that survives the intended restarts. Deleting identity state causes a new registration and does not clean up the old environment; deleting the directory also removes its job history. |

Identity mode (persistent or ephemeral) is not daemon configuration: it is
the registration key's policy, minted with
`lightspeed env registration-keys create` or on the Platform Environments
page. A closed environment's daemon identity is spent; the daemon exits with
a non-zero status on any terminal rejection and never generates a new
identity on its own.

Run `lightspeed-envd upgrade` to install the build currently named by the
deployment. It uses `LIGHTSPEED_ENVD_DISCOVERY_URL` when set, otherwise derives
the well-known HTTPS URL from `LIGHTSPEED_ENVD_GATEWAY_URL`. The daemon streams
the target-specific archive with a size limit, verifies its SHA-256 checksum,
runs the candidate's `--print-build`, and checks its version, commit, target,
and protocol before atomically replacing the current executable. The command
prints manual installation commands instead when the executable directory is
not writable. `LIGHTSPEED_ENVD_CA_FILE` supplies additional trust roots to both
gateway WebSockets and upgrade downloads.

Automatic upgrade applies only to registered outbound daemons and only to a
protocol mismatch—not ordinary commit drift. After replacement it re-executes
the original executable path with the same arguments, preserving the working
directory, process ID, state directory, and daemon identity. A lineage marker
prevents a stale discovery document from causing an upgrade loop.

`./dev.sh full` and `./dev.sh runtime` also start one daemon on the developer
machine (opt out with `./dev.sh --no-envd` or `LIGHTSPEED_DEV_ENVD=off`):
listening on
`LIGHTSPEED_ENVD_LISTEN` (default `127.0.0.1:19091`), working directory
`LIGHTSPEED_DEV_ENVD_CWD` (default `.lightspeed-dev/envd/workspace`, git-
ignored). It is not registered anywhere automatically; the Platform
Environments page offers **Attach local daemon** (an external environment,
no provider) with the endpoint prefilled through
`LIGHTSPEED_PLATFORM_DEV_ENVD_ENDPOINT`, which the dev supervisor sets for the
Platform process. Provider-backed provisioning (Incus) is not available on
machines without Incus.

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
| `LIGHTSPEED_PLATFORM_API_KEY` | Unset | Deployment-scoped Platform service key with `assert_user` and `manage_identity`. Interactive calls assert the mapped user. Full development provisions one when omitted. |
| `LIGHTSPEED_PLATFORM_DATABASE_URL` | **Required** | Platform PostgreSQL connection URL. |
| `LIGHTSPEED_PLATFORM_AUTH_SECRET` | **Required** | Better Auth signing/encryption secret. Use a strong, stable deployment secret. |
| `LIGHTSPEED_PLATFORM_BASE_URL` | `http://localhost:3000` | Public Platform origin used by authentication and trusted-origin checks. |
| `LIGHTSPEED_PLATFORM_TRUSTED_ORIGINS` | Empty list | Comma-separated additional browser origins accepted by Better Auth. The development supervisor supplies both `http://127.0.0.1:5173` and `http://localhost:5173`. |
| `PORT` | `3000` | Platform HTTP listen port. |
| `LIGHTSPEED_PLATFORM_ADMIN_PRINCIPAL_ID` | Unset | Existing active core user with DeploymentAdmin to bind to the first Platform login. Required with bootstrap email/password until the Platform binds its first admin through organizations; the development launcher no longer provisions it. |
| `LIGHTSPEED_PLATFORM_ADMIN_EMAIL` | Unset | Bootstrap administrator email. Applied only with the password and only while the users table is empty. |
| `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD` | Unset | Bootstrap administrator password. Applied only with the email and only while the users table is empty. |
| `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_ID` | Unset | GitHub login client ID. GitHub login is enabled only when both GitHub variables are present. |
| `LIGHTSPEED_PLATFORM_GITHUB_CLIENT_SECRET` | Unset | GitHub login client secret. |
| `LIGHTSPEED_API_URL` | Unset | Runtime endpoint bound to the Platform service key. Per-universe overrides must match it exactly. |
| `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_URL` | Unset | Public Configurator MCP endpoint installed by the Configurator setup. The setup is unavailable when omitted. |
| `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_ALLOW_PRIVATE_NETWORK` | `false` | Whether the setup-created Configurator MCP record may reach a private network. Enable only for an intentional local/internal endpoint and configure `LIGHTSPEED_MCP_PRIVATE_NETWORKS` on the Runtime accordingly. |
| `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER` | `false` | Retired; `true` fails startup. |
| `LIGHTSPEED_PLATFORM_CHANNELS_HEALTH_URLS` | Empty list | Comma-separated internal connector-host health base URLs (`/healthz` reports every served account) aggregated for Platform administrators. |
| `LIGHTSPEED_PLATFORM_DEV_ENVD_ENDPOINT` | unset | Development only: `lightspeed-envd` endpoint offered as the default when registering an external environment. Set by `./dev.sh`; never in deployed configuration. |
| `LIGHTSPEED_PLATFORM_DEV_SEED` | `false` (`true` in the authenticated full dev profile) | Development only: ensure the Test universe and Admin, Operator, Contributor, Viewer logins at startup. New logins share the bootstrap Admin password; existing passwords are preserved. |

The Platform administration CLI additionally accepts
`LIGHTSPEED_PLATFORM_CONFIG_DIR`; it defaults to
`~/.config/lightspeed-platform` and stores its URL and bearer token in
`config.json`.

## Connector host

`platform/connectors` is the one Node worker left after Bots and Channels
core moved into the Rust runtime: a single process that serves many channel
accounts across many universes. It reads no database; its only dependencies
are the core JSON-RPC endpoint and Temporal. Accounts are discovered through
`deployment/channels/accounts/list`, provider tokens are leased through
`auth/grants/lease` (never configured in the environment), and every
universe-scoped call carries `x-lightspeed-universe` and a bearer service key.
Core must use authenticated mode. The service needs deployment
`discover_channel_accounts` and scoped `lease_credentials` / `admit_channel_inbound`.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_API_URL` | **Required** | Core JSON-RPC endpoint. |
| `LIGHTSPEED_CONNECTOR_API_KEY` | **Required** | Canonical service bearer key for the core gateway. |
| `LIGHTSPEED_CONNECTOR_PROVIDERS` | `telegram,whatsapp` | Comma-separated providers this host serves. |
| `LIGHTSPEED_CONNECTOR_ACCOUNTS` | All discovered accounts | Comma-separated `<universeId>/<accountId>` entries narrowing the served accounts. |
| `LIGHTSPEED_CONNECTOR_DISCOVERY_INTERVAL_MS` | `30000` | Positive interval between discovery passes; new accounts start, disabled or removed ones stop, and a changed revision or a failed runner restarts without a process restart. |
| `TEMPORAL_ADDRESS` | `localhost:7233` | Temporal frontend address. |
| `TEMPORAL_NAMESPACE` | `default` | Temporal namespace. Each account gets one activity worker on its derived `lightspeed-connector-<provider>-<hash>` task queue. |
| `LIGHTSPEED_CONNECTOR_INGRESS_MAX_PER_MINUTE` | `120` | Positive per-chat, per-sender ingress rate limit applied before `channels/inbound/admit`. |
| `LIGHTSPEED_CONNECTOR_WHATSAPP_AUTH_DIR` | **Required when WhatsApp is served** | Root directory of the Baileys session state; each account uses `<dir>/<universeId>/<accountId>`. |
| `LIGHTSPEED_CONNECTOR_WHATSAPP_MEDIA_LOCATOR_KEY` | **Required when WhatsApp is served** | Base64-encoded 32-byte key sealing WhatsApp media locators in inbound envelopes. Keep stable across restarts. |
| `LIGHTSPEED_CONNECTOR_HEALTH_HOST` | `0.0.0.0` | Bind host of the host's `/healthz`, `/readyz`, and `/metrics` listener. |
| `LIGHTSPEED_CONNECTOR_HEALTH_PORT` | `8090` | Port of that listener; `/readyz` is 200 after at least one successful discovery and while every currently served account is ready. Zero accounts qualifies; there is no discovery-freshness deadline. |
| `LIGHTSPEED_CONNECTOR_METRICS_HOST` | `0.0.0.0` | Temporal Prometheus exporter host. |
| `LIGHTSPEED_CONNECTOR_METRICS_PORT` | `9090` | Temporal Prometheus exporter port, shared by every per-account worker in the process. |

Telegram accounts need a retrievable auth grant (`credentialGrantId`) holding
the bot token; WhatsApp accounts pair through the QR code printed on the host's
terminal unless the account's `settings.printQr` is false.

## Configurator MCP

Configurator is a separate deployable service. Its auth mode must match the
upstream Lightspeed gateway.

| Variable | Requirement/default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_AUTH_MODE` | `single` | `single` (explicit local development identity) or `authenticated` (canonical scoped bearer keys). Configurator uses the same mode. |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_HOST` | `127.0.0.1` | HTTP bind host. |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT` | `18081` | HTTP bind port. |
| `LIGHTSPEED_CONFIGURATOR_MCP_RPC_URL` | `http://127.0.0.1:18080/rpc` | Upstream Lightspeed JSON-RPC endpoint. |
| `LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_HOSTS` | Loopback hosts | Comma-separated HTTP `Host` allow-list; **required** when binding beyond loopback. |
| `LIGHTSPEED_CONFIGURATOR_MCP_ALLOWED_ORIGINS` | Empty list | Comma-separated browser `Origin` allow-list. |
| `LIGHTSPEED_CONFIGURATOR_MCP_MAX_BODY_BYTES` | `67108864` | Maximum MCP JSON request size. |
| `LIGHTSPEED_CONFIGURATOR_MCP_UPSTREAM_TIMEOUT_MS` | `60000` | Per-probe and per-tool upstream timeout. |
| `LIGHTSPEED_CONFIGURATOR_MCP_SHUTDOWN_TIMEOUT_MS` | `10000` | Grace period before open HTTP connections are closed. |

## Local development

`./dev.sh` and the helpers under `scripts/dev/` provide development-only defaults.
Never reuse their credentials in a deployed environment.

### Supervisor overrides

| Variable | Default | Purpose |
| --- | --- | --- |
| `LIGHTSPEED_AUTH_MODE` | `authenticated` for `full`; `single` otherwise | Full development explicitly bootstraps a local service credential when no Platform key is supplied. |
| `LIGHTSPEED_CHANNELS_CONNECTORS` | Empty | Providers the `full` development profile hands to one connector host process (`LIGHTSPEED_CONNECTOR_PROVIDERS`). Values: `telegram`, `whatsapp`, or both. WhatsApp additionally needs `LIGHTSPEED_CONNECTOR_WHATSAPP_MEDIA_LOCATOR_KEY`; the session directory defaults to `.lightspeed-dev/whatsapp-auth`. |
| `PORT` | `3000` | Platform server port. |
| `LIGHTSPEED_CONFIGURATOR_MCP_BIND_PORT` | `18081` | Configurator port used by the supervisor. |
| `LIGHTSPEED_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER_URL` | Unset | Retired; a nonempty value fails startup. Use a scoped service key. |
| `LIGHTSPEED_PLATFORM_CONFIGURATOR_MCP_INTERNAL_TRUSTED_HEADER` | `false` | Retired; `true` fails startup. |

The supervisor also honors all runtime, Platform, connector host, and
Configurator variables documented above.

The `full` and `runtime` development profiles warn when neither
`OPENAI_API_KEY` nor `ANTHROPIC_API_KEY` is set: they still start, and provider
API keys can be added per universe from the Platform UI (Settings →
Integrations); the Sessions view shows a banner until a usable key exists.
Pass `./dev.sh --require-api-keys` to make a missing deployment key fatal
(useful in CI). `--allow-missing-api-keys` is still accepted and is a no-op.

### Docker infrastructure

| Variable | Default | Purpose |
| --- | --- | --- |
| `COMPOSE_PROJECT_NAME` | `lightspeed-dev` | Compose project name. |
| `POSTGRES_IMAGE` | `postgres:17` | PostgreSQL image. |
| `POSTGRES_CONTAINER_NAME` | `lightspeed-postgres` | PostgreSQL container name. |
| `POSTGRES_USER` | `lightspeed` | Local database user. |
| `POSTGRES_PASSWORD` | `lightspeed` | Local database password. |
| `POSTGRES_DB` | `lightspeed` | Rust runtime database name. |
| `LIGHTSPEED_PLATFORM_POSTGRES_DB` | `lightspeed_platform` | Platform database name on the same local PostgreSQL server. |
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

`scripts/dev/env.sh` derives and exports the runtime and Platform connection URLs from
these values. The two databases are intentionally separate because their
independent migration systems contain overlapping table names. The script also
supplies a fixed local `LIGHTSPEED_SECRETS_MASTER_KEY`; that key is public
development material and is unsafe for any shared or production deployment.
The key encodes `lightspeed-local-dev-master-key!`; changing it requires
resetting local encrypted state.

## Tests and evaluations

These variables opt into external integration tests or select live-provider
fixtures. Ordinary unit tests do not require them.

| Variable | Purpose |
| --- | --- |
| `LIGHTSPEED_TEST_POSTGRES_URL` | PostgreSQL URL for Rust live store/runtime tests and the development fallback. |
| `LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL` | Scratch PostgreSQL URL used by the Platform empty-install and upgrade migration test. |
| `LIGHTSPEED_OPENAI_MODEL` | First-choice model override in hosted runtime live tests. |
| `OPENAI_LIVE_MODEL` | Shared fallback model for OpenAI live suites. |
| `OPENAI_RESPONSES_MODEL` | OpenAI Responses live-test model. |
| `OPENAI_RESPONSES_WEB_SEARCH_MODEL` | Web-search-specific Responses model; falls back to the Responses live model. |
| `OPENAI_RESPONSES_COMPACTION_MODEL` | Responses compaction live-test model. |
| `OPENAI_RESPONSES_PROMPTS_MODEL` | Responses prompts live-test model. |
| `OPENAI_COMPLETIONS_API_KEY` | Credential override for OpenAI-compatible Completions tests; falls back to `OPENAI_API_KEY`. |
| `OPENAI_COMPLETIONS_BASE_URL` | Base URL override for Completions tests; falls back to `OPENAI_BASE_URL`. |
| `OPENAI_COMPLETIONS_MODEL` | Completions live-test model. |
| `DEEPSEEK_API_KEY` | Credential for ignored DeepSeek compatibility and endpoint-routing live tests. |
| `DEEPSEEK_BASE_URL` | DeepSeek live-test endpoint; defaults to `https://api.deepseek.com`. |
| `DEEPSEEK_COMPLETIONS_MODEL` | DeepSeek live-test model; defaults to `deepseek-v4-pro`. |
| `OPENAI_AUDIO_TRANSCRIPTION_MODEL` | Audio transcription live-test model; defaults to `gpt-4o-transcribe`. |
| `OPENAI_AUDIO_TRANSCRIPTION_FIXTURE` | Repository-relative local audio fixture; overrides the remote fixture URL. |
| `OPENAI_AUDIO_TRANSCRIPTION_FIXTURE_URL` | Remote audio fixture URL. |
| `OPENAI_AUDIO_TRANSCRIPTION_EXPECT` | Optional case-insensitive text expected in the transcription. |
| `ANTHROPIC_LIVE_MODEL` | Shared fallback model for Anthropic live suites (default `claude-opus-5`). |
| `ANTHROPIC_MESSAGES_MODEL` | Anthropic Messages live-test model (default `claude-opus-5`). |

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
| `LIGHTSPEED_ARTIFACT_URL_DEMO` | Published static demo archive URL recorded in the manifest. |
| `LIGHTSPEED_ARTIFACT_URL_DOCS` | Published static documentation archive URL recorded in the manifest. |
| `LIGHTSPEED_RELEASE_CHANNEL` | `release` for a tagged build, `main` (default) for a snapshot; recorded in the envd discovery document (`envd.json`). |
| `LIGHTSPEED_ENVD_PUBLIC_URL_BASE` | HTTPS base under which the envd archives are downloadable without credentials (a tag's GitHub release assets); unset for snapshots, whose discovery document then carries `null` URLs for the serving deployment to fill in. |
| `LIGHTSPEED_ENVD_TARGETS` | Comma-separated envd targets this release publishes, baked into the server so `initialize` can report them; a local build reports its own target. |
| `LIGHTSPEED_RUNTIME_IMAGE` | Digest-pinned runtime image recorded in the manifest. |
| `LIGHTSPEED_PLATFORM_IMAGE` | Digest-pinned Platform image recorded in the manifest. |
| `LIGHTSPEED_PLATFORM_WORKERS_IMAGE` | Digest-pinned connector-host image (still published as `platform-workers`) recorded in the manifest. |
| `LIGHTSPEED_CONFIGURATOR_MCP_IMAGE` | Digest-pinned Configurator image recorded in the manifest. |

`release/metadata.env` additionally owns these release-source values. They are
consumed by build scripts and should not be used as deployment overrides.

| Variable | Purpose |
| --- | --- |
| `LIGHTSPEED_RELEASE_TARGET` | Rust release target triple for the server, provider, and CLI. |
| `LIGHTSPEED_ENVD_TARGET` | Static musl target the environment daemon is published for, so it runs on any Linux image regardless of glibc. |
| `LIGHTSPEED_PRODUCT_VERSION` | Product version used when no explicit release version is supplied. |
| `LIGHTSPEED_RELEASE_RUST_VERSION` | Pinned release Rust toolchain. |
| `LIGHTSPEED_RELEASE_BUILD_BASE_IMAGE` | Digest-pinned base used to construct the build environment. |
| `LIGHTSPEED_API_PROTOCOL_VERSION` | API protocol identifier recorded in release metadata. |
| `LIGHTSPEED_ENVIRONMENT_PROTOCOL_VERSION` | Environment protocol number the gateway and envd must match exactly; checked against the protocol crate and published in `envd.json`. Changing it is the release event that stops older daemons from registering. |
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

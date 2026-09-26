# Local development

The development launcher runs Lightspeed from your checkout. PostgreSQL,
object storage, and Temporal run in Docker; Rust and TypeScript processes run
on your machine. This keeps the durable services available while you edit,
rebuild, and restart the code that uses them.

If you want to try the product before changing it, follow
[the quickstart's source setup](../getting-started/quickstart.md#run-from-source-for-local-development).
This page picks up from that setup and explains how to choose an edit loop,
find the code that owns a behavior, and keep local state useful while developing.

## Prepare the checkout

Run the commands below from the repository root. The complete local product
needs Rust and Cargo through rustup, Node.js 24 or newer with npm, and a running
Docker daemon with Docker Compose v2. The checked-in
[Rust toolchain](../../../rust-toolchain.toml) selects the compiler. Rust
dependencies also need a native build toolchain and `protoc`, including its
standard `.proto` include files.

Start the product with:

```bash
./dev.sh
```

The launcher installs npm workspace dependencies when they are missing or the
root lockfile has changed. It starts the infrastructure, applies the Rust
database migrations, and starts the application processes. Platform applies
its own database migrations during startup. Wait for the readiness checks,
then open [http://localhost:5173/app/](http://localhost:5173/app/).

A root `.env` is loaded automatically if present. You can start without a
model API key and add a universe credential through **Models → Add provider**.
A provider-backed run still needs a valid credential for its
selected model. The [quickstart](../getting-started/quickstart.md#configure-a-model)
covers that first connection and the development account.

Plain `./dev.sh` also starts `lightspeed-envd` on the host. It runs commands as
your OS user, with a working directory under `.lightspeed-dev/envd/workspace`;
that directory is not a sandbox. Sessions must still have an environment
configured before using it. Pass `--no-envd` when your work doesn't need a
machine, or follow the
[local attachment walkthrough](../environments/bring-your-own-compute.md#direct-attachment-for-local-development)
to configure it.

## Development logins

The default authenticated `./dev.sh` (full profile) ensures a **Test** universe
and these accounts on every startup:

| Role in Test | Login |
| --- | --- |
| Admin | `admin@lightspeed.dev` (or `LIGHTSPEED_PLATFORM_ADMIN_EMAIL`) |
| Operator | `operator@lightspeed.dev` |
| Contributor | `contributor@lightspeed.dev` |
| Viewer | `viewer@lightspeed.dev` |

New accounts share `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`, defaulting to
`lightspeed-dev-password`. Admin keeps its Platform administrator role; the
other accounts receive only their listed universe role. Startup reuses existing
accounts and the Test universe, restores the listed universe roles, and preserves
existing passwords and content. Changing the password variable does not reset
existing logins.

Set `LIGHTSPEED_PLATFORM_DEV_SEED=false` to disable these fixtures. Ordinary
Platform startup and other launcher profiles leave them disabled. Seeding creates
the runtime universe through the Platform's deployment key, then records the
accounts and memberships in the Platform database. A supplied key needs
`deployment/universes` access for that step and the usual Platform permissions
for interactive work.

## Choose the processes you need

Launcher profiles select local processes. They are separate from the agent
profiles stored in a universe.

| Command | Use it for |
| --- | --- |
| `./dev.sh full` | The complete editable product: runtime, Platform API and UI, Configurator MCP, local infrastructure, and the optional local daemon. This is the default. |
| `./dev.sh runtime` | Rust runtime and daemon work against local infrastructure, without Platform or Configurator. |
| `./dev.sh platform` | Platform API and UI work against the existing runtime named by `LIGHTSPEED_API_URL`. It starts local infrastructure but does not start that runtime. |
| `./dev.sh demo` | UI work against the in-browser demo backend at `http://localhost:5175/demo/`. It needs neither Docker nor the Rust runtime. |
| `./dev.sh infra` | Just PostgreSQL, pgAdmin, MinIO, and Temporal, for manual processes or live tests. |

The supervisor tracks one application profile at a time. Use `full` for
combined runtime and Platform work; starting a second supervised profile while
the first is running is rejected. The `infra` command starts the shared
services and returns without holding an application supervisor open.

You can inspect the commands and configuration for a profile before starting
it:

```bash
./dev.sh --plan full
```

Planning still loads the development environment and can create the local
daemon's working directory. It does not start the services. The complete
override table is in the
[environment-variable reference](../reference/environment-variables.md#local-development).

The full profile uses authenticated runtime access and bootstraps a
deployment key when no Platform key is configured. The runtime-only profile
uses `single`: no key or actor, with ordinary requests pinned to the configured
universe. Keep that listener private. See
[API keys and service access](../access-and-security/api-keys-and-service-access.md).

Telegram and WhatsApp connector processes are opt-in. For example,
`LIGHTSPEED_CHANNELS_CONNECTORS=telegram ./dev.sh` enables Telegram account
discovery through the core API. Configure the corresponding account and
credentials before expecting messages to flow. Bots and Channels core already
run inside the Rust runtime; they don't require the connector host to exist.
See [Channel connectors](../integrating-and-extending/channel-connectors.md)
for connector development and setup.

## Follow a change to its owner

Before changing a behavior, identify which layer decides it. A new user action
may need a web control, an API operation, and a runtime implementation, but
those pieces have different responsibilities.

| If you are changing… | Start here |
| --- | --- |
| Agent decisions, scheduling, or replayed session state | `crates/engine`; keep decisions deterministic and represent effects as intents. |
| Public operation names, request fields, or response shapes | `crates/api`; follow [Changing contracts](changing-contracts.md) through generated consumers. |
| Durable execution, activities, or gateway behavior | `crates/temporal-workflow` and `crates/temporal-server`; distinguish workflow decisions from activity I/O. |
| Provider transport, tool execution, or persistence | The relevant adapter under `crates/`, such as `llm-runtime`, `tools`, or `store-pg`. |
| Product accounts, management routes, or the web experience | `platform/server`, `platform/db`, and `platform/web`; shared product types live in `platform/shared`. |
| Machine execution or provisioning | The environment protocol, daemon, client, or provider; see [Environment providers](../integrating-and-extending/environment-providers.md). |

Use [Architecture](../how-it-works/architecture.md) to understand those
responsibilities, then the nearest `Cargo.toml`, `package.json`, and module tests to
find the actual implementation. The workspace manifests remain the current
inventory. A domain crate can validate a record without owning the database
or network operation that eventually uses it.

For example, if a session control behaves incorrectly, trace the web request
to the public API and then to the command it admits. Check whether the error
is in the request, the admission policy, the reducer decision, or its effect.
A UI workaround may hide a bug that a CLI caller can still reach. That same
trace tells you where the regression test belongs.

## Edit, check, and restart

Vite updates the web UI as you edit. The Platform server runs through `tsx
watch` and restarts on relevant source changes. The Rust runtime and daemon
run through ordinary `cargo run`; the supervisor does not watch and rebuild
them. Configurator and the optional connector host also run without watch
mode in the full profile.

For a Rust change, run the focused test first, then stop and restart the
profile to exercise the new executable:

```bash
cargo test -p engine
./dev.sh stop
./dev.sh
```

Replace `engine` with the crate you changed. Restarting recompiles changed
Rust code and repeats startup migrations while retaining the infrastructure
and its data. After changing startup environment variables, restart the
relevant processes too.

For a web change, use its own checks while the development server stays open:

```bash
npm run typecheck --workspace @lightspeed/platform-web
npm run test --workspace @lightspeed/platform-web
```

The demo backend makes many visual and interaction changes easy to inspect
without services. Validate against the full product when the change depends
on real authorization, persistence, runtime progress, or service errors.
[Testing and evaluation](testing-and-evaluation.md) explains how
to widen checks as a change reaches more of the system.

For a manually launched Rust process or CLI, load the local connection
settings into that terminal with `source scripts/dev/env.sh`. A manual server
startup needs an explicit migration first:

```bash
source scripts/dev/env.sh
cargo run -p temporal-server -- migrate
cargo run -p temporal-server
```

Do this with the supervised runtime stopped so both processes don't claim
the same ports. The [launcher implementation guide](../../../scripts/dev/README.md#manual-runtime-roles)
also shows split-role startup and a direct CLI conversation.

## Inspect and preserve local state

The launcher prints readiness and process output in its terminal. Use
`./dev.sh status` to inspect the tracked supervisor and Compose services.
For durable work, the local Temporal UI is at `http://localhost:8233`;
pgAdmin is at `http://localhost:15080`, and the MinIO Console is at
`http://localhost:29001`. The
[service guide](../../../scripts/dev/README.md#services) lists their endpoints
and explains connecting to the two databases.

Stopping the application and deleting its data are separate operations:

| Command | Effect |
| --- | --- |
| Ctrl+C in the launcher, or `./dev.sh stop` | Stops tracked host processes; leaves Docker services and durable state available. |
| `./dev.sh down` | Stops host processes and tears down the Compose services; retains their volumes. |
| `./dev.sh down --volumes` | Also removes the Compose volumes and the data in them. |
| `./dev.sh reset` | Recreates both local databases, migrates the runtime database, and clears the Lightspeed MinIO prefix. Stop the supervisor first. |

Use reset for disposable development data when that is the intended outcome.
A changed migration checksum requires understanding the schema history;
deleting rows does not repair a ledger mismatch. See
[Changing contracts](changing-contracts.md#database-migrations) before editing
migrations, and [Upgrades and recovery](../deployment/upgrades-and-recovery.md)
when data must survive.

## Work on the documentation

The manual sources live under `docs/documentation/`. The site reads them
directly and publishes the generated API and workflow references alongside
them. From the repository root:

```bash
npm install
npm run dev:docs
```

Open `http://localhost:4321/docs/`. Source edits refresh the site. Add new
manual pages to the sidebar in `docs/site/astro.config.mjs` and give them a
path from the [documentation index](../index.md).

Run `npm run check:docs` before submitting documentation changes. It checks
the content adapter, Astro diagnostics, and the built site's links and
publication outputs. The [site guide](../../site/README.md) covers previewing
the production build, Markdown exports, and deployment details.

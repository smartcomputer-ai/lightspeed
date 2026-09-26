# Product documentation structure

Status: the initial getting-started, usage, environment, deployment,
integration/extension, internals, and development guides are drafted under
`docs/documentation/`, alongside the authoritative environment-variable
reference. The remaining reference page scopes are planned.
Astro Starlight is set up in `docs/site/` and renders the authored manual plus
two generated reference pages for hosting at `https://ls.bot/docs/`. Public deployment remains to be connected in
the infrastructure repository.

Use `docs/documentation/` as the product documentation root. `docs/roadmap/`
holds implementation plans and design history. A dedicated publishing root
makes the reader-facing manual easy to navigate and gives the documentation
renderer a clear input directory.

Organize the manual around reader tasks: get a first agent running, use the
product, provide compute, operate a deployment, integrate or extend it, understand
the internals, and contribute. Keep exact configuration and contract details in
reference pages. These are different reading modes; a first-run tutorial should
not require reading the architecture or an exhaustive configuration table.

## Proposed index

The comments below describe page scope, not additional pages. This is a compact
target structure for the first manual: keep related topics together initially
and split pages only when their content warrants it. The tree is the target
structure; the implementation progress below lists the pages written so far.

```text
docs/documentation/
├── index.md                              # What Lightspeed is; capabilities; choose a reading path
│
├── getting-started/
│   ├── concepts.md                       # Universes, sessions, runs, profiles, bots, workspaces, environments
│   ├── quickstart.md                     # Release or source setup → sign in → configure a model
│   └── first-agent.md                    # Complete one task; inspect its tools, output, and persisted session
│
├── using-lightspeed/
│   ├── sessions-and-runs.md              # Web app and CLI/TUI; history, steering, queueing, cancellation, branching limits
│   ├── models-and-credentials.md         # Model selection; provider connections, API keys, OAuth, compatible endpoints
│   ├── profiles-and-instructions.md      # Reusable setups, capability grants, instructions, limits, applying profiles
│   ├── workspaces-and-skills.md          # Persistent VFS files, workspace links, prompts, skill discovery/activation
│   ├── tools-and-mcp.md                  # Built-in tools and web access; connect MCP, discover tools, handle approvals
│   ├── bots-and-triggers.md              # Create a bot; schedules, webhooks, pollers, routing, activity, lifecycle
│   ├── subagents-and-federation.md       # Delegate with profiles and limits; coordinate independent bots
│   └── chat-channels.md                  # Telegram/WhatsApp accounts, pairing, access, routing to bots, media
│
├── environments/
│   ├── overview.md                      # When compute is needed; VFS vs real files; three environment sources
│   ├── bring-your-own-compute.md         # Outbound envd registration and keys; persistent/ephemeral identities; direct endpoints
│   ├── incus-vms.md                      # Set up the included provider, images/templates, universe bindings; cluster option
│   ├── using-environments.md            # Create/select/share; profile provisioning; sessions, bots, and sub-agents
│   ├── processes-and-jobs.md             # Run commands; background jobs; inspect output, await, cancel
│   ├── credentials.md                   # Bind and inject credentials into compute and jobs
│   ├── power-and-cleanup.md             # Readiness, pause/stop/wake, idle policies, closing, retention and ownership
│   └── networking-and-ingress.md        # Connectivity requirements; provider-supported application endpoints
│
├── access-and-security/
│   ├── overview.md                      # Universes, people, keys and actors; Platform and core checks; current evidence
│   ├── people-and-roles.md              # Sign-in, accounts, memberships, four roles, removing access
│   ├── private-and-shared-work.md       # Private sessions, one-way sharing, admin reads, bots and inherited visibility
│   ├── api-keys-and-service-access.md   # Gateway modes, key scope/groups/actors, bootstrap and integration credentials
│   ├── agent-and-tool-access.md         # Tool configuration, internal controller rules, external credentials and stopping
│   └── tenant-isolation-and-data-protection.md # Storage boundaries, shared infrastructure, encryption, compute and data
│
├── deployment/
│   ├── overview.md                      # Components, dependencies, full product vs runtime-only, deployment topology
│   ├── self-hosting.md                   # Install a coherent release; configure dependencies; migrate; start and verify
│   ├── multi-tenancy.md                 # Managing universes: creation/adoption, record reconciliation, archive and deletion
│   ├── configuration.md                 # Configure runtime, Platform, storage, connectors, Configurator; secrets and URLs
│   ├── operations.md                    # Health, logs, metrics, Temporal inspection, worker roles/scaling, storage retention
│   ├── upgrades-and-recovery.md          # Release compatibility, database migrations, backup/restore, recovery constraints
│   └── troubleshooting.md               # Startup, auth, model calls, stalled runs, compute connectivity, channel delivery
│
├── integrating-and-extending/
│   ├── api-and-typescript.md            # Authenticate; start a session/run; follow events and terminal results; client examples
│   ├── configurator-mcp.md              # Manage Lightspeed from another MCP client; deployment and access
│   ├── workflow-tools.md                # Managed sessions, external controllers, durable tools, replies and cancellation
│   ├── custom-tools-and-model-providers.md # Extension choices; tool adapters and native LLM integrations
│   ├── environment-providers.md         # Implement the public environment protocol; control/data boundaries and conformance
│   └── channel-connectors.md            # Add a chat provider through the connector boundary
│
├── how-it-works/
│   ├── architecture.md                  # Components and ownership: clients, Platform, runtime, Temporal, stores, compute
│   ├── agent-loop-and-durability.md     # Commands → events → state → intents; replay, activities, long-lived sessions
│   ├── context-and-storage.md           # Provider-native content, compaction, prompt caching, CAS, VFS, projections, collection
│   └── tools-and-controller-workflows.md # Tool execution; managed sessions; bots, channels, and sub-agents as workflows
│
├── development/
│   ├── local-development.md             # Repository map; dev.sh profiles; Rust/TypeScript loops; demo backend
│   ├── testing-and-evaluation.md        # Unit/integration/replay tests, generated checks, explicit live tests, evaluations
│   ├── changing-contracts.md            # API and workflow schema ownership; generation; database migration changes
│   └── contributing-and-releasing.md   # Contribution workflow, architectural rules, release construction and publication
│
└── reference/
    ├── api.md                           # Entry point to the generated JSON-RPC contract and TypeScript types
    ├── environment-variables.md         # Authoritative component-variable reference
    ├── session-and-profile-config.md    # Exact config fields, feature grants, defaults, limits, profile/environment intents
    ├── cli.md                           # User CLI, server administration, Platform CLI, daemon/provider commands
    ├── tools.md                         # Capabilities and logical tool identities; provider-dependent exposed names
    └── protocols.md                     # Generated workflow contract and environment control/data/registration protocol
```

## Navigation and page boundaries

The site home should offer four immediate paths: **Try Lightspeed**, **Use an
existing installation**, **Deploy Lightspeed**, and **Build with Lightspeed**.
The sidebar follows the section order above. Getting started establishes one
working path; environments and advanced features follow when the reader needs
them. The concepts page also serves as the initial glossary.

The most important Lightspeed-specific boundaries to preserve in the writing:

- **Session, run, and bot:** explain these terms before asking the reader to
  configure triggers or delegate. Sub-agents and bot federation belong in one
  introductory comparison, with separate procedures inside the page.
- **VFS and execution environments:** show that persistent VFS files work
  without a machine and that an environment has a separate real filesystem.
  Files do not synchronize automatically. Environment ownership, selection,
  sharing, and lifecycle deserve their own guide.
- **Using compute and implementing a provider:** the environments section is
  for users and operators. The provider protocol and implementation guide live
  under extensions and reference.
- **MCP in both directions:** using an MCP server as an agent tool belongs in
  the usage guide. Controlling Lightspeed through Configurator MCP belongs in
  the integration guide.
- **Local setup and deployment:** the quickstart leads with prebuilt Linux
  releases and offers `dev.sh` for local development. Self-hosting explains
  downloading matching images and binaries, services, credentials, migrations,
  and network exposure. Release construction belongs in development.
- **Configuration guidance and exact fields:** task pages explain choices and
  use small examples. Reference owns exhaustive settings and schemas.

Use the web app as the main first-agent walkthrough because it exposes the
complete product. Include CLI alternatives where supported; API consumers get
a separate end-to-end path. Each task guide should finish with a way to verify
the result and a short troubleshooting section. The deployment troubleshooting
page can index those sections alongside cross-service failure diagnosis.

## Repository material behind the index

These are writing inputs and sources to verify, rather than a proposal to
publish every existing document unchanged. Roadmaps contain implementation
history and deferred ideas; current code and generated contracts settle the
supported behavior.

| Proposed area | Existing material to use |
| --- | --- |
| Introduction and getting started | [Product overview and capabilities](../../README.md), [development launcher guide](../../scripts/dev/README.md), [Platform and web app](../../platform/README.md), and [launcher implementation](../../scripts/dev/stack.mjs). |
| Sessions, models, profiles, workspaces, skills | [Context and client views](../documentation/how-it-works/context-and-storage.md), [public API reference](../../crates/api/contract/api-reference.md), [profile DTOs](../../crates/api/src/profiles.rs), [session UI](../../platform/web/src/pages/SessionsPage.tsx), [CLI commands](../../crates/cli/src/main.rs), and [skill implementation](../../crates/tools/src/skills/mod.rs). |
| MCP and provider credentials | [Auth guide](../../crates/auth/README.md), [MCP approvals](archive/p144-mcp-approvals.md), [native MCP execution](archive/p145-native-mcp-execution.md), and [MCP discovery and tool search](archive/p150-scalable-mcp-discovery-and-tool-search.md). |
| Bots, delegation, federation, and channels | [Bots design and implementation history](archive/p130-bots.md), [sub-agent design](archive/p134-subagents.md), [bot federation](archive/p135-bot-federation.md), [channels as bot triggers](archive/p139-channels-as-bot-triggers.md), [bot lifecycle and environments](archive/p140-bot-environments-and-bot-lifecycle.md), [Bots domain](../../crates/bots/src/lib.rs), and [connector host guide](../../platform/connectors/README.md). |
| Environments | [Environment guide](../documentation/environments/overview.md), [Incus provider guide](../../crates/environment-provider-incus/README.md), [outbound daemon registration](archive/p148-key-based-outbound-environment-registration.md), [profile provisioning](archive/p125-profile-provisioned-environments.md), and [environment API types](../../crates/api/src/environments.rs). |
| Deployment and operation | [Build and release](../releasing.md), [release manifest schema](../../release/release-manifest.schema.json), [environment variables](../documentation/reference/environment-variables.md), [tenant isolation](../documentation/deployment/multi-tenancy.md), [Platform guide](../../platform/README.md), and [runtime role instructions](../../scripts/dev/README.md#manual-runtime-roles). |
| Integration and extension | [TypeScript client guide](../../clients/typescript/README.md), [Configurator MCP guide](../../platform/configurator-mcp/README.md), [workflow contract](../../crates/temporal-workflow/contract/workflow-contract.md), [environment protocol](../../crates/environment-protocol/src/lib.rs), [tool packages](../../crates/tools/src/lib.rs), and [LLM clients](../../crates/llm-clients/README.md). |
| How it works | [Architecture walkthrough](../documentation/how-it-works/architecture.md), [engine guide](../../crates/engine/README.md), [environment guide](../documentation/environments/overview.md), and the bot/channel/delegation designs above. |
| Development and reference | [Repository contribution guidance](../../AGENTS.md), [contribution policy](../../CONTRIBUTING.md), [development guide](../../scripts/dev/README.md), [evaluation harness](../../crates/eval/README.md), [API exporter](../../crates/api/src/bin/export-schema.rs), [workflow exporter](../../crates/temporal-workflow/src/bin/export-workflow-contract.rs), and [profile configuration reference generator](../../platform/scripts/generate-config-reference.mjs). |

## Implementation progress

The root launcher serves the manual with `./dev.sh docs`, also accepted as
`doc` or `documentation`. It bootstraps npm dependencies without Docker or Rust,
watches documentation edits, and stops the server through the usual supervisor
lifecycle. Standalone `npm run dev:docs` remains available.

Launcher failures now identify the failed step, retain recent command output,
and give a next diagnostic step. Readiness failures preserve the last HTTP or
connection result. Stack traces are opt-in with `--debug`; shutdown stops host
processes and reports infrastructure left in place. Isolated launcher fixtures
cover port conflicts, migration and runtime failures, and supervisor cleanup.

The initial writing batch is complete:

- [Core concepts](../documentation/getting-started/concepts.md)
- [Local quickstart](../documentation/getting-started/quickstart.md)
- [First agent](../documentation/getting-started/first-agent.md)
- [Environment overview](../documentation/environments/overview.md)
- [Bring your own compute](../documentation/environments/bring-your-own-compute.md)
- [Deployment overview](../documentation/deployment/overview.md)
- [Self-hosting](../documentation/deployment/self-hosting.md)
- [Documentation home](../documentation/index.md)

The walkthroughs use the current UI and configuration surfaces. The first-agent
example edits VFS files without compute. The self-hosting recipe builds release
images from a pinned source revision and uses existing PostgreSQL, Temporal,
and an HTTPS edge. Procedures were reviewed against source and generated
contracts; a live deployment and credentialed model calls have not been run
as part of this documentation work.

Initial-batch verification passed: all 125 local links and anchors in the new and updated
documents resolve, all 22 shell examples in the manual pass Bash syntax checks,
and the deployment's Platform environment example passes its configuration
parser. The five focused Platform configuration tests and release metadata
verification also pass. Sub-agent reviews checked the UI sequences, daemon
lifecycle, and deployment recipe against their implementations.

The second writing batch adds the eight usage guides:

- [Sessions and runs](../documentation/using-lightspeed/sessions-and-runs.md)
- [Models and credentials](../documentation/using-lightspeed/models-and-credentials.md)
- [Profiles and instructions](../documentation/using-lightspeed/profiles-and-instructions.md)
- [Workspaces and skills](../documentation/using-lightspeed/workspaces-and-skills.md)
- [Tools and MCP](../documentation/using-lightspeed/tools-and-mcp.md)
- [Bots and triggers](../documentation/using-lightspeed/bots-and-triggers.md)
- [Sub-agents and federation](../documentation/using-lightspeed/subagents-and-federation.md)
- [Chat channels](../documentation/using-lightspeed/chat-channels.md)

These continue the Acorn release example through a reviewer profile, sourced
instructions, a skill, delegated review, and an event-driven bot. Source and
contract reviews cover UI procedures, credentials, approvals, profile updates,
event delivery, and channel pairing. The guides distinguish stored history
from active context, shared files from copied setup, and event admission from
completed work. Credentialed model, webhook, MCP, and messaging walkthroughs
have not been exercised against live services.

Second-batch verification passed: all 188 local links and anchors across the
sixteen manual pages and five updated repository documents resolve. All 30
Bash examples pass syntax checks, all three fenced JSON examples parse, and
the manual has balanced code fences and no numeric roadmap references. All
seven mentioned RPC methods or namespaces match the generated contract.
The 29 focused web tests for session configuration and bot triggers, plus
the three Rust skill-frontmatter parser tests, pass. The examples were
reviewed against current UI, CLI, tool, and API contracts by the research
sub-agents; shell examples were parsed without executing their operations.

The third writing batch completes the environment guides:

- [Using environments](../documentation/environments/using-environments.md)
- [Processes and jobs](../documentation/environments/processes-and-jobs.md)
- [Environment credentials](../documentation/environments/credentials.md)
- [Incus VMs](../documentation/environments/incus-vms.md)
- [Power and cleanup](../documentation/environments/power-and-cleanup.md)
- [Networking and ingress](../documentation/environments/networking-and-ingress.md)

These add a machine-side release-note check, job dependencies and promise
lifetime, credential assignment and renewal, a standalone Incus recipe, power
and cleanup procedures, and a public application test. Source reviews cover
the current UI limitations, source-specific selection filters, interrupted
jobs after daemon restart, initial-only profile credentials, idle-policy
staging limits, and public ingress without visitor authentication. No live
Incus build/provision, credentialed integration, or deployed ingress has been
executed as part of this writing batch.

Third-batch verification passed: all 249 local links and anchors across the
twenty-two manual pages and five updated repository documents resolve. All
45 Bash examples pass syntax checks, all eleven fenced JSON examples parse,
and nineteen mentioned RPC methods or namespaces match the generated contract.
The three new public API params examples also validate against its JSON Schema.
The release-check script succeeds on a valid local fixture and rejects empty
or incomplete notes. Twenty-two focused Rust tests cover provider configuration,
ingress routing, job dependencies/retries/restart behavior, credential injection,
and idle accounting; four web subscription tests also pass. Operational shell
examples were not executed against the configured deployment.

The fourth writing batch completes the initial deployment section:

- [Authentication and access](../documentation/deployment/authentication-and-tenancy.md)
- [Multitenancy](../documentation/deployment/multi-tenancy.md)
- [Configuration](../documentation/deployment/configuration.md)
- [Operations](../documentation/deployment/operations.md)
- [Upgrades and recovery](../documentation/deployment/upgrades-and-recovery.md)
- [Troubleshooting](../documentation/deployment/troubleshooting.md)

The authoritative [environment-variable reference](../documentation/reference/environment-variables.md)
now lives in the manual. Repository links and the site adapter use the new
location, preserving the published reference URL. The old variable and
multitenancy documents are short pointers. The manual index and sidebar include
the completed deployment pages.

Source reviews distinguish Platform roles from runtime authentication, service
and operator access, tenant data isolation from shared capacity, archive from
shutdown, and runtime purge from full infrastructure cleanup. Operations covers
listener liveness, connector readiness limits, singleton environment routing,
role scaling, retention, and CAS collection. Recovery covers both databases,
Temporal, object content, keys, machine/daemon state, and connector persistence.
The existing installation and reference pages now document the 64 KiB inline
blob limit and the need for object storage above it. Model-default guidance,
archive-dialog copy, and historical migration advice were reconciled with the
current implementation.

Fourth-batch verification passed: the docs workspace's six adapter tests,
Astro diagnostics, production build, and output checks validate all 31 published
pages, four diagram blocks, internal links/anchors, assets, search, and sitemap.
All 55 Bash blocks in the 29 manual sources pass syntax checks, and all eleven
JSON blocks parse. Twenty-eight focused authentication/connector/Platform tests
and three runtime migration-ledger tests pass, as does release metadata
verification. Sub-agents researched and reviewed the six guides against current
code. No live deployment, migration, backup/restore, model call, or external
integration was executed as part of this writing batch.

The fifth writing batch adds the integration and extension guides:

- [API and TypeScript](../documentation/integrating-and-extending/api-and-typescript.md)
- [Configurator MCP](../documentation/integrating-and-extending/configurator-mcp.md)
- [Workflow tools](../documentation/integrating-and-extending/workflow-tools.md)
- [Custom tools and model providers](../documentation/integrating-and-extending/custom-tools-and-model-providers.md)
- [Environment providers](../documentation/integrating-and-extending/environment-providers.md)
- [Channel connectors](../documentation/integrating-and-extending/channel-connectors.md)

The API example continues Acorn release work with explicit retry identity,
event observation, and terminal output. The workflow example stores real
schema/recipe bytes, declares a started review tool, and demonstrates the
custom workflow's reply and recovery query. The guides distinguish ordinary
MCP calls, durable workflow tools, and compiled tool/native-model changes;
provider and connector chapters use their actual protocol boundaries.

Sub-agent source reviews cover immutable workflow declarations, completion
modes, cancellation and recovery limits, provider ownership, capabilities,
channel activity contracts, and credential boundaries. The index/sidebar and
component entry links include the new guides. Old client examples no longer
send the removed session `cwd` field, and connector authentication guidance
now correctly requires the shipped host's trusted-header gateway path.

Fifth-batch verification passed: the docs workspace's nine adapter tests,
Astro diagnostics, and production HTML/Markdown export checks validate the
current published set, including links, anchors, assets, search, diagrams, and
`llms.txt`. All 62 Bash blocks pass syntax checks and all seventeen JSON blocks
parse. The five TypeScript fences and the corrected Temporal activity example
typecheck against current client/Temporal declarations. Five mocked API scenarios
exercise completion, retry identity, failure, cancellation, and event gaps.
Provider/config examples and workflow/MCP parameters validate against generated
schemas; controller parameters match the protocol fixture with an example UUID.

The 146 focused existing tests pass: five client transport tests, nineteen
Configurator tests, twenty-six workflow/engine/helper tests, four tool/provider
unit tests, twenty-one environment protocol tests, eight environment client
tests, and sixty-three connector tests. These are local or fixture-based checks.
No model calls, deployed custom workers, chat accounts, or infrastructure-backed
provider operations were executed for this documentation batch.

The sixth writing batch adds the four how-it-works pages:

- [Architecture](../documentation/how-it-works/architecture.md)
- [Agent loop and durability](../documentation/how-it-works/agent-loop-and-durability.md)
- [Context and storage](../documentation/how-it-works/context-and-storage.md)
- [Tools and controller workflows](../documentation/how-it-works/tools-and-controller-workflows.md)

Each page was researched independently against the implementation. The writing
retains the old design walkthrough's progression from a small deterministic
loop to a complete product, while following the Acorn release editor through
specific decisions and their consequences. Four diagrams show ownership,
commit/effect ordering, CAS offloading, and a joined workflow call.

The former design material is covered by these pages:

| Former topic | Current home |
| --- | --- |
| Deterministic core and durable workflow hosting | Agent loop and durability; architecture establishes the surrounding process boundaries. |
| Provider APIs, context, compaction, CAS, and client projections | Context and storage, including prompt assembly/caching, VFS, and retained output. |
| Tool registry, borrowed compute, sub-agents, and external controllers | Tools and controller workflows, with compute ownership introduced in architecture. |
| Crate map | Architecture's source entry points and the current workspace manifest. |

Source review corrected idle-only rollover, provider-native-only compaction,
unconditional pre-run catalog refresh, and blanket controller-at-creation
claims. The pages distinguish recorded facts from effect execution, session
history from Temporal history, active context from retained bytes, and lifecycle
ownership from tool receivers. The old `docs/design.md` is removed; active
repository guidance and links now point to the relevant manual pages.

Sixth-batch verification passed: 57 focused tests cover rollover, checkpoint
replay/fallback, append retries, tool catalogs, workflow-tool bindings and
scheduling, compaction, prompt caching, and catalog prefix preservation.
The nine site adapter tests and Astro diagnostics pass. The production build
validates all 41 published HTML/Markdown pages, their discovery index, nine
diagram transforms, links/anchors, assets, search, and sitemap. All four new
diagrams also parse with Mermaid, and 162 link targets across the new pages
and updated entry documents resolve. No live services, model calls, or
credentialed tests were used for this batch.

The seventh writing batch adds the four development pages:

- [Local development](../documentation/development/local-development.md)
- [Testing and evaluation](../documentation/development/testing-and-evaluation.md)
- [Changing contracts](../documentation/development/changing-contracts.md)
- [Contributing and releasing](../documentation/development/contributing-and-releasing.md)

Each page was checked against its current implementation, with independent
sub-agent research and review for tests/evaluations, contract generation, and
release construction. The guides explain the actual watch/restart behavior,
what replay and live tests establish, evaluator assertions and output limits,
generation order and Git diff checks, compatibility across retained histories
and both databases, and the distinction between packaging, publication, and
deployment. Existing component guides retain implementation detail and link
to the manual; the index and sidebar include the development reading path.

Seventh-batch verification passed: two named replay examples, 24 API/workflow,
environment-wire, and migration-registry tests, and eleven CI/release-script
tests. The nine docs adapter tests and Astro diagnostics pass; the build
validates all 45 published HTML/Markdown pages, discovery, search, assets,
links, anchors, sitemap, and the existing nine diagrams. All 24 new Bash
blocks pass syntax checks, all three JSON examples parse, and the two API
examples validate against the generated schema. The 147 link targets across
the new pages and updated entry documents resolve. Release metadata and
`git diff --check` also pass. No live services, model calls, migrations against
a database, or release publication were executed for this batch.

The root README links to the manual. Existing development guidance now reflects
optional deployment API keys and the local daemon, the environment specification
includes registration-key filtering, and the design guide points to the workspace
manifest for its crate inventory. The README now describes fork/clone support
as core/storage primitives, matching the absence of a creation action in the
current public clients. The environment specification and overview now explain
the current idle-policy staging limit, and the borrowed-compute walkthrough
uses the supported CLI/API close path. Remaining reference work follows the
target index above.

### Access and security documentation

The access model now has a dedicated six-page section between Environments and
Deployment. Its overview follows a Platform request and a direct API-key
request through their different gates. The focused pages cover people and
roles, private/shared sessions, keys and service access, agent/tool authority,
and tenant isolation/data protection. They describe the current implementation:
people and roles in Platform; scope, key groups, attribution and internal
controller guards in core.

The old authentication and identity pages become short pointers with their
existing route and anchor targets preserved. The deployment multitenancy page
retains universe management and retirement; its isolation explanation moves
into the new section. Surrounding usage, architecture, deployment, integration,
and configuration-reference prose is reconciled in the same pass, including
role prerequisites and removed execution-identity and sharing UI.

The new pages distinguish retained session attribution from the deferred
Platform audit trail, and keep SSO, SCIM and invitations out of present-tense
capability claims. No unfinished feature pages are added to navigation.

Validation: the nine site adapter tests and Astro diagnostics passed. The
production build verified 53 HTML/Markdown pages, 15 diagrams, links, anchors,
assets, search, the sitemap, and `llms.txt`. Twenty shell/JSON examples in the
new section and affected setup/session procedures parsed without execution.
Documentation diffs pass whitespace checks. Interactive browser review was
unavailable; no live services, credentials, or agent operations were exercised.

### Manual drift and readability review

Reviewed the authored manual against current UI, runtime behavior, contracts,
and release metadata. Setup now follows Models and Access navigation, current
session filters, attachment access labels, and the simpler creation dialogs.
Environment guides distinguish provisioning from profile attachments and explain
prompt/skill discovery on either filesystem. Deployment instructions separate
the current consolidated schema from historical tagged-release upgrades;
contract-authoring guidance includes generated Platform method roles.

Shortened the home page into reading paths and reduced repeated policy,
transport, rendering, and storage detail across the guides. Architecture pages
build from the main concepts to a concrete operation; task guides keep setup,
verification, and failure recovery. Exact protocol and implementation details
remain in their contracts and source links. The review preserves operational
limits, examples, diagrams, and existing inbound anchors.

Validation: `npm run check:docs` passed the nine adapter tests, Astro
diagnostics, and production-build checks. After the final guide edits,
`npm run build:docs` again verified 53 published pages, 15 diagrams, links,
anchors, assets, search, sitemap, and Markdown exports. Syntax checks passed
for 93 shell and 23 JSON examples without executing their operations.
Documentation whitespace checks passed. No live service or credentialed tests
were run.

### Screenshot refresh after the manual review

Refreshed all seven existing screenshots with Playwright against the current
Software Factory demo. Added four focused captures for universe members,
session sharing, API-key groups, and model-provider selection. The images show
the current navigation and controls, with captions identifying demo data.
The key and sharing dialogs were captured before confirmation. The workspace
skill was entered and saved through the demo UI from the guide's example.

All eleven assets were visually reviewed. The production docs build verified
their paths and exports; Playwright checked every illustrated page at 1440px
and 390px widths, with all images loaded and no horizontal page overflow.
Desktop and mobile article layouts were also inspected visually. Capture
recipes are maintained in `docs/site/README.md`. No live runtime, provider
credentials, or model calls were needed.

## First writing pass

### UI screenshot pass

The welcome page now also includes a full-app dark-mode demo capture after
its introduction, showing navigation, a session transcript, tool activity,
and the linked sub-agent. Its refresh recipe is recorded with the other
screenshots in `docs/site/README.md`.

Core concepts gained three Mermaid diagrams beside the relevant explanations:
successive runs within one retained session, explicit transfer between VFS and
machine files, and event routing into a bot's Main and incident conversations.
The existing overview remains as the closing map. The docs build passed with
12 diagrams overall; browser review confirmed all four Core concepts diagrams
render, the welcome image loads, and the concepts page fits a 390px viewport.

Added six focused dark-mode screenshots from the local in-browser demo:

| Page and section | Capture |
| --- | --- |
| Quickstart — Start a conversation | New session dialog, before customizing the model. |
| Profiles and instructions — Create a profile for a job | Release scribe instructions and model settings in Form view. |
| Sessions and runs — Inspect what happened | Expanded tool group with the first command's Result. |
| Workspaces and skills — Add a skill | The walkthrough's saved release-review skill in the workspace tree and editor. |
| Bots and triggers — Read outcomes and replay work | PR Reviewer Activity with event #11 expanded, showing deferred, failed, and handled outcomes. |
| Using environments — Select a machine for a task | CI runner with Details expanded. |

Images live in `docs/documentation/images/`, with one relative Markdown image
per selected page, descriptive alt text, and captions identifying demo examples.
The skill was entered through the demo UI; the other populated views use seeded
Software Factory data. Captures use a 1440 × 1000 viewport, CSS-pixel PNGs, and
dialog/panel crops to keep labels readable. Refresh instructions are in
`docs/site/README.md`. No live services, credentials, model calls, or provider
operations were used.

Validation: visually reviewed all six assets and representative desktop/mobile
article layouts. All six published images loaded successfully; the 390px mobile
layout had no horizontal overflow. The docs build passed its HTML, Markdown,
link, anchor, asset, diagram, search, and sitemap checks; `git diff --check`
passed. Technical chapters retain their code examples and diagrams without
additional decorative screenshots.

Write in this order while retaining the target navigation above:

1. **A complete first-use path:** home, concepts, quickstart, first agent,
   sessions/runs, models/credentials, and profiles/instructions. Connect the
   existing API and configuration references.
2. **A complete self-hosting and compute path:** deployment overview and
   installation, authentication, operations and recovery, environment overview,
   daemon registration, Incus, and environment selection/lifecycle.
3. **The remaining capabilities and builder paths:** VFS/skills, MCP,
   bots/channels/delegation, API and workflow integration, extensions,
   architecture, and contributor guides.

The first batch establishes the introductory tutorials and self-hosting path;
the second adds the initial usage guides; the third completes the initial
environment section; the fourth completes the deployment guides and migrates
the variable reference; the fifth adds integration and extension guides;
the sixth completes internals; the seventh adds development. The remaining
reference pages are still planned.
Add orchestrator-specific recipes when the repository
supports them. Continue reconciling older guidance against executable behavior
as each new page is authored.

## Existing docs and publishing

### Site implementation

- `docs/site/` is an npm workspace with independent development, build, preview,
  and validation commands. It emits a static directory for the `/docs/` route.
- The build reads the existing Markdown, derives page metadata from its leading
  title, and rewrites relative manual links into website routes. Other source
  links lead to GitHub; local images are included in the output. Missing source
  paths and broken published links or heading anchors fail validation.
- The static output includes a `.md` version of every published page and an
  `llms.txt` index. Markdown keeps the page title, formatting, code examples,
  and Mermaid source, with links to the other Markdown pages and absolute
  asset URLs. HTML pages advertise their Markdown alternate. Stale exports
  are removed with deleted or renamed sources, and release checks require the
  index and Markdown files in the documentation archive. The site guide
  records the future Caddy `Accept` negotiation and cache-header contract;
  deployment configuration remains in the infrastructure repository.
- Environment variables, the generated API reference, and the generated workflow
  contract are staged from their authoritative sources. No reference copy is
  maintained by hand. Only authored pages and these references are published.
- Navigation follows the planned section order and the home provides four
  reading paths. Search uses Starlight's static Pagefind index; Mermaid diagrams
  render with automatic theme switching.
- Styling and copied assets follow `ls-site`: self-hosted Merriweather,
  Merriweather Sans, League Gothic, the Lightspeed mark, and the cyan/cobalt
  palette. Both light and dark modes retain Starlight's navigation and controls.
  The header uses the landing page's transparent white mark in dark mode and
  its black inverse in light mode; the favicon matches the landing page.
  The logo links to the documentation home; a separate Website link beside
  GitHub leads to the main site in the header and mobile menu.
  Dark-mode body text uses the landing-page hero's light sans-serif weight,
  with restrained emphasis and regular serif headings. Near-white body text,
  white headings, and brighter secondary text provide the hero's high contrast.
  Sidebar hover and keyboard focus use background and accent highlights so
  they remain distinct with bright text, including on collapsible groups.
  Font preloads and blocking font display prevent the immediate fallback-font
  swap during navigation; the styles themselves are present at initial render.
- The path-classified documentation job in the main CI workflow runs adapter
  tests, Astro diagnostics, and a production build with output checks. It
  participates in the required gate and is skipped for unrelated product
  changes. The site is labeled current development; release-specific manuals
  can follow when supported releases require them.

Initial site validation passed for 25 published pages, four Mermaid transforms,
theme controls, search coverage, links, anchors, and bundled assets. The six adapter
tests, Astro diagnostics, root npm checks, and release-packaging checks pass.
HTTP checks cover every page and the missing-page response; live-preview checks
cover source additions, edits, deletions, and image staging. Browser visual
review remains outstanding because the browser connection was unavailable.

The [site guide](../site/README.md) documents the commands and publishing
boundary. Snapshot and tagged-release builds produce a documentation archive
alongside the existing static demo and runtime artifacts. The release manifest
records its checksum, immutable OCI URL, and `/docs/` base path under
`artifacts.docs`; checksums, provenance, the aggregate release bundle, and
tagged-release assets include it. The existing main-build notification supplies
the release-bundle digest, so it also identifies the documentation. Docs-only
main pushes now enter the CI/snapshot chain, with only relevant CI suites
selected before the complete snapshot build. The infrastructure installer and
Caddy route remain a separate deployment task.

Release integration validation covers CI path selection, required-gate failure
handling, both manifest channels, missing or corrupted documentation, static
archive layout and reproducibility, and digest-preserving publication aliases.
The built documentation archive was extracted and compared byte-for-byte with
the site output. Registry publication and infrastructure deployment were not
run locally.

Markdown export checks validate every published page and index entry, retained
code and diagram source, working links and assets, removal of stale exports,
and HTML discovery links. HTTP checks passed for all 31 Markdown pages and the
index, and all 230 files in the documentation archive matched the static build.

### Source ownership

Keep the root README as the short product overview with a documentation entry
link. The four `how-it-works/` pages now own the architecture explanation;
`docs/design.md` has been removed after its useful material was incorporated
and checked against current code. Links to the former design document now
select the relevant architecture, agent-loop, storage, or controller page.
Other component guides can retain local implementation and maintenance detail
while linking to the manual for product explanations.

Preserve one authoritative source for each reference. In particular,
`docs/documentation/reference/environment-variables.md` now owns the variable
reference, and API and workflow contracts remain generated from Rust. The old
`docs/variables.md` and `docs/multi-tenancy.md` locations are short migration
pointers. The site discovers the moved variable page as part of the manual,
retaining its published route, and stages the two generated contracts without
creating hand-maintained copies. Keep consumers aligned when a source moves.

Use ordinary Markdown, relative links, stable descriptive filenames, and
existing repository images. Update the Starlight sidebar as pages are added. Publish completed pages as they land; keep this plan and
unfinished page placeholders out of the product sidebar.


## Writing Style

The voice reference is Lukas's hand-authored
[Agent OS design document](https://github.com/smartcomputer-ai/agent-os/blob/pre-next/docs/design/design.md).
Use it for exposition and voice. Lightspeed's current code and contracts remain
the sources for technical claims.

The distinctive quality is patient explanation by someone who has built the
system. The document develops a few primitives, follows their consequences,
and makes abstractions concrete through examples. It is conversational, uses
definitions and transitions freely, and acknowledges tradeoffs. Confidence
comes from making the mechanism understandable. Preserve that reasoning and
rhythm while editing accidental repetition and errors.

### Who reads this

Buyers and their engineers include architects, platform and security teams,
and compliance officers, often at enterprises and banks. They need enough
detail to evaluate the claims. The manual also serves people installing,
using, integrating, and developing Lightspeed. Match the page to their task:

| Page type | What the writing should accomplish |
| --- | --- |
| Product overview and evaluation | State the capability, explain its mechanism, and establish its practical value and limits. |
| Architecture and concepts | Build an understanding of how the pieces fit together and why the design takes this shape. |
| Tutorials and task guides | Help the reader complete a task, verify the result, and understand the decisions they must make. |
| Reference | Make exact behavior, fields, defaults, prerequisites, and errors easy to find. |

Assume technical competence, but introduce Lightspeed-specific vocabulary.
Knowing what a workflow engine is does not tell someone what a Lightspeed
session owns or how a bot differs from a sub-agent. Explain familiar concepts
briefly when their precise meaning matters to the argument.

### Voice

- State what Lightspeed does and explain why. Use concrete subjects and verbs:
  the gateway admits a command, the core emits an intent, an activity calls a
  provider. Avoid corporate enthusiasm and unsupported superlatives such as
  "world-class" or "rock-solid".
- Name Lightspeed or the responsible component. Reserve "we" for reasoning
  with the reader; use "you" for the reader's actions and choices. Keep the
  house preference against "we believe" and company "our", even though the
  historical sample sometimes uses them.
- Sound like an engineer explaining the system to a peer. Contractions and
  occasional informal asides are welcome. Directness can be warm; it does not
  require every sentence to sound like a specification.
- Put the main point early. A paragraph can start with a constraint, an
  observation, or the next step in the explanation. Develop the point with
  evidence and consequences instead of repeatedly increasing its intensity.
- Name real products and mechanisms when they clarify the explanation:
  Temporal, PostgreSQL, S3, Telegram, WhatsApp, vLLM. Comparisons must describe
  a specific, verified difference.
- Show arithmetic when scale or cost is the point. State assumptions and
  distinguish illustrative numbers from measured results. Prices need a
  dated source; performance figures need conditions. An idle agent's low
  compute use does not establish that its total operating cost is zero.
- State implemented behavior, limitations, and intended future work distinctly.
  A design goal is not a guarantee. Put material qualifications beside the
  claim they qualify.

### How to develop an explanation

Start with the problem or property that matters, introduce the smallest model
needed to explain it, and follow one concrete operation through that model.
Then explain what follows from the mechanism: what it enables, what it costs,
or what the reader must account for. This is a useful progression, not a
mandatory template for every page.

- Build on what the reader has just learned. Introduce a new component when
  its job becomes clear. If an explanation depends on storage, establish the
  relevant storage model before describing execution against it.
- Use causal connections: because, so, therefore, which means. A second
  sentence should explain, demonstrate, or qualify the first. Restate an
  abstraction when the restatement gives the reader a more concrete view.
- Define a term near its first meaningful use, preferably through what it
  does. Keep the same names through the explanation, code, and diagram so the
  reader can follow the same objects throughout.
- Guide the reader when the dependency or change of level needs explaining.
  A short transition into an example or a reminder of an earlier invariant
  is useful. Remove generic announcements that add no information. Address
  likely objections in the prose without turning each paragraph into a
  rhetorical question and answer.
- Keep a page focused on its reader's task. An overview should establish the
  model before introducing exceptions; a task guide should get the reader to
  a working result. Link to the contract for exhaustive fields and to the
  implementation for algorithms, rendering details, or internal bookkeeping.
- When a feature changes, revise the explanation where it belongs. Avoid
  accumulating implementation notes at the end of a paragraph or adding the
  same qualification to every related page. Keep a material limit beside the
  claim it qualifies and link to its fuller explanation.
- Use familiar systems as analogies, then identify where the analogy stops
  helping. Follow with the actual data flow, code, or behavior. An analogy
  should make the implementation easier to understand.
- Give code and diagrams a job. Introduce what to notice, keep the example
  focused, and explain the result afterward. Show a lower-level mechanism
  when it helps explain what a convenient API handles for the user.
- Use connected paragraphs for reasoning, numbered steps for procedures and
  sequences, and lists or tables for parallel facts. A short recap can make a
  long explanation easier to retain; it should collect the model the reader
  has now learned.

### Examples from the voice reference

These brief excerpts illustrate specific moves, not phrases to repeat in every
page. The technical subject matter belongs to the historical document.

| Excerpt | What to carry into Lightspeed documentation |
| --- | --- |
| "Let's first consider Git." | Begin with a familiar model. [Object store section](https://github.com/smartcomputer-ai/agent-os/blob/pre-next/docs/design/design.md#grit-object-store). |
| "To make it a bit more concrete" | Move from design to a worked example. [Prototype section](https://github.com/smartcomputer-ai/agent-os/blob/pre-next/docs/design/design.md#python-prototype). |
| "Or another way to look at it:" | Restate a relationship from another useful angle. [Data structure section](https://github.com/smartcomputer-ai/agent-os/blob/pre-next/docs/design/design.md#grit-data-structure). |
| "there is no magic" | Expose what the abstraction does underneath. [Low-level API section](https://github.com/smartcomputer-ai/agent-os/blob/pre-next/docs/design/design.md#low-level-api). |

The following paragraphs are newly written Lightspeed examples, based on the
[current architecture](../documentation/how-it-works/architecture.md) and [environment model](../documentation/environments/overview.md).
They demonstrate how to apply the guide rather than quote the older document.

An explanation that starts with a task and follows it to a boundary:

> An agent can keep notes and edit files in its VFS workspace without an
> operating system attached. Suppose it now needs to run a test suite. That
> requires an execution environment, which provides a real filesystem and
> processes. The session selects an environment, and process tools run there.
> The files in the VFS remain separate, so anything the test needs from that
> workspace must be transferred explicitly.

An explanation that connects a design constraint to a mechanism:

> Temporal records activity inputs and results in its workflow history. If
> every model response and tool result passed through that history in full,
> a long-running session would accumulate a large amount of data. Lightspeed
> stores those payloads in content-addressed storage and passes references
> between the workflow and its activities. The activity can load the bytes
> when it needs them, while the workflow records the smaller reference. This
> keeps the data crossing that boundary small as the conversation grows.

### Sentences and punctuation

- Mix short statements with longer explanatory sentences. Keep a connected
  cause and consequence together when that makes the reasoning easier to
  follow. Break a sentence when it starts carrying a separate argument.
- Use a colon to introduce an explanation, example, or compact enumeration.
  Use ordinary conjunctions when the relationship needs to be spelled out.
- Em dashes are occasional asides, spaced ( — ). Prefer a full stop when the
  aside becomes a second argument. Documentation has no mandatory dash count.
- Parentheses can hold a small clarification. Put important limits in the
  main sentence or a separate sentence where readers will see them.
- Use normal punctuation in documentation. A comma splice may occasionally
  suit short product copy, but should not become the default sentence rhythm.
- Use italics sparingly for emphasis and bold for useful scanning, including
  UI labels and list lead-ins. There is no required italicized word per page.
  Use backticks for code identifiers, commands, paths, and literal values.
- Use American spelling. Write round rhetorical quantities in words
  ("a thousand agents", "weeks to months") and arithmetic and specifications
  in digits ("3,200 machine-hours", "2 vCPUs").

### Short product copy

Landing and short topic copy can be tighter than the manual. Lead with the
claim and follow it with the mechanism or concrete consequence that earns it.
Prefer positive statements of capability. Define terms only when needed for
that claim, and keep navigation announcements out of the opening.

Keep emphasis restrained: usually one stressed word is enough for a short
piece, and at most one or two spaced em-dash asides. A "Rests on:" label can
introduce supporting mechanisms if the page's format calls for it; it is not a
required ending for documentation. These are choices for short copy, not
restrictions on explaining a complex system.

# Platform modules on a shared Lightspeed deployment

**Status** — Sessions, Bots, and Artifacts are installed as Platform modules on this branch. Moving the Bots worker and its storage out of Rust is a separate proposal.

Lightspeed teams can ship a feature inside the shared Platform as a **module**. The distribution selects which modules to bundle. Each organization chooses which of those modules to enable. Platform hosts their pages and browser APIs, and checks permissions on the operations they use. The core runtime continues to own the session lifecycle.

## Try the prototype

Run `./dev.sh demo`. In **Software Factory**, open **Bots → Planner** and its conversation. In **Organization settings → Modules**, turn off **Sessions** and return to Planner. Its transcript and composer still work. Switch Sessions back on, open a bot-managed session in Sessions, then turn off **Bots**. The standalone conversation still works, with the recorded workflow kind and no bot-management actions. A direct visit to the disabled module’s page shows its unavailable state.

For a module that needs no browser API of its own, open **Workspaces**, select a workspace, and choose **View artifacts**. Publish a file and open its preview. Artifacts uses existing VFS operations and permissions. Disabling it removes the page and workspace action. The file stays in the workspace.

## What this branch implements

| Owner | Implemented on this branch |
| --- | --- |
| Sessions module | Standalone routes, filtered and paginated session browser, list preferences, new-session form, and private browser endpoints for listing and creating sessions. [Web](../../../modules/sessions/web/index.ts) · [API](../../../modules/sessions/api/index.ts) |
| Bots module | Bot list and editor, chat and activity routes, triggers and events, controller links and actions in a shared conversation, and the private browser endpoints for these features. [Web](../../../modules/bots/web/index.ts) · [API](../../../modules/bots/api/index.ts) |
| Shared conversation UI and Platform gateway | Both modules can open a conversation by ID. They share the transcript, composer, settings, and individual session operations including messages, approvals, and child sessions. Deployment-admin purge uses a separate permission. [Conversation UI](../../../platform/session-ui/SessionDetail.tsx) · [Browser handlers](../../../platform/backend/src/routes/conversations.ts) |
| Rust runtime | The public session and bot contracts, durable records, and Temporal execution. Platform exposes these contracts to SDK and CLI callers through its authenticated gateway. [Gateway](../../../platform/backend/src/runtime-http.ts) |

```mermaid
flowchart LR
    sessions["Sessions page"] --> collection["Sessions browser API<br/>list and start"]
    bots["Bots pages"] --> management["Bots browser API<br/>manage bots, triggers and events"]
    sessions --> view["Shared conversation view"]
    bots --> view
    view --> detail["Shared conversation API<br/>read, message, approve"]
    collection --> client["Platform's caller-scoped runtime client"]
    management --> client
    detail --> client
    client --> core["Rust runtime"]
    sdk["SDK or CLI"] --> gateway["Public Platform gateway<br/>/api/v1"]
    gateway --> core
```

The standard distribution wires up the web entries and private API handlers. For example, opening the Sessions list calls `GET /api/ui/universes/:id/modules/sessions`. The host authenticates the browser, resolves the universe and its organization, and checks that Sessions is enabled. The module calls `session/list` on the client the host supplies. That client applies the caller’s permissions and visibility restrictions. A request to a disabled module returns 404. Bots also requires the universe’s existing Bots feature switch. [Distribution catalog](../../../distributions/standard/catalog.ts) · [API assembly](../../../distributions/standard/api.ts) · [Host admission](../../../platform/backend/src/module-routes.ts)

Organization choices persist in PostgreSQL in the running Platform. The demo stores them in the browser until reload. Today an organization switch controls access to a module’s browser surfaces and private endpoints. Public SDK operations retain their own permission checks and runtime feature policies. The design below describes how installation could govern separately deployed services and workers in the future.

## How teams contribute and ship

| Contribution | What the user sees | How an update ships |
| --- | --- | --- |
| **Native Platform module** | Pages, settings, or actions integrated into the shared shell. | When one team updates its UI package, CI tests it with the shell. That change can ship on its own in the next shared frontend deployment. |
| **External integration** | A link to a separately hosted application, or a headless service that uses the Platform APIs. | The integration team deploys its own UI and services on its own schedule. It calls Lightspeed with scoped credentials. |

The distribution selects native UI packages at build time. At run time, configuration can activate a bundled page or supply something the shell already knows how to render, such as a navigation link. A team that needs a new React component inside the shell contributes it to a native package. Its backend can still ship independently as long as its API stays compatible. Independently loaded transcript extensions would be a separate design. These choices apply equally to open-source, proprietary, and client-built features.

## How a module becomes available to a team

The deployment operator decides which modules to offer each organization. An Organization Admin enables a module once. If it provides features in universes, all current and future universes in the organization inherit that choice by default. The admin can limit it to selected universes when needed. A module that works entirely at organization scope can contribute an organization page directly.

```mermaid
flowchart TB
    definition["Module definition<br/>ID and optional contributions"]
    definition -->|Distribution includes| native["Native module<br/>UI built into shared shell"]
    definition -->|Operator registers| external["External integration<br/>Own UI or headless"]
    native --> catalog["Deployment catalog"]
    external --> catalog
    catalog -->|Organization admin enables| installation["Organization installation<br/>Settings, grants and optional universe limits"]
    installation --> shell["Shared web shell<br/>Bundled pages or app links"]
    installation --> api["Platform API<br/>Permission checks"]
```

## What stays in core

The **Core runtime** owns sessions, runs, durable state, and tool execution. It should build and run with the entire `modules/` directory omitted. Modules are clients of its published API, event stream, and workflow protocols. Core can bind a session to a controller and notify it when a run finishes without knowing which product feature implements that controller. Core also enforces deployment limits on capabilities such as VFS and web search. A universe or session can narrow those limits further. [Session and workflow contracts](../../documentation/integrating-and-extending/workflow-tools.md)

**Platform** owns identity, organizations, universes, permissions, module availability, and the shared shell and gateway. Browsers, CLI clients, and module services normally send HTTP requests through the gateway. Module services use credentials scoped to the installation and universe. A trusted worker participating in an admitted workflow can exchange protocol signals directly over Temporal. The operator decides separately which UI packages to bundle and which workers may access Temporal. [Workflow transport and trust](../../documentation/integrating-and-extending/workflow-tools.md#preserve-integration-and-delivery-semantics)

## Proposed ownership for Sessions, Workspaces, and Bots

| Module | What the module owns | How a request runs |
| --- | --- | --- |
| **Sessions** | Standalone session browser and launcher, including their browser endpoints. | The module uses the shared conversation view and runtime client. Other clients can still start sessions when the distribution omits the module. |
| **Workspaces** | The file browser and editor for VFS. It can also contribute a handler to the Platform backend if an editor action needs server-side logic. | Platform authorizes the request. The handler can compose Core workspace operations, and Core owns the snapshots and blobs. |
| **Bots** | Bot pages, public operations, records and migrations, and a Temporal worker for controllers and triggers. | Platform forwards authorized bot operations to the Bots API. The worker calls Core through the gateway and receives workflow notifications over Temporal. |

```mermaid
flowchart TB
    subgraph shell["Shared Platform web shell"]
        sessions["Sessions pages"]
        workspaces["Workspace editor"]
        botsUI["Bots pages"]
    end
    sessions --> gate["Platform API gateway"]
    workspaces --> gate
    botsUI --> gate
    gate -->|session and workspace operations| runtime["Core runtime<br/>Sessions, runs and VFS"]
    gate -->|editor operation| editor["Workspace handler in Platform"]
    editor -->|workspace operations| runtime
    gate -->|bot operations| bots["Bots API service"]
    subgraph module["Optional Bots backend"]
        bots --> tables[("Bot tables")]
        bots -->|signals via Temporal| worker["Bot worker"]
        worker --> tables
    end
    worker -->|scoped HTTP session and run requests| gate
    runtime -.->|completion signal via Temporal| worker
```

An organization can disable the Workspaces module while its agents still use VFS. To disable VFS execution for a deployment, the operator sets a Core capability limit. Enabling Bots for a second organization lets its authorized members use the Bots service and worker already running in the deployment.

## A proposed independent Bots service

Suppose Acme enables Bots for its support universe. A Contributor opens **Bots → Support triage → Activity** and sends the bot an event. The page, the public bot operations, and the process that does the work all belong to the Bots team. They ship in different places.

The **Bots UI package** contributes the `/u/:handle/bots` routes and their navigation entry. Sending an event is a domain operation, so the Activity page, CLI, and SDK use the same supported `bots/events/admit` API. A UI-specific endpoint may assemble the activity history and session summaries for that page using the caller's permissions on each resource. The **Bots worker** runs the controller and trigger workflows. An Activity page change ships with the shell. A compatible trigger fix can ship with the worker alone.

Bots owns its records, triggers, events, and database migrations. It could use a separate database or a schema in the shared PostgreSQL instance, where the module's database role determines which tables it can access. A deployment that omits the entire module should still be able to run sessions and collect unused blobs. Today, core session access queries and blob collection both reference bot tables. Their replacements must preserve session visibility and retain blobs that bot events still use. [Session access](../../../crates/store-pg/src/access.rs) · [Blob collection](../../../crates/store-pg/src/cas_sweep.rs)

When the Contributor sends an event, the gateway checks that Bots is enabled in the support universe, that the caller has `invokeBot`, and that their API key permits the operation. The worker then acts under two scopes of authority. [Bot permissions](../../../platform/backend/src/routes/method-actions.ts)

- **Installation.** The worker authenticates as Acme's Bots service. The gateway checks the target universe and the installation's grants on each HTTP request.
- **Controller.** Support triage starts a managed session. Core binds the session to its authenticated controller and checks that binding on later control requests. Release Assistant, another bot in the same installation and universe, may be allowed to read the conversation but cannot control it. Today's worker supplies controller identity in-process. The new boundary must preserve that authority across an authenticated call. [Bot worker](../../../crates/temporal-runtime/src/worker/bots.rs) · [Controller authority](../../../crates/temporal-runtime/src/gateway/service/controller.rs)

```mermaid
sequenceDiagram
    actor U as Support Contributor
    participant P as Platform API
    participant B as Bots API
    participant W as Bots worker
    participant R as Session runtime
    U->>P: Send an event from the Activity page
    P->>B: Admit event for this caller and universe
    B->>B: Record the event in the Bots database
    B->>W: Signal the support bot's controller via Temporal
    W->>P: Start or reuse session as Support triage
    P->>R: Forward authorized request with controller identity
    R->>R: Establish or verify the session's controller binding
    W->>P: Submit run and request notification
    P->>R: Forward for controller check and run admission
    R-->>W: Run completed (Temporal signal)
    W->>W: Persist outcome for the original event in Bot tables
```

### What the Bots example must demonstrate

Before committing to a TypeScript rewrite, exercise one event end to end using only published contracts. Restart the worker after Core accepts the run but before Bots records that acceptance. On retry, the worker must recover the same session and run, and record the outcome once even if the completion notification arrives twice. Then have Release Assistant try to control that session using its own controller identity. Core must refuse the request even though the installation grant covers both bots.

## Shared design and product language

Use a shared session view for the transcript, composer, and session attribution. Bots and Sessions both embed Platform’s `SessionDetail`. The shell asks enabled modules whether they can present the recorded controller. For “Started by Support triage,” the view owns the wording, translations, placement, and fallback. Bots supplies the display name and, when the caller may open the bot, a link. If Bots is disabled later, the session still shows its recorded workflow kind as plain text. [Bot chat](../../../modules/bots/web/components/chat.tsx)

The broader question remains open. When a module introduces a new visual pattern or term, what would justify moving it into the shared design system? For general-purpose UI slots or transcript renderers, identify a concrete feature that needs an extension point before defining its interface.

## How configuration could read

These YAML excerpts illustrate three decisions. The field names are proposals for discussion.

**1. The publisher describes what the module contributes.** This excerpt declares the actions Platform checks before forwarding two bot operations. Under the current role mapping, a Contributor can send an event, while editing a trigger requires Operator or Owner.

```yaml
id: bots
operations:
  bots/events/admit: { action: invokeBot }
  bots/triggers/put: { action: manageBot }
```

**2. The distribution selects native modules. The deployment operator makes services available.** The distribution's list selects the web and API contributions for the shared application. Sessions contributes pages and private browser handlers. In the proposed service design, Bots also contributes public operations, so the operator supplies its service endpoint. Acme's approvals link opens the client's own UI.

```yaml
distribution:
  nativeModules: [sessions, workspaces, bots]
deployment:
  modules:
    bots:
      service: http://bots.internal
    acme.approvals:
      appUrl: https://approvals.acme.example
      organizations: [acme]
```

**3. Acme's admin enables modules for the organization.** Approvals is available across Acme. The admin limits Bots to the support universe, where the team can configure individual bots, triggers, and credentials. A newly created research universe inherits Approvals automatically.

```yaml
kind: organization
handle: acme
modules:
  acme.approvals: { enabled: true }
  bots: { enabled: true, universes: [support] }
```

The organization installation governs who can use Bots and in which universes. It also gives the operator a scope for lifecycle decisions.

- **Access.** Removing a universe from the installation closes access to the module there. Today, turning off Bots in a universe gates member requests while scheduled work can continue. Existing universe choices should carry over.
- **Suspension.** Define what happens to the next trigger, queued events, and a run already in progress. Today's per-bot **Pause** stops schedule and poll production and pauses delivery while an active run continues. Installation-wide suspension needs an explicit policy for each of those cases.
- **Deletion.** Define when Bots removes its triggers, events, and records, and how Core's session retention policy applies to conversations that outlive the bot. [Current Bots lifecycle](../../documentation/using-lightspeed/bots-and-triggers.md#pause-update-reset-or-close)

## Proposed project structure

Lightspeed stays in one repository. `core/` builds the Rust `lightspeed-runtime` executable, `platform/` contains the reusable hosts, and `modules/` keeps each feature's code together. `distributions/standard/` assembles the shared deployment used by many organizations. A compatible module service or worker can ship independently.

```text
core/
  Cargo.toml                 Rust workspace for Core
  Cargo.lock
  crates/
    api/                     Public Core API contract
    …                        Session engine, workflows, storage, runtime adapters

platform/
  web/                       Shared shell, router, and navigation registration
  session-ui/                Shared session view used by Sessions and Bots
  backend/                   Authentication, permissions, gateway, and route registration
  db/                        Platform-owned records and migrations
  shared/                    Platform API contract and module registration types

modules/
  sessions/
    web/                     Standalone session browser and launcher
    api/                     Browser adapters for list and create
  workspaces/
    web/                     VFS browser and editor
    api/                     Editor-specific handlers hosted in Platform, if needed
  bots/
    module.ts                Module ID, availability defaults, and required runtime feature
    web/                     Bot pages, settings, and navigation
    api/                     Browser handlers mounted by Platform
    service/                 Proposed independent service for public bot operations
    worker/                  Temporal workflows and activities
    db/                      Bot tables and migrations

distributions/
  standard/
    catalog.ts               Native modules included in the shared distribution
    web.ts                   Supplies selected page and navigation exports to the shell
    api.ts                   Wires selected operations to handlers or service routes

clients/
  cli/                       Lightspeed command-line client
  python/                    Public Python SDK
  typescript/
    core-client/             Typed calls to the Core API
    sdk/                     Public @lightspeed-ai/sdk package

environments/                Environment daemon and standalone providers
```

### Where modules are registered

Only the distribution's assembly code depends on both the hosts and the concrete modules. Module metadata uses `@lightspeed-ai/platform-shared`. Browser code uses `@lightspeed-ai/platform-web/modules` and its feature interfaces. A hosted browser handler uses `@lightspeed-ai/platform-backend/modules` to receive the admitted caller and runtime client.

- **Frontend.** `distributions/standard/web.ts` supplies the selected UI contributions, such as the pages exported by `modules/bots/web/`. The shell builds its router and navigation from these exports. At sign-in or when switching organizations, it shows entries according to the organization's installations and the caller's permissions.
- **API.** `distributions/standard/api.ts` supplies the selected operation descriptors and their handlers or service routes. Today the host mounts private browser handlers under `/api/ui/universes/:id/modules/:moduleId`. Sessions registers list and create handlers there. In the proposed service design, the gateway could also mount module-owned public operations under `/api/v1` and forward authorized calls to a separately deployed service.

The web and API entries for a feature share its module ID. The deployment starts the Bots worker separately. Temporal workflows, task queues, and database migrations belong to Bots and run under its deployment lifecycle.

The TypeScript Core client lives under `clients/` because both Platform and the public SDK use it. Core owns the session and run contract. Platform owns the contract for operations such as managing organizations. The arrows show build dependencies, including contracts used to generate bindings.

```mermaid
flowchart LR
    backend["Platform backend"] --> client["TypeScript Core client"]
    sdk["Public TypeScript SDK"] --> client
    client --> core["Core API contract"]
    backend --> platform["Platform API contract"]
    sdk --> platform
```

Application developers install `@lightspeed-ai/sdk` and point it at Platform. Module workers can use bindings for their own language with the same gateway and their scoped credentials. The Platform backend uses the smaller Core client to forward authorized calls to its private runtime endpoint.

Core has its own build and CI job. A change to a Bots page runs checks for the module and shell and ships with the shared frontend. A worker-only fix can ship with the worker. A Core API change also triggers checks for the clients and modules that consume the affected contract.

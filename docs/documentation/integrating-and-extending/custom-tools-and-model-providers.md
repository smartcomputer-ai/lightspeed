# Extend tools and model providers

Lightspeed can reach additional tools and models through several boundaries.
Some integrations need only a configured endpoint and capability grant. Others
change the runtime's executable code. Choose the smallest boundary that
supports the behavior so the integration keeps the existing admission,
credential, and durability rules.

| Requirement | Extension path |
| --- | --- |
| Call an external service through ordinary tool requests | Register a native MCP server and grant it to the profile/session. |
| Start or coordinate durable external work | Declare a workflow tool and run its receiver or execution on a custom worker. |
| Add a built-in runtime capability | Implement and register a compiled tool, including its admission and execution paths. |
| Use another service speaking Responses or Chat Completions | Configure an OpenAI-compatible provider connection and select its model route. |
| Support another model wire protocol | Implement a native client/materializer and extend the compiled API-kind boundary. |

The runtime has no general plugin-directory loader or public RPC for installing
an arbitrary Rust handler. A compiled extension becomes part of a new runtime
build. MCP and [workflow tools](workflow-tools.md) provide the existing
boundaries for separately deployed implementations.

## Add an external tool through MCP

A native MCP server is the usual starting point for an external request/reply
tool. Lightspeed handles discovery, tool admission, credentials, and execution
through the stored server connection. The session or profile must also grant
the MCP feature and select the server; registering an endpoint does not expose
its tools to every agent.

Native execution supports the existing Responses, Chat Completions, and
Anthropic Messages routes. Provider-hosted MCP has different reachability and
capability constraints. Choose deliberately, and test discovery, one valid
call, an argument error, and the expected approval behavior. For private
endpoints, both the record and the deployment network allowlist must permit
access. See [Tools and MCP](../using-lightspeed/tools-and-mcp.md).

If the service owns work that must outlive a request or coordinate durable
steps, use the workflow-tool contract. Its declared receiver and completion
policy let the session track the work without adding a feature-specific
transport to the stable session worker.

## Add a compiled tool

A compiled tool needs more than a function that performs the operation.
Lightspeed must know its stable identity, how to expose it to the selected
model API, which sessions may call it, and how to execute it safely under
retry and cancellation.

The [web-fetch implementation](../../../crates/tools/src/web/fetch.rs)
illustrates a small complete tool: typed arguments and output, a function
definition and input schema, argument decoding, bounded effectful execution,
and both structured output and model-visible text. Follow that separation
when adding an operation such as a release artifact validator.

The main implementation steps are:

1. Define a durable logical tool ID, input schema, output shape, and execution
   policy. Decide whether the operation reads, mutates, or starts external work.
2. Add definition registration and resolution so the model-facing function
   maps back to that logical tool and its binding.
3. Implement the operation in an effectful adapter. Return structured results
   and an appropriate model-visible representation; bound inputs and output.
4. Add the hosted dispatch path and derive availability from explicit session
   capability grants. Verify that removing the grant removes the installed tool.
5. Test admission, execution, errors, cancellation/retry behavior, and each
   supported provider-facing representation before building the release.

These code locations explain how the existing paths fit together:

| Source | Responsibility |
| --- | --- |
| [Tool definitions](../../../crates/tools/src/definitions.rs) | Register and resolve stable logical identities, function definitions, and bindings. |
| [Toolset](../../../crates/tools/src/toolset.rs) | Construct the selected tool surface and provider-specific presentation. |
| [Tool runtime interfaces](../../../crates/tools/src/runtime/mod.rs) | Typed invocation/output helpers and runtime contracts. |
| [Inline dispatch](../../../crates/tools/src/runtime/inline.rs) | Execute supported compiled operations with the correct context. |
| [Session tool preparation](../../../crates/temporal-server/src/gateway/service/session_preparation.rs) | Materialize the desired tools from session capabilities and current registry records. |
| [Workflow preparation](../../../crates/temporal-workflow/src/workflows/session/preparation.rs) | Order preparation and publish tool changes at safe session boundaries. |
| [Hosted tool execution](../../../crates/temporal-server/src/worker/session_tools.rs) | Assemble runtime adapters for session tool batches. |

The provider-facing function name is not necessarily the logical ID. Existing
tool presentations differ by model API, and returned model calls must resolve
back to the admitted registry entry. Test both the rendered definition and
the reverse mapping. A model inventing a plausible function name does not
grant access to an operation that was not installed.

Keep VFS and machine operations in their respective contexts. A VFS tool uses
workspace storage; an environment tool uses the selected environment's
filesystem and process boundary. Avoid making an adapter silently copy files
or execute on the runtime host because its intended context is missing.

### Decide retry behavior explicitly

Tool execution policy includes an execution class and whether the operation
is retry-safe. Those are trusted implementation facts, not fields the model
can use to authorize its own retries. Read operations may be safe to repeat;
creating a process or mutating a remote system needs downstream idempotency
before automatic repetition is appropriate.

Design the domain operation around a stable idempotency key when retries are
necessary. Cancellation can stop waiting or interrupt an activity without
undoing effects already completed. Return enough structured information for
the caller to reconcile an ambiguous outcome.

The deterministic engine records facts and emits intents; it performs no
network, shell, filesystem, database, or workflow I/O. If a new capability
requires reducer-visible state or branching, keep that state provider-neutral
and add replay coverage. A tool that can use existing execution semantics
does not need a new engine-specific transport.

## Configure another compatible model service

If the service implements an existing OpenAI-style API, configure an endpoint
before considering a native adapter. The provider record selects transport
and credentials; the session selects provider ID, API kind, and model name.

For example, the following are `auth/providers/create` parameters for a
credentialless loopback service:

```json
{
  "providerId": "model:local-models",
  "displayName": "Local models",
  "config": {
    "type": "modelEndpoint",
    "endpoint": {
      "baseUrl": "http://127.0.0.1:11434/v1",
      "apiKinds": ["openai:completions"]
    }
  }
}
```

The authentication provider record includes the `model:` prefix. A session or
profile's model configuration uses the provider ID without that prefix:

```json
{
  "model": {
    "providerId": "local-models",
    "apiKind": "openai:completions",
    "model": "<installed-model-id>"
  }
}
```

Replace the model placeholder with one served by the endpoint. Loopback refers
to the host or container of the process making the request. Gateway model
discovery and worker generation can originate from different processes, so
both need the intended route.

The base URL includes the API path when the service requires it. The client
appends the appropriate Responses or Chat Completions path. Hosted endpoint
validation requires HTTPS except for loopback and rejects embedded credentials,
queries, and fragments. Extra headers are non-secret settings and cannot
replace reserved authentication or transport headers.

For an authenticated endpoint, use the API's `modelApiKey` configuration and
credential field, or configure it through the Platform. Keep the secret out
of endpoint URLs and profile documents. A custom provider needs its stored
connection; it does not borrow a built-in deployment key when that record is
missing. See [Models and credentials](../using-lightspeed/models-and-credentials.md)
for the UI flow and fallback behavior.

Compatibility still needs validation. A service can accept one model request
while rejecting tool schemas, media, or reasoning settings. The runtime also
has targeted request adjustments for exact provider IDs such as `deepseek`
and `openrouter`; assigning an arbitrary alias does not select those rules.

## Add a native model integration

A provider ID names a configured connection. An API kind identifies its wire
protocol. Adding another provider ID does not add a new protocol, and a new
wire format requires a coordinated compiled change.

Use the existing implementation layers as the source map:

| Layer | What a native integration supplies |
| --- | --- |
| [Native clients](../../../crates/llm-clients/README.md) | Provider request/response types, transport, streaming where supported, and provider error handling. |
| [Runtime executor](../../../crates/llm-runtime/src/executor.rs) | Generation and compaction adapter implementations and retry classification. |
| [Provider credentials](../../../crates/llm-runtime/src/provider_keys.rs) and [endpoint admission](../../../crates/auth/src/providers.rs) | Admit supported endpoint/API kinds and resolve the configured connection and credentials at the effectful boundary. |
| [Tool catalog materialization](../../../crates/llm-runtime/src/tool_catalog.rs) | Render admitted tool definitions and map model calls back to their identities. |
| [API route conversion](../../../crates/temporal-server/src/gateway/service/api_config.rs) | Validate the public model route against supported kinds. |
| [Hosted adapter assembly](../../../crates/temporal-server/src/worker/activities/state.rs) | Register generation and compaction implementations used by workers. |

The compiled `ProviderApiKind` and public/generated consumers must agree with
the new kind. Custom endpoint admission currently permits only Responses and
Chat Completions, so extend endpoint/credential validation where the new kind
needs it. Update model discovery and selection surfaces too, so a route
the UI offers can actually be materialized and executed. Within an existing
kind, a provider-specific request adjustment may be sufficient; avoid adding
a new kind solely for another service endpoint.

Preserve native assistant, tool, and reasoning payloads in CAS. The runtime
loads context and tool definitions, builds the provider request, sends it,
stores native output, and returns only the facts the deterministic engine
needs to branch. Endpoints, keys, and transport headers belong outside durable
session state and provider-neutral engine facts.

Define behavior for tool results, multimodal input, usage, prompt caching,
compaction, and failures. If a feature is unsupported, make that explicit
instead of accepting configuration that cannot work. Classify retries from
known transient conditions; a validation error is not made recoverable by
repeating it.

## Regenerate and verify the changed boundary

If public wire types or method metadata change, regenerate the API contract
and TypeScript consumers through the existing exporters. Do the same for a
workflow-contract change. Never hand-edit generated schemas, clients, or
Configurator tools. The [repository guidance](../../../AGENTS.md#generated-artifacts)
lists the required commands.

For a tool, test rejected arguments, missing grants, bounded output, retry
policy, and provider name mapping. For a model integration, use captured
request/response fixtures to check native payload preservation, tool-call
admission, endpoint selection, credential isolation, and failure classification.
Add engine replay coverage if deterministic behavior changes.

The existing tool-definition, provider-key, catalog-parity, materialization,
and endpoint-override tests provide focused starting points. Local fixtures
establish those boundaries before a deliberately configured live integration
exercise with the target service.

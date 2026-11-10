# P197 — Custom request headers for MCP servers

**Status:** Implemented on `mcp-server-headers`, 2026-11-10, uncommitted. Unit,
store, migration and gateway live tests pass. User documentation is not yet
written. Answers
[issue #138](https://github.com/smartcomputer-ai/lightspeed/issues/138).

## Goal

Let a configured MCP server receive extra HTTP request headers, each either
plain text or a saved credential. Self-hosted MCP gateways commonly expect
this: one header selects the upstream and another carries its token.

```
X-Upstream-Url: https://tracker.example.com
X-Upstream-Token: <secret>
```

Today a server can authenticate only through `Authorization`, with a Bearer
credential or OAuth, so such a gateway cannot be registered.

## Design

**The MCP server stores the header configuration. Credentials stores the
secret values.** Header names and plain values are part of the server
document; a secret header names a saved credential and never holds the value.

This is the shape bot HTTP poll triggers already use (`PollHttpAuth`), extended
to several headers. The issue's alternative, a credential type that itself
holds a set of header names and values, was not chosen: it would put
non-secret routing configuration inside a secret and add a second credential
shape with its own rotation and editing rules.

### Server document

`McpServerInput` and `McpServerView` gain `headers`, a map from header name to
a typed value. It defaults to empty, so existing servers are unchanged.

```json
{
  "execution": "runtime",
  "authPolicy": { "type": "none" },
  "headers": {
    "x-upstream-url": { "type": "literal", "value": "https://tracker.example.com" },
    "x-upstream-token": { "type": "credential", "grantId": "tracker-token" }
  }
}
```

- `literal` — sent as written. Not secret: reads return the value.
- `credential` — the credential's token, sent raw with no scheme prefix. Reads
  return the grant id only.

Any number of headers may be configured, in any mix, and each secret header
may name a different credential. There is no interpolation and no templating.

`authPolicy` and `credential` are unchanged and independent of `headers`. They
keep governing the `Authorization` header alone, so a Bearer or OAuth server
may also carry extra headers. No auth policy variant is added: whether a
server authenticates with a header is derived from the map.

### Validation at write

- Header names are valid HTTP field names and are stored lowercased, so two
  entries cannot differ only by case.
- Reserved names are rejected: `authorization`, `proxy-authorization`, `host`,
  `content-type`, `content-length`, `accept`, the hop-by-hop headers, the MCP
  transport headers (`mcp-session-id`, `mcp-protocol-version`,
  `last-event-id`), and every `x-lightspeed-*` name.
- Literal values are valid, non-empty header values within a size bound. The
  map has an entry-count bound.
- A credential entry's grant must exist in the same universe, be active, be a
  static token (not an OAuth grant), and, when it has an audience, cover the
  server URL. This is the check a Bearer binding gets today, applied per
  header. Brokered credentials suffice; retrieval by trusted services is not
  required.
- `execution: provider` with a non-empty `headers` map is rejected. Provider
  connectors carry one authorization token, so the configuration cannot be
  honoured there, and refusing the write is clearer than failing a run.

Restricting header credentials to static tokens keeps an OAuth access token
from being sent raw to an arbitrary endpoint and keeps refresh out of header
resolution.

### Storage

Headers live in `mcp_server_headers`, one row per header, holding either a
literal value or a grant id (a check enforces exactly one). Rows cascade with
their server. The grant column has a foreign key, so a credential in use by a
header cannot be deleted. Keeping grant ids out of a JSON column is what makes
that guarantee possible. The key is `NO ACTION DEFERRABLE INITIALLY IMMEDIATE`,
like the environment registration-key reference, so a universe delete (which
defers its constraints) removes grants and headers in either cascade order.

Server writes are transactional: the server row, then its header rows
replaced, then the whole record read back. Reads fold the header rows into
the server row with one `jsonb_agg` subquery.

Migration 15 adds the table; `REQUIRED_SCHEMA_REVISION` and
`LIGHTSPEED_SCHEMA_REVISION` are both 15.

### Resolution

Headers are resolved where the bearer is resolved today: in the runtime, from
the live server record, at each discovery or tool-call attempt. One function,
`resolve_mcp_request_headers`, produces the full header set; gateway
discovery, worker inventory and worker tool calls all use it, so they cannot
disagree. Each path keeps its existing bearer resolution.

A credential header resolves through the `SecretResolver` boundary under a
new `mcp_server_header` namespace whose id is `<server_id>/<header>`; neither
part can contain `/`. The broker-backed resolver re-reads the server, checks
that its URL is still the target and that it is usable, and leases the
header's grant for the server URL. The transport takes one `McpRequestAuth`
(bearer plus header map) in place of a bare bearer; credential header values
are marked sensitive and its `Debug` lists names only.
The set is applied to every request of the MCP exchange (initialize, list,
call, session and stream requests), alongside the built-in `Authorization`
when one is configured.

- A credential that is revoked or no longer covers the server URL fails the
  operation before anything is sent, with the same typed failure a Bearer
  credential produces.
- Retries and reconnections resolve again, so a rotated credential applies
  without editing the server.
- Resolution stays outside the deterministic harness. Sessions and profiles
  select a server by id and cannot override its headers.
- Resolved values are held as secrets and never enter persisted requests,
  events or diagnostics.
- Headers go only to the MCP endpoint. They are not sent to OAuth metadata or
  authorization-server requests. The MCP HTTP client does not follow
  redirects, so they cannot leak to another origin that way.

### Clients

- **Web:** the MCP server form gains a header editor: name, value source
  (plain text or credential) and either an editable value or a credential
  picker. Plain text is labelled as not secret. A credential row shows the
  credential's name. Servers with headers offer only "Lightspeed connects".
- **CLI:** `mcp server put` takes repeatable `--header NAME=VALUE` and
  `--header-credential NAME=GRANT_ID`; credential and login updates keep the
  existing headers.
- **Generated consumers:** regenerate the API contract, the TypeScript client
  and the Configurator MCP tools. Update the browser demo fixtures and stubs.

## Out of scope

- **`Authorization` as a custom header, and a `scheme` prefix.** Needed for
  servers expecting `Authorization: Token …`. Deferred together: allowing it
  brings a one-source rule against the built-in policies.
- **Provider-direct execution with headers.** OpenAI's MCP tool accepts
  headers and could be supported later; Anthropic's connector accepts a single
  authorization token.
- **Renaming the "Bearer token" credential label to "Token".** Cosmetic and
  separable.

## Tests

- Unit: name normalization and reserved names; grant kind, status and
  audience checks; provider execution with headers refused; a referenced
  credential cannot be deleted.
- Runtime: discovery and a tool call both deliver literal and credential
  headers to a test server; rotation applies on the next attempt; revocation
  fails before sending; resolved values are absent from persisted records and
  diagnostics; Bearer and OAuth servers behave as before, with and without
  extra headers.
- Web: the editor round-trips both value sources and never shows a secret.

## Documentation

When implemented, the user documentation for MCP servers and credentials needs
a section on custom headers, to be reviewed with the owner before it lands.

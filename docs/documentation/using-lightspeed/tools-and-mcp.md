# Tools and MCP

Tools let an agent do work outside a model response: read a file, fetch a web
page, call another agent, or act through an external service. A profile's
capability grants determine which tools Lightspeed exposes. Instructions can
explain how to use a tool, but they cannot grant one that is absent from the
setup.

Start with the smallest toolset that can complete the task. The release editor
needs VFS access to read its source and save a document. If it later needs to
check an issue tracker, connect that tracker through MCP and grant the relevant
tools. That keeps the agent's access understandable as its job grows.

Use a universe owner/admin or platform administrator account for the setup
procedures. Configure tools in a profile for reuse, or in an idle session's
**Session settings** for a local change.

## Choose built-in capabilities

The model-configuration editor groups capabilities by what the agent can do:

| Capability | Use it for |
| --- | --- |
| **Virtual File System: Files, Instructions, Skills** | Persistent workspace files and sourced instructions. See [Workspaces and skills](workspaces-and-skills.md). |
| **Web** | Fetching public pages and, with a supported API kind, searching the web. |
| **Sub-agents** | Delegating a task to an allowed profile. See [Sub-agents and federation](subagents-and-federation.md). |
| **Timers** | Waiting within agent work through durable timer operations. Use [bot schedules](bots-and-triggers.md) for recurring event production. |
| **Environments** | Working with execution environments and their processes. See [Environments](../environments/overview.md). |
| **MCP Servers** | Calling tools supplied by registered external MCP servers. |

Each feature has its own settings. A file-tool grant still needs workspace
links, and process access needs an environment. After changing the profile,
create a new session or [apply the setup](profiles-and-instructions.md#apply-changes-deliberately)
to an existing idle one.

## Add web access

Enable **Web**, then choose **Fetch pages**, **Search the web**, or both as
supported by the selected model route. Fetching a known URL and searching for
sources solve different parts of a research task.

Built-in search supports OpenAI Responses and Anthropic Messages. Chat
Completions supports page fetching but not this search feature. Responses
search uses the provider's cached search mode; the UI has no switch to turn
on live external access. Anthropic uses its native search and fetch tools,
while the other API kinds use Lightspeed's public-URL fetch for page content.
Do not treat a search result as proof that a page was fetched live.

Optional allowed or blocked domains restrict search results. These fields are
hidden under **Customize domains** by default; configured filters remain
summarized while collapsed. Anthropic accepts
one of those lists at a time. Search filters do not restrict **Fetch pages**
or MCP tools, which have separate access paths.

Verify the setup by asking the agent to fetch a specific public documentation
page and cite a fact from it. Inspect the tool activity and the cited page.
When freshness matters, check what the selected search mode actually returned.

## Register an MCP server

MCP connects a session to tools advertised by an external server. Registration
stores the server's URL and authentication in the universe; a profile then
references that server by ID. Registering it alone does not expose its tools
to every agent.

1. Open **Settings → MCP servers → Add server**.
2. Enter a **Name** and **Server URL**, then choose **Continue**. Use the
   server's Streamable HTTP endpoint. This field does not accept a local
   executable or a stdio command.
3. Under **Execution**, choose **Lightspeed connects** for the native path
   described here. The form defaults to **Model provider connects directly**;
   the differences are explained below.
4. Under **Tool exposure**, choose **Show tools to the model up front** for a
   small inventory, or **Let the model search on demand** for a larger one.
5. Configure authentication and finish adding the server.

For **No authentication**, the server must accept unauthenticated requests.
For **Bearer token**, first open **Settings → Secrets → Add secret**, set
**Secret type** to **Bearer token**, enter a **Display name** and **Secret
value**, and choose **Add secret**. Select that credential in the MCP form.

For **OAuth sign-in**, choose **Add and connect**, then **Open the sign-in**
in the OAuth dialog. Complete the external service's consent flow and return
until the connection shows **Connected**. The consent authorizes the MCP
connection; it does not grant the server to a profile yet.

## Select tools and grant the server

A newly registered server initially allows all of its advertised tools.
Before adding it to a session, edit the server, choose **Load tools**, then
**Allow only selected tools**. Select the operations needed for the job and
choose **Save**. If you changed the URL or credential first, save those
connection changes before loading tools.

For an issue-tracker integration, an initial review profile might need issue
search and issue read operations. Add a write operation when the task also
needs to update issues. Tool names and arguments come from the actual server;
there is no common issue-tracker tool name to paste into every configuration.

Loading tools discovers their metadata without invoking them. Read descriptions
and safety annotations as claims from that server. The allowlist and approval
policy are the controls you configure in Lightspeed.

Now open the profile, enable **MCP Servers**, choose **Add server**, and select
the registered **Server**. Save and start a session from that profile. Ask it
to perform a small read-only lookup against a known object, then inspect the
arguments and returned result in the transcript.

The profile references the server ID. The universe record owns its endpoint,
credential, execution path, exposure, tool selection, and approval policy.
Changes to that shared record can affect every profile using it; it is not
copied into each profile as an independent connection.

## Choose how MCP executes

| Choice | Where calls happen | What it requires |
| --- | --- | --- |
| **Lightspeed connects** | Lightspeed discovers and calls the server, returning results to the model. | Runtime network access to the endpoint. Supports Responses, Chat Completions, and Anthropic Messages. |
| **Model provider connects directly** | The model provider connects to the MCP server through its hosted MCP support. | A publicly reachable endpoint and a supporting provider/model. Chat Completions does not support this path. |

With native execution, showing tools up front places the selected definitions
in the model request. Search-on-demand exposes discovery and call tools so the
model can find relevant operations without loading the whole inventory. Native
injection is capped at 256 MCP tools per request; narrow the selection or use
search-on-demand for larger catalogs.

Provider-hosted Anthropic MCP additionally requires the operator to enable
`ANTHROPIC_BETA=mcp-client-2025-11-20`. Its approval restriction is described
below. An OpenAI-compatible endpoint is not guaranteed to implement hosted
MCP simply because it accepts another part of the Responses API.

For a private endpoint under native execution, enable **Advanced options →
Allow private-network egress** on the server and have the operator include
the destination in `LIGHTSPEED_MCP_PRIVATE_NETWORKS`. Both controls must
permit the connection.
Private-network permission for an OAuth sign-in is separate and does not
authorize tool-call egress. See the
[environment-variable reference](../reference/environment-variables.md) for operator settings.

## See images and documents from tools

Tools can show the model images and PDF documents, not only text. An MCP
server whose result carries `image` blocks or embedded PDF resources, and a
`read_file` (or **Read**) of a PNG, JPEG, GIF, WebP, or PDF file in a
workspace or environment, hand the bytes to the model as provider-native
media on every supported API: Anthropic Messages, OpenAI Responses, and
OpenAI Chat Completions. Nothing is converted or resized. Audio, video, and
other types are dropped with a note in the tool result, as is any asset over
10 MB or beyond eight per result, so a wrong tool call or an accidental read
of a binary file never fails the run. A text-only model such as DeepSeek
receives a note in place of the media instead of an error.

Every media item is named by a **handle**: `media:` followed by the first
twelve characters of its content hash, for example `media:3f9a2c1d4e7b`. The
tool result announces each item by position and handle
(`[image 1 · media:3f9a2c1d4e7b · image/png · 38 KiB]`), and the model
refers to media the same way, writing `![caption](media:3f9a2c1d4e7b)` or
`[report](media:9b21e04c77a1)` in its answer. The transcript resolves those
links against the session's media and renders the image or a document link
in place; a handle the session never saw shows as a broken reference. A
sub-agent hands an image or document up to its parent by linking it in its
final answer the same way.

One provider caveat: Claude Opus 5's refusal classifier rejects some
tool-produced media follow-ups, reliably a PDF that arrives after a tool
round-trip, and a refusal fails the turn. Other Claude models and both OpenAI
APIs read the same content; choose one of them for sessions whose tools
return documents.

## Require approval for tool calls

The server's **Advanced options → Tool approval** defaults to **Never require
approval**. Choose **Always require approval** to pause proposed calls for a
decision. The transcript shows each pending operation and its arguments;
choose **Approve** to allow it or **Reject** to refuse it. A batch continues
after all pending decisions are supplied.

Approval applies to MCP calls on that server. It does not create a universal
approval gate for every other feature in the profile. Configure those grants
according to the operations the agent should be able to perform.

Native MCP supports approval with all three API kinds. Provider-hosted
Responses supports it too. Provider-hosted Anthropic MCP rejects **Always
require approval**; choose native execution when that combination needs
approval.

Verify the policy using a harmless read operation. Confirm that the run waits
for a decision, then approve it and inspect the result. An approved call can
still fail at the server, and cancellation does not roll back a call that has
already completed.

## If a tool is missing or fails

| Symptom | What to check |
| --- | --- |
| A registered MCP server supplies no tools to the agent | Enable it under the profile or session's **MCP Servers** feature, then apply that setup. |
| Only discovery and call tools appear | The server may use search-on-demand. Ask for the task so the agent can discover the relevant operation. |
| **Load tools** is unavailable after editing | Save the connection changes first. |
| Calls cannot reach a private endpoint | Check native execution, runtime network access, server egress permission, and the deployment allowlist. |
| A run is waiting without another model response | Look for pending approvals and decide every call in the batch. |
| A provider rejects MCP or web configuration | Check its API kind and execution mode against the compatibility rules above. |
| The server advertises a tool but calls are refused | Check the tool allowlist, credential scopes, and remote service's own permissions. |

This guide connects external tools to Lightspeed agents. To let another MCP
client manage Lightspeed itself, use
[Configurator MCP](../../../platform/configurator-mcp/README.md).

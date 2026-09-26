# Tools and MCP

Tools let an agent read files, fetch web pages, delegate tasks, and act through
external services. The session's configuration determines which tools it can
use. Save that configuration in a profile when several sessions need the
same setup.

Start with the smallest toolset that can complete the task. The release editor
needs VFS access to read its source and save a document. If it later needs to
check an issue tracker, connect that tracker through MCP and grant the relevant
tools. That keeps the agent's access understandable as its job grows.

Use an Operator or Admin account to register MCP servers and configure
profiles. A Contributor can change tools in an idle session they can control
under **Session settings**. See
[Agent and tool access](../access-and-security/agent-and-tool-access.md) for
how tool configuration and external credentials determine what an agent can do.

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

Each feature has its own settings. VFS tools come from attached workspaces,
and process access needs an environment attached with `exec` or higher access.
After changing the profile,
create a new session or [apply the setup](profiles-and-instructions.md#apply-changes-deliberately)
to an existing idle one.

## Built-in tool formats

Tool names vary with the model's API kind. These are the names you may see in
the transcript for the same kinds of work:

| API kind | Filesystem editing | Process execution |
| --- | --- | --- |
| Chat Completions | `write_file`, exact-replacement `edit_file` | `exec_command`, `write_stdin` |
| OpenAI Responses | `write_file`, `edit_file`, `apply_patch` | `exec_command`, `write_stdin` |
| Anthropic Messages | `Write`, `Edit` | `Bash`, `BashOutput`, `KillShell` |

Read, directory-listing, glob, and grep tools accompany these editing tools.
Attached VFS workspaces have their own prefixed filesystem tools; they are
separate from environment files. Each operation still requires its capability
grant.

Long-running commands can return output while the process continues. The
agent uses the returned handle to collect more output or send input. See
[Processes and jobs](../environments/processes-and-jobs.md) to choose between
an interactive command and a background job, and to stop either safely.

## Add web access

Enable **Web**, then choose **Fetch pages**, **Search the web**, or both as
supported by the selected model route. Fetching a known URL and searching for
sources solve different parts of a research task.

Built-in search supports OpenAI Responses and Anthropic Messages. Chat
Completions supports page fetching but not this search feature. Responses
search uses the provider's cached search mode; the UI has no switch to turn
on live external access. Anthropic uses its native search and fetch tools,
while the other API kinds use Lightspeed's public-URL fetch for page content.

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

1. Open **MCP servers → Add server**.
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
For **Bearer token**, first open **Credentials → Add credential → Paste a
token or secret**. Set **Secret type** to **Bearer token**, enter a **Display
name** and **Secret value**, and choose **Add credential**. Select that
credential in the MCP form.

For **OAuth sign-in**, choose **Add and connect**, then **Open the sign-in**.
Complete the external service's consent flow and return until the connection
shows **Connected**. The next step selects which profiles can use it.

## Select tools and grant the server

A newly registered server initially allows all of its advertised tools.
To restrict that connection for every profile and session, edit the server
and choose **Selected tools**. Tool selection is always visible here, with
the live inventory loaded when the editor opens. Search by name or
description, select the allowed operations, and choose **Save**. If you changed
the URL, credential, or network access first, save those connection changes
before loading tools.

For an issue-tracker integration, an initial review profile might need issue
search and issue read operations. Add a write operation when the task also
needs to update issues. Use the names advertised by your server.

Loading tools discovers their metadata without invoking them. Read descriptions
and safety annotations as claims from that server. The allowlist and approval
policy are the controls you configure in Lightspeed.

Now open the profile and enable **MCP Servers**. It starts with no server
attachments and grants no MCP tools until you choose **Add server** and select
the registered **Server**. Remove all server attachments before disabling the
feature. Each attachment defaults to **All server-allowed tools**.
To narrow them for this profile, choose **Customize tools**, then **Selected
tools**. The same picker shows only tools within the server's allowance, plus
any saved selections that are no longer allowed so you can remove them.

The all-tools mode includes future tools within the server's allowance.
**Selected tools** keeps the names you chose, even if that includes every
current tool. Refreshing the inventory leaves your selection unchanged;
remove unavailable tools or update the selection before saving.

Save and start a session from that profile. Ask it to perform a small read-only
lookup against a known object, then inspect the arguments and returned result
in the transcript.

The registered server is shared configuration: changing its endpoint,
credential, tools, or approval policy can affect every profile using it.
Profiles reference that connection and may narrow its tool selection.

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

Reading a PNG, JPEG, GIF, WebP, or PDF can return the file to the model as
media. MCP servers can also supply image blocks and embedded PDFs. The
transcript shows those items as thumbnails or document links, and the agent
can reference them in its answer. Sub-agents can pass the same media back in
their results.

Lightspeed accepts up to eight media items per tool result, each at most
10 MiB. Unsupported types and oversized items produce an explanatory note.
The bytes are passed through without resizing or conversion, so the selected
model must support the format. A text-only model receives a note instead;
provider-specific limits or refusals can still reject a model request. Test a
representative document before choosing a model for a document-heavy task.
Claude Opus 5 has refused some tool-produced PDF follow-ups in Lightspeed's
live tests; verify that combination with the documents your task will use.

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
| The server advertises a tool but calls are refused | Check the record's tool allowlist, the profile's `tools` subset for that server, credential scopes, and the remote service's own permissions. |

This guide connects external tools to Lightspeed agents. To let another MCP
client manage Lightspeed itself, use
[Configurator MCP](../integrating-and-extending/configurator-mcp.md).

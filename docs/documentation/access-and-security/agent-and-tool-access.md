# Agent and tool access

The Platform checks whether someone can start or control a run. Once the run
starts, the agent uses the tools and resources configured for its session.
Credentials used by those tools can reach systems outside Lightspeed.

## Give a session tools and resources

A profile selects the tools an agent can use and attaches the resources those
tools can reach. Instructions tell the agent what to do within that setup.

For example, Acorn's release editor can attach a workspace with edit access.
A reviewer can attach the same workspace with read access. The workspace
tools enforce those permissions. An environment attachment similarly controls
whether the session can read files, edit them, or run commands. For MCP,
the profile selects from the tools the registered server allows. See
[Profiles and instructions](../using-lightspeed/profiles-and-instructions.md)
and [Tools and MCP](../using-lightspeed/tools-and-mcp.md).

A delegated sub-agent starts with the selected child profile and the brief
the parent sends. Include the allowed child profiles when reviewing what a
parent can accomplish. For example, a reviewer can cause database writes by
delegating to a child configured to perform them. See [Sub-agents and
federation](../using-lightspeed/subagents-and-federation.md).

## Bound the runtime's session control

The runtime gives each bot and delegated session authority over related work.
A bot controller manages its own bot and sessions, including delegated
descendants. A delegated session can control itself and its direct children.
Both can read work in their own session tree and shared conversations in the
same universe. The session's configured tools determine what the agent can
actually use.

A bot that creates a webhook receives the ingest URL once in Main. Anyone
who can read that session can see the tool result. See
[Bots and triggers](../using-lightspeed/bots-and-triggers.md).

## Follow credentials across a tool call

An external MCP server authorizes a call using the credential configured for
that connection. For example, the built-in [Configurator
setup](../integrating-and-extending/configurator-mcp.md) uses an organization
service user with an Operator grant and an API key limited to configuration
operations in one universe. Keep full-authority runtime tokens with trusted
infrastructure.

Brokered credentials stay with the runtime for its own requests. A
retrievable credential or model provider API key can be bound into an
environment. Assume a process that receives a credential can retain it.
Other sessions using that machine share its filesystem and can reach files
their environment permissions allow. See [Environment
credentials](../environments/credentials.md).

File tools run within the daemon's filesystem root and operating-system
permissions. A read-only attachment can still expose host credentials that
are stored in readable files. File tools protect daemon state, the
registration-key file, and Linux kernel filesystems such as `/proc` and
`/sys`. Commands have the access of the operating-system account running
them. See [Choose the access
level](../environments/using-environments.md#choose-the-access-level).

## Approve and stop work explicitly

An MCP server can require approval before its calls proceed. A person
approving a call through the Platform needs permission to control the session.
See [Require approval for tool
calls](../using-lightspeed/tools-and-mcp.md#require-approval-for-tool-calls).

Removing someone from an organization blocks their subsequent requests. To
stop existing work, cancel the run or close the session, then check any
external jobs or processes. Completed tool effects and copies of delivered
credentials may persist.

## Display agent output

The web app renders Markdown from agents, tools, and files. It can display
images recorded with the session or referenced from its files. External
image addresses appear as links and are fetched only when someone follows
them. Raw HTML is displayed as text.

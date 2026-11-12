from typing import assert_type

from lightspeed_ai import AsyncLightspeedClient
from lightspeed_ai.types import (
    AgentApiOutcomeOfAuthGrantPutResponse,
    AgentApiOutcomeOfMcpServerPutResponse,
    AgentApiOutcomeOfRunReadResponse,
    SessionEventKindViewRunFailed,
    SessionEventView,
    SessionStartResponse,
)


async def use_client(client: AsyncLightspeedClient) -> None:
    assert_type(await client.start_session(display_name="SDK", delete_after_close_ms=None), SessionStartResponse)
    assert_type(await client.call("session/runs/read", {"sessionId": "s", "runId": "r"}), AgentApiOutcomeOfRunReadResponse)


async def configure_mcp_server(client: AsyncLightspeedClient) -> None:
    assert_type(
        await client.call("mcp/servers/put", {
            "server": {
                "serverId": "example",
                "serverUrl": "https://mcp.example.test/mcp",
                "defaultServerLabel": "Example",
                "headers": {
                    "X-Workspace": {"type": "literal", "value": "demo"},
                    "Authorization": {"type": "credential", "grantId": "grant"},
                },
            },
        }),
        AgentApiOutcomeOfMcpServerPutResponse,
    )
    assert_type(
        await client.call("auth/grants/put", {"grantId": "grant", "token": "fixture-token"}),
        AgentApiOutcomeOfAuthGrantPutResponse,
    )


def failure_message(event: SessionEventView) -> str | None:
    kind = event["kind"]
    if kind["type"] == "runFailed":
        assert_type(kind, SessionEventKindViewRunFailed)
        return kind["message"]
    return None

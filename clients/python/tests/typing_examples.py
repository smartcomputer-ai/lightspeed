from typing import assert_type

from lightspeed_ai import AsyncLightspeedClient
from lightspeed_ai.types import AgentApiOutcomeOfRunReadResponse, SessionStartResponse


async def use_client(client: AsyncLightspeedClient) -> None:
    assert_type(await client.start_session(display_name="SDK", delete_after_close_ms=None), SessionStartResponse)
    assert_type(await client.call("session/runs/read", {"sessionId": "s", "runId": "r"}), AgentApiOutcomeOfRunReadResponse)

"""Run explicitly with LIGHTSPEED_TEST_GATEWAY=1 and a configured local gateway."""

import asyncio
import json
import os
from pathlib import Path
from uuid import uuid4

import pytest

from lightspeed import AsyncLightspeedClient

pytestmark = pytest.mark.skipif(
    os.getenv("LIGHTSPEED_TEST_GATEWAY") != "1", reason="opt-in test requires a configured local gateway",
)


async def test_typescript_submission_can_resume_in_python():
    suffix = uuid4().hex
    session_id, submission_id = f"sdk-parity-{suffix}", f"submission-{suffix}"
    prompt = "Reply with the word ready."
    process = await asyncio.create_subprocess_exec(
        "node", str(Path(__file__).with_name("typescript_start.mjs")), session_id, submission_id, prompt,
        stdout=asyncio.subprocess.PIPE,
    )
    stdout, _ = await process.communicate()
    assert process.returncode == 0, "Build the TypeScript SDK and check gateway credentials"
    started = json.loads(stdout)
    connection = os.environ.get("LIGHTSPEED_TEST_MODEL_CONNECTION")
    client = (
        await AsyncLightspeedClient.from_connection(connection, config_dir=os.environ["LIGHTSPEED_TEST_CONNECTION_CONFIG"])
        if connection else AsyncLightspeedClient(
            os.environ["LIGHTSPEED_API_URL"], token=os.environ.get("LIGHTSPEED_RUNTIME_TOKEN"),
            universe=os.environ.get("LIGHTSPEED_UNIVERSE"),
        )
    )
    async with client:
        repeated = await client.start_run(session_id, prompt, submission_id=submission_id)
        assert repeated["run"]["id"] == started["runId"]
        async with asyncio.timeout(120):
            result = await client.await_run(session_id, started["runId"], after=started["cursor"])
        assert result["run"]["id"] == started["runId"]
        assert result["run"]["status"] == "completed", result["run"].get("failure")
        assert result["run"].get("outputText")

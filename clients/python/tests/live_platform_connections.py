"""Explicit live suite against real Platform, runtime and saved CLI credentials.

Run after building the TypeScript SDK:
  LIGHTSPEED_TEST_CONNECTION_CONFIG=/private/fixture \
    uv run pytest tests/live_platform_connections.py -v

The fixture must contain `sdk-session` and `sdk-key` CLI connections for the
same human/organization/universe. The personal key must be read-only and restricted
to that universe. `other-universe.txt` names another universe the human owns.
The fixture script's renewal-database option must age only the dedicated CLI
session and write `renewal-required`; the first SDK request verifies renewal.
Both dedicated credentials are revoked by this suite. Missing prerequisites
fail rather than skipping. This file is not collected by the ordinary suite.
"""

import asyncio
from contextlib import suppress
from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
from uuid import uuid4

import pytest
import httpx

from lightspeed import AsyncLightspeedClient, LightspeedApiError


async def typescript_check(directory: Path, name: str, mode: str, blob_ref: str = "") -> dict:
    process = await asyncio.create_subprocess_exec(
        "node", str(Path(__file__).with_name("typescript_connections.mjs")),
        str(directory), name, mode, blob_ref,
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
    )
    try:
        stdout, stderr = await asyncio.wait_for(process.communicate(), 60)
    except TimeoutError:
        process.kill()
        await process.communicate()
        raise
    assert process.returncode == 0, stderr.decode()
    return json.loads(stdout)


async def test_saved_cli_credentials_work_across_sdks_and_revocation_is_enforced():
    directory = Path(os.environ["LIGHTSPEED_TEST_CONNECTION_CONFIG"])
    other_universe = (directory / "other-universe.txt").read_text().strip()
    assert other_universe, "fixture requires another universe owned by the human"
    assert (directory / "renewal-required").is_file(), "prepare fixture with LIGHTSPEED_PLATFORM_LIVE_RENEWAL_DATABASE_URL"
    renewed_tokens = []

    async def observe_renewal(response):
        if token := response.headers.get("set-auth-token"):
            renewed_tokens.append(token)

    http = httpx.AsyncClient(event_hooks={"response": [observe_renewal]}, timeout=30)
    try:
        session = await AsyncLightspeedClient.from_connection("sdk-session", config_dir=directory, http_client=http)
    except BaseException:
        await http.aclose()
        raise
    key = None
    key_id = None
    session_revoked = False
    key_revoked = False
    try:
        assert renewed_tokens, "aged CLI session must renew on the first SDK request"
        saved = json.loads((directory / "connections.json").read_text())
        credential_file = Path(saved["connections"]["sdk-session"]["credentialFile"])
        if credential_file.read_text().strip() != renewed_tokens[-1]:
            pytest.fail("SDK did not persist the renewed CLI credential")
        session_view = await session.platform.read_current_session()
        assert datetime.fromisoformat(session_view["session"]["expiresAt"]) > datetime.now(timezone.utc) + timedelta(days=1)
        key = await AsyncLightspeedClient.from_connection("sdk-key", config_dir=directory)
        session_info = await session.initialize()
        key_info = await key.initialize()
        assert session_info["runtime"] == key_info["runtime"] == "platform"
        assert session_info["platform"]["credential"]["kind"] == "cliSession"
        assert key_info["platform"]["credential"]["kind"] == "personalKey"
        assert session_info["platform"]["principal"]["id"] == key_info["platform"]["principal"]["id"]
        key_id = key_info["platform"]["credential"]["id"]
        assert key_info["platform"]["restrictions"]["readOnly"] is True
        assert other_universe not in key_info["platform"]["restrictions"]["universeIds"]

        # Exceed the inline-blob threshold so this exercises object storage and
        # multiple streamed chunks through the Platform proxy as well.
        binary = bytes(range(256)) * 4096
        blob_ref = f"sha256:{hashlib.sha256(binary).hexdigest()}"
        assert (await session.upload_blob(blob_ref, binary))["blobRef"] == blob_ref
        for name, client in [("sdk-session", session), ("sdk-key", key)]:
            organizations = await client.platform.list_organizations()
            assert client.organization_id in {organization["id"] for organization in organizations["organizations"]}
            assert "profiles" in await client.list_profiles()
            async with client.open_download("blobs/read", {"blobRef": blob_ref}) as response:
                assert await response.aread() == binary
            stream = client.watch(reconnect=False)
            try:
                async with asyncio.timeout(15):
                    assert (await anext(stream))["method"] == "watch/reset"
            finally:
                await stream.aclose()
            observed = await typescript_check(directory, name, "allowed", blob_ref)
            assert observed["principalId"] == session_info["platform"]["principal"]["id"]
            assert observed["organizationId"] == client.organization_id

        # The human can use the second universe, while its restricted key cannot.
        async with await AsyncLightspeedClient.from_connection(
            "sdk-session", config_dir=directory, universe=other_universe,
        ) as other_session:
            assert "profiles" in await other_session.list_profiles()
            # The principal owns both universes, so this refusal must come
            # from universe-bound storage rather than missing membership.
            with pytest.raises(LightspeedApiError) as missing:
                async with other_session.open_download("blobs/read", {"blobRef": blob_ref}):
                    pytest.fail("a blob from another universe was readable")
            assert missing.value.status == 404
        async with await AsyncLightspeedClient.from_connection(
            "sdk-key", config_dir=directory, universe=other_universe,
        ) as other_key:
            with pytest.raises(LightspeedApiError) as denied:
                await other_key.list_profiles()
            assert denied.value.status == 404  # Platform conceals inaccessible universes.
        with pytest.raises(LightspeedApiError) as denied:
            await key.call("profiles/delete", {"profileId": f"absent-{uuid4().hex}"})
        assert denied.value.status == 403
        with pytest.raises(LightspeedApiError) as denied:
            await key.platform.list_personal_api_keys()
        assert denied.value.status == 403
        with pytest.raises(LightspeedApiError) as denied:
            await key.upload_blob(blob_ref, binary)
        assert denied.value.status == 403

        await session.platform.revoke_personal_api_key({"keyId": key_id})
        key_revoked = True
        with pytest.raises(LightspeedApiError) as revoked:
            await key.list_profiles()
        assert revoked.value.status == 401
        assert await typescript_check(directory, "sdk-key", "revoked") == {"revoked": True}
        with pytest.raises(LightspeedApiError) as revoked:
            await AsyncLightspeedClient.from_connection("sdk-key", config_dir=directory)
        assert revoked.value.status == 401

        await session.platform.revoke_current_session()
        session_revoked = True
        with pytest.raises(LightspeedApiError) as revoked:
            await session.platform.list_organizations()
        assert revoked.value.status == 401
        assert await typescript_check(directory, "sdk-session", "revoked") == {"revoked": True}
    finally:
        if not session_revoked:
            if key_id is not None and not key_revoked:
                with suppress(LightspeedApiError):
                    await session.platform.revoke_personal_api_key({"keyId": key_id})
            with suppress(LightspeedApiError):
                await session.platform.revoke_current_session()
        if key is not None:
            await key.aclose()
        await session.aclose()
        await http.aclose()

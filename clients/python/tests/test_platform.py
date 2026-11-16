import json

import httpx
import pytest

from lightspeed_ai import AsyncLightspeedClient, LightspeedApiError


async def test_native_platform_uses_shared_credentials_and_preserves_path_prefix():
    async def handle(request):
        assert request.method == "POST"
        assert request.url.raw_path == b"/lightspeed/api/v1/platform/accounts/account%2Fone/universes"
        assert request.headers["authorization"] == "Bearer personal-key"
        assert json.loads(request.content) == {"name": "Sandbox", "protected": False}
        return httpx.Response(200, json={"universe": {"id": "u"}})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with AsyncLightspeedClient("https://host/lightspeed/api/v1", token="personal-key", http_client=http) as client:
            assert await client.call_platform("platform/universes/create", {"accountId": "account/one", "name": "Sandbox", "protected": False}) == {"universe": {"id": "u"}}


async def test_native_platform_refuses_path_traversal_and_preserves_denial():
    calls = []
    async def handle(request):
        calls.append(request)
        return httpx.Response(403, json={"type": "about:blank", "title": "Forbidden", "status": 403, "kind": "forbidden", "detail": "Outside key scope"})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with AsyncLightspeedClient("https://host/api/v1", http_client=http) as client:
            with pytest.raises(ValueError, match="Invalid path"):
                await client.call_platform("platform/accounts/read", {"accountId": ".."})
            assert calls == []
            with pytest.raises(LightspeedApiError) as caught:
                await client.call_platform("platform/accounts/read", {"accountId": "account"})
            assert caught.value.kind == "forbidden"

import json

import httpx
import pytest

from lightspeed_ai import AsyncLightspeedClient, LightspeedApiError


async def test_native_platform_uses_shared_credentials_and_preserves_path_prefix():
    async def handle(request):
        assert request.method == "POST"
        assert request.url.raw_path == b"/lightspeed/api/v1/platform/organizations/organization%2Fone/universes"
        assert request.headers["authorization"] == "Bearer personal-key"
        assert json.loads(request.content) == {"name": "Sandbox", "protected": False}
        return httpx.Response(200, json={"universe": {"id": "u"}})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with AsyncLightspeedClient("https://host/lightspeed/api/v1", token="personal-key", http_client=http) as client:
            assert await client.call("platform/universes/create", {"organizationId": "organization/one", "name": "Sandbox", "protected": False}) == {"universe": {"id": "u"}}


async def test_native_platform_refuses_path_traversal_and_preserves_denial():
    calls = []
    async def handle(request):
        calls.append(request)
        return httpx.Response(403, json={"type": "about:blank", "title": "Forbidden", "status": 403, "kind": "forbidden", "detail": "Outside key scope"})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with AsyncLightspeedClient("https://host/api/v1", http_client=http) as client:
            with pytest.raises(ValueError, match="Invalid path"):
                await client.call("platform/organizations/read", {"organizationId": ".."})
            assert calls == []
            with pytest.raises(LightspeedApiError) as caught:
                await client.call("platform/organizations/read", {"organizationId": "organization"})
            assert caught.value.kind == "forbidden"


async def test_native_helpers_and_unified_call_share_the_same_transport():
    calls = []
    async def handle(request):
        calls.append((request.method, request.url.path, json.loads(request.content) if request.content else None))
        if request.url.path.endswith("/organizations"):
            return httpx.Response(200, json={"organizations": [], "canCreate": True})
        return httpx.Response(200, json={"organization": {"id": "organization"}})
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with AsyncLightspeedClient("https://host/deployment/", http_client=http) as client:
            assert await client.platform.list_organizations() == await client.call("platform/organizations/list")
            await client.platform.read_organization({"organizationId": "organization"})
            assert not hasattr(client, "call_platform")
    assert calls == [("GET", "/deployment/api/v1/platform/organizations", None), ("GET", "/deployment/api/v1/platform/organizations", None), ("GET", "/deployment/api/v1/platform/organizations/organization", None)]

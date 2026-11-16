"""Saved CLI credentials remain pinned across requests, renewal and logout."""
import json
import os
from pathlib import Path

import httpx
import pytest

from lightspeed_ai import AsyncLightspeedClient, LightspeedApiError, LightspeedTransportError
from lightspeed_ai._endpoint import normalize_endpoint
from lightspeed_ai import _connection

ENDPOINTS = json.loads((Path(__file__).resolve().parents[2] / "fixtures/endpoints.json").read_text())


@pytest.mark.parametrize("case", ENDPOINTS["accepted"])
def test_shared_endpoint_normalization(case):
    assert normalize_endpoint(case["input"]) == case["normalized"]


@pytest.mark.parametrize("endpoint", ENDPOINTS["rejected"])
def test_shared_invalid_endpoints(endpoint):
    with pytest.raises(ValueError):
        normalize_endpoint(endpoint)


def saved(tmp_path, *, kind="platformSession", owned=True, account="account-a", universe="u", single=False):
    directory = tmp_path / "config"
    credentials = directory / "credentials"
    credentials.mkdir(parents=True)
    path = credentials / "session"
    path.write_text("token-one")
    path.chmod(0o600)
    entry = {"endpoint": "https://platform.test/team/api/v1", "single": single,
             "credentialKind": "none" if single else kind, "serverKind": "runtime" if single or kind == "runtimeToken" else "platform",
             "credentialFile": str(path) if not single else None, "ownedCredential": owned,
             "accountId": None if single or kind == "runtimeToken" else account, "universe": universe}
    config = {"selected": "work", "connections": {"work": entry}}
    (directory / "connections.json").write_text(json.dumps(config))
    return directory, path, config


def discovery(kind="cliSession", *, default="suggested", single=False):
    return {"runtime": "runtime" if single or kind == "runtimeToken" else "platform", "contractRevision": "1",
            "caller": {"single": single}, "defaultUniverse": default,
            "platform": {"principal": {"kind": "application" if kind == "applicationKey" else "user", "id": "principal", "name": "Principal", "platformAdmin": False},
                         "credential": {"kind": kind, "id": "credential"}}}


async def test_import_pins_endpoint_and_credentials_ignores_ambient_overrides(tmp_path, monkeypatch):
    directory, path, config = saved(tmp_path)
    monkeypatch.setenv("LIGHTSPEED_API_URL", "https://other.test")
    monkeypatch.setenv("LIGHTSPEED_API_TOKEN", "wrong-token")
    requests = []
    async def handle(request):
        requests.append(request)
        assert request.url.host == "platform.test"
        assert request.headers["authorization"] == "Bearer token-one"
        return httpx.Response(200, json=discovery() if request.url.path.endswith("connection") else {"accounts": []})
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http) as client:
            assert client.account_id == "account-a"
            await client.platform.list_accounts()
            config["connections"]["work"]["endpoint"] = "https://other.test/api/v1"
            (directory / "connections.json").write_text(json.dumps(config))
            with pytest.raises(ValueError, match="changed"):
                await client.platform.list_accounts()
    assert len(requests) == 2


@pytest.mark.parametrize("explicit, expected", [(None, None), ("explicit", "explicit")])
async def test_account_override_clears_inherited_universe_and_suppresses_default(tmp_path, explicit, expected):
    directory, _, _ = saved(tmp_path)
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(200, json=discovery()))) as http:
        async with await AsyncLightspeedClient.from_connection("work", config_dir=directory, account="account-b", universe=explicit, http_client=http) as client:
            assert client.account_id == "account-b"
            assert client._universe == expected


async def test_single_runtime_import_uses_discovered_universe_without_authentication(tmp_path):
    directory, _, _ = saved(tmp_path, single=True, universe=None)
    async def handle(request):
        assert "authorization" not in request.headers
        return httpx.Response(200, json=discovery(single=True))
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http) as client:
            assert client._universe == "suggested"
            assert client.account_id is None


async def test_owned_session_renewal_on_discovery_and_denial_is_reread(tmp_path):
    directory, path, _ = saved(tmp_path)
    tokens = []
    async def handle(request):
        tokens.append(request.headers["authorization"])
        if request.url.path.endswith("connection"):
            return httpx.Response(200, json=discovery(), headers={"set-auth-token": "token-two"})
        if len(tokens) == 2:
            return httpx.Response(403, json={"type": "about:blank", "title": "Forbidden", "status": 403, "kind": "forbidden", "detail": "denied"}, headers={"set-auth-token": "token-three"})
        return httpx.Response(200, json={"accounts": []})
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http) as client:
            with pytest.raises(LightspeedApiError):
                await client.platform.list_accounts()
            await client.platform.list_accounts()
    assert tokens == ["Bearer token-one", "Bearer token-two", "Bearer token-three"]
    assert path.read_text() == "token-three"
    if os.name == "posix":
        assert path.stat().st_mode & 0o777 == 0o600


@pytest.mark.parametrize("kind, owned, auth_override", [("personalKey", True, None), ("cliSession", False, None), ("cliSession", True, "Bearer override-key")])
async def test_renewal_never_changes_api_keys_external_credentials_or_overridden_auth(tmp_path, kind, owned, auth_override):
    directory, path, _ = saved(tmp_path, kind="platformApiKey" if kind == "personalKey" else "platformSession", owned=owned)
    async def handle(request):
        return httpx.Response(200, json=discovery(kind) if request.url.path.endswith("connection") else {"accounts": []},
                              headers={} if request.url.path.endswith("connection") else {"set-auth-token": "unexpected-token"})
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http) as client:
            await client.platform.list_accounts(headers={"authorization": auth_override} if auth_override else None)
    assert path.read_text() == "token-one"


async def test_same_token_renewal_does_not_replace_the_file(tmp_path):
    directory, path, _ = saved(tmp_path)
    before = path.stat()
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(200, json=discovery(), headers={"set-auth-token": "token-one"}))) as http:
        async with await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http):
            pass
    assert path.stat().st_ino == before.st_ino
    assert path.stat().st_mtime_ns == before.st_mtime_ns


async def test_wrong_discovered_credential_is_rejected_without_persisting_rotation(tmp_path):
    directory, path, _ = saved(tmp_path)
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(200, json=discovery("personalKey"), headers={"set-auth-token": "wrong-session"}))) as http:
        with pytest.raises(ValueError, match="credential kind"):
            await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http)
    assert path.read_text() == "token-one"


@pytest.mark.parametrize("transfer", ["binary", "watch"])
async def test_stream_transports_process_session_renewal(tmp_path, transfer):
    directory, path, _ = saved(tmp_path)
    async def handle(request):
        if request.url.path.endswith("connection"):
            return httpx.Response(200, json=discovery())
        if transfer == "binary":
            return httpx.Response(200, content=b"raw", headers={"content-type": "application/octet-stream", "set-auth-token": "token-two"})
        return httpx.Response(200, text='event: watch/end\ndata: {}\n\n', headers={"content-type": "text/event-stream", "set-auth-token": "token-two"})
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http) as client:
            if transfer == "binary":
                async with client.open_download("blobs/read", {"blobRef": "sha256:data"}) as response:
                    assert await response.aread() == b"raw"
            else:
                assert [event async for event in client.watch(reconnect=False)] == []
    assert path.read_text() == "token-two"


@pytest.mark.parametrize("target", ["runtime", "platform", "binary", "watch"])
async def test_custom_http_client_redirect_defaults_cannot_forward_credentials(target):
    requests = []
    async def handle(request):
        requests.append(request)
        return httpx.Response(307, headers={"location": "https://other.test/steal"})
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle), follow_redirects=True) as http:
        async with AsyncLightspeedClient("https://platform.test", token="secret", universe="u", http_client=http) as client:
            with pytest.raises(LightspeedTransportError, match="redirect"):
                if target == "runtime":
                    await client.list_profiles()
                elif target == "platform":
                    await client.platform.list_accounts()
                elif target == "binary":
                    async with client.open_download("blobs/read", {"blobRef": "sha256:data"}):
                        pass
                else:
                    async for _ in client.watch(reconnect=False):
                        pass
    assert len(requests) == 1
    assert requests[0].url.host == "platform.test"


async def test_logout_during_atomic_renewal_removes_only_new_orphan(tmp_path, monkeypatch):
    directory, path, config = saved(tmp_path)
    replace = os.replace
    def logout_then_replace(source, destination):
        config["connections"].clear()
        (directory / "connections.json").write_text(json.dumps(config))
        path.unlink()
        replace(source, destination)
    async def handle(request):
        return httpx.Response(200, json=discovery() if request.url.path.endswith("connection") else {"accounts": []},
                              headers={} if request.url.path.endswith("connection") else {"set-auth-token": "token-two"})
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http) as client:
            monkeypatch.setattr(_connection.os, "replace", logout_then_replace)
            await client.platform.list_accounts()
            assert not path.exists()
            with pytest.raises(ValueError, match="changed"):
                await client.platform.list_accounts()


@pytest.mark.parametrize("unsafe", ["symlink", "permissions", "outside"])
async def test_unsafe_saved_credentials_fail_before_network(tmp_path, unsafe):
    directory, path, config = saved(tmp_path)
    if unsafe == "symlink":
        original = path.with_name("original")
        path.rename(original)
        path.symlink_to(original)
    elif unsafe == "permissions":
        if os.name != "posix":
            pytest.skip("POSIX permission check")
        path.chmod(0o644)
    else:
        outside = tmp_path / "outside"
        path.rename(outside)
        config["connections"]["work"]["credentialFile"] = str(outside)
        (directory / "connections.json").write_text(json.dumps(config))
    def never_send(_):
        raise AssertionError("unsafe credential reached the network")
    async with httpx.AsyncClient(transport=httpx.MockTransport(never_send)) as http:
        with pytest.raises(ValueError):
            await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http)


@pytest.mark.parametrize("field,value", [("credentialKind", "platformApiKey"), ("ownedCredential", False), ("allowInsecureHttp", True), ("serverKind", "runtime")])
async def test_saved_security_binding_changes_invalidate_existing_client(tmp_path, field, value):
    directory, _, config = saved(tmp_path)
    calls = []
    async def handle(request):
        calls.append(request)
        return httpx.Response(200, json=discovery())
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http) as client:
            config["connections"]["work"][field] = value
            (directory / "connections.json").write_text(json.dumps(config))
            with pytest.raises(ValueError, match="changed"):
                await client.platform.list_accounts()
    assert len(calls) == 1


async def test_saved_selection_changes_do_not_retarget_existing_client(tmp_path):
    directory, _, config = saved(tmp_path)
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(200, json=discovery()))) as http:
        async with await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http) as client:
            config["connections"]["work"]["accountId"] = "other"
            config["connections"]["work"]["universe"] = "other"
            (directory / "connections.json").write_text(json.dumps(config))
            assert client.account_id == "account-a"
            assert client._universe == "u"
            await client.platform.list_accounts()


async def test_non_ascii_renewal_never_replaces_a_valid_credential(tmp_path):
    directory, path, _ = saved(tmp_path)
    imported = _connection.load_connection(None, directory, None, None)
    request = httpx.Request("GET", "https://platform.test/team/api/v1/connection", headers={"authorization": "Bearer token-one"})
    response = httpx.Response(200, request=request, headers={b"set-auth-token": "renewed-é".encode("utf-8")})
    with pytest.raises(ValueError, match="bearer token"):
        await imported.accept_response(request, response)
    assert path.read_text() == "token-one"


@pytest.mark.skipif(os.name != "posix", reason="POSIX metadata integrity")
async def test_writable_connection_metadata_is_refused_before_credentials_are_sent(tmp_path):
    directory, _, _ = saved(tmp_path)
    (directory / "connections.json").chmod(0o666)
    def never_send(_):
        raise AssertionError("unsafe metadata reached the network")
    async with httpx.AsyncClient(transport=httpx.MockTransport(never_send)) as http:
        with pytest.raises(ValueError, match="writable"):
            await AsyncLightspeedClient.from_connection(config_dir=directory, http_client=http)

import asyncio
from argparse import Namespace
from uuid import UUID

import pytest

import bootstrap
from bootstrap import SetupError

TEST_UNIVERSE = "6c696768-7473-4065-8064-000000000010"


def arguments(path, **overrides):
    return Namespace(**{
        "env_file": path, "gateway": None, "universe": None, "profile": None,
        "environment_id": None, "envd_endpoint": None, "model_provider": None,
        "model_api_kind": None, "model": None, **overrides,
    })


def test_explicit_remote_target_does_not_use_local_handoff_key(monkeypatch, tmp_path):
    monkeypatch.delenv("LIGHTSPEED_DEMO_ADMIN_API_KEY", raising=False)
    monkeypatch.setattr(bootstrap, "launcher_handoff", lambda: {
        "endpoint": bootstrap.DEFAULT_GATEWAY, "credentialFile": "/unread-local-key",
    })
    args = arguments(tmp_path / ".env", gateway="https://elsewhere.example/rpc")
    with pytest.raises(SetupError, match="LIGHTSPEED_DEMO_ADMIN_API_KEY"):
        asyncio.run(bootstrap.initialize(args))


def test_local_setup_targets_platform_test_universe_and_leaves_app_config(monkeypatch, tmp_path):
    calls = []
    env_file = tmp_path / ".env"
    original = "LIGHTSPEED_API_URL=http://127.0.0.1:18080/rpc\nUSER_SETTING=keep\n"
    env_file.write_text(original)
    key_file = tmp_path / "launcher.key"
    key_file.write_text("launcher-key")
    key_file.chmod(0o600)
    monkeypatch.delenv("LIGHTSPEED_DEMO_ADMIN_API_KEY", raising=False)
    monkeypatch.delenv("LIGHTSPEED_DEMO_MODEL_API_KEY", raising=False)
    monkeypatch.delenv("LIGHTSPEED_UNIVERSE", raising=False)
    monkeypatch.delenv("LIGHTSPEED_API_URL", raising=False)
    monkeypatch.delenv("LIGHTSPEED_PROFILE_ID", raising=False)
    monkeypatch.setattr(bootstrap, "ROOT", tmp_path)
    monkeypatch.setattr(bootstrap, "launcher_handoff", lambda: {
        "endpoint": bootstrap.DEFAULT_GATEWAY, "credentialFile": str(key_file),
    })

    class Client:
        def __init__(self, endpoint, *, api_key, universe=None):
            assert endpoint == bootstrap.DEFAULT_GATEWAY
            self.api_key = api_key
            self.universe = universe

        async def __aenter__(self):
            return self

        async def __aexit__(self, *_args):
            pass

        async def initialize(self):
            if self.api_key == "scoped-key":
                return {"caller": {
                    "scope": {"kind": "universe", "universeId": TEST_UNIVERSE},
                    "groups": sorted(bootstrap.APP_GROUPS),
                }}
            assert self.api_key == "launcher-key"
            return {"caller": {"scope": {"kind": "deployment"}}}

        async def call(self, method, params):
            calls.append((self.universe, method, params))
            if method == "deployment/universes/list":
                return {"result": {"universes": [{"slug": "test", "universeId": TEST_UNIVERSE}]}}
            if method == "environments/external/create":
                return {"result": {"environment": {"environmentId": "daemon"}}}
            if method == "vfs/snapshots/commit":
                return {"result": {"snapshotRef": "snapshot"}}
            if method == "deployment/api-keys/create":
                return {"result": {"apiKey": {"keyPrefix": "prefix"}, "secret": "scoped-key"}}
            raise AssertionError(f"Unexpected method {method}")

        async def read_profile(self, profile_id):
            for universe, method, profile in calls:
                if universe == TEST_UNIVERSE and method == "profiles/create":
                    return {"profile": profile}
            raise bootstrap.LightspeedRpcError({"code": -32004, "message": "missing"})

        async def read_workspace(self, _workspace_id):
            raise bootstrap.LightspeedRpcError({"code": -32004, "message": "missing"})

        async def put_blob(self, _text):
            return "blob"

        async def create_workspace(self, **kwargs):
            calls.append((self.universe, "vfs/workspaces/create", kwargs))

        async def create_profile(self, *, profile):
            calls.append((self.universe, "profiles/create", profile))

        async def read_model_defaults(self):
            return {"defaults": {"agentRun": {"providerId": "openai", "model": "configured-model"}}}

    monkeypatch.setattr(bootstrap, "AsyncLightspeedClient", Client)
    args = arguments(env_file)
    asyncio.run(bootstrap.initialize(args))
    asyncio.run(bootstrap.initialize(args))

    profile_calls = [call for call in calls if call[1] == "profiles/create"]
    assert len(profile_calls) == 1
    universe, _, profile = profile_calls[0]
    assert universe == TEST_UNIVERSE
    assert profile["profileId"] == "internals-demo"
    assert profile["config"]["features"]["vfs"]["workspaces"][0]["path"] == "/workspace"
    assert profile["config"]["features"]["environments"]["environments"][0]["environmentId"] == "daemon"
    key_calls = [params for _, method, params in calls if method == "deployment/api-keys/create"]
    assert len(key_calls) == 1
    assert key_calls[0]["scope"] == {"kind": "universe", "universeId": TEST_UNIVERSE}
    assert env_file.read_text() == original
    key = next((tmp_path / ".lightspeed-dev").glob("streamlit-bootstrap/*/api-key"))
    assert key.read_text() == "scoped-key\n"
    assert key.stat().st_mode & 0o777 == 0o600


def test_model_options_must_describe_a_complete_route(monkeypatch, tmp_path):
    monkeypatch.delenv("LIGHTSPEED_DEMO_MODEL_API_KEY", raising=False)
    with pytest.raises(SetupError, match="together"):
        bootstrap.model_override(arguments(tmp_path / ".env", model_provider="openai"))


def test_reuses_platform_uuid_without_discovering_by_slug():
    class Admin:
        async def call(self, method, params):
            assert method == "deployment/universes/read"
            assert params == {"universeId": TEST_UNIVERSE}

    assert asyncio.run(bootstrap.resolve_universe(Admin(), str(UUID(TEST_UNIVERSE)))) == TEST_UNIVERSE


def test_explicit_model_updates_only_the_profile_route(tmp_path):
    features = {
        "vfs": {"workspaces": [{"workspaceId": "demo-workspace", "path": "/workspace", "access": "read"}]},
        "environments": {"environments": [{"environmentId": "env", "default": True, "access": "exec"}]},
    }
    model = {"providerId": "openai", "apiKind": "openai:responses", "model": "requested-model"}
    profile = {"profileId": "internals-demo", "revision": 5, "config": {"features": features}, "metadata": {"keep": "value"}}

    class Client:
        async def put_profile(self, *, expected_revision, profile):
            assert expected_revision == 5
            assert profile["config"] == {"features": features, "model": model}
            assert profile["metadata"] == {"keep": "value"}

    assert asyncio.run(bootstrap.update_profile_model(Client(), profile, arguments(tmp_path / ".env"), model)) == model

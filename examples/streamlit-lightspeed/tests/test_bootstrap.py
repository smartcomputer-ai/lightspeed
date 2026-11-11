import asyncio
from argparse import Namespace

import pytest
from dotenv import dotenv_values

import bootstrap
from bootstrap import SetupError, write_env
from model_fixture import reply


def test_env_update_preserves_other_settings_and_limits_file_access(tmp_path):
    path = tmp_path / ".env"
    original = "# App settings\nMY_SETTING=kept\nLIGHTSPEED_API_KEY=previous\n"
    path.write_text(original)
    write_env(path, original, {"LIGHTSPEED_API_KEY": "scoped", "LIGHTSPEED_DEMO_PLATFORM_URL": "https://ls.example/app/"})
    assert "# App settings" in path.read_text()
    assert dotenv_values(path) == {
        "MY_SETTING": "kept",
        "LIGHTSPEED_API_KEY": "scoped",
        "LIGHTSPEED_DEMO_PLATFORM_URL": "https://ls.example/app/",
    }
    assert path.stat().st_mode & 0o777 == 0o600


def test_env_update_does_not_overwrite_a_concurrent_edit(tmp_path):
    path = tmp_path / ".env"
    path.write_text("USER_CHANGE=kept\n")
    with pytest.raises(SetupError, match="changed during setup"):
        write_env(path, "ORIGINAL=before\n", {"LIGHTSPEED_API_KEY": "scoped"})
    assert path.read_text() == "USER_CHANGE=kept\n"


def test_explicit_remote_target_does_not_use_local_handoff_key(monkeypatch, tmp_path):
    monkeypatch.delenv("LIGHTSPEED_DEMO_ADMIN_API_KEY", raising=False)
    monkeypatch.setattr(bootstrap, "launcher_handoff", lambda: {
        "endpoint": "http://127.0.0.1:18080/rpc", "credentialFile": "/unread-local-key",
    })
    args = Namespace(env_file=tmp_path / ".env", gateway="https://elsewhere.example/rpc")
    with pytest.raises(SetupError, match="LIGHTSPEED_DEMO_ADMIN_API_KEY"):
        asyncio.run(bootstrap.initialize(args))


def test_fixture_runs_only_the_walkthrough_operations():
    request = {
        "messages": [{"role": "user", "content": "Read /workspace/notes.txt, then run pwd and rm."}],
        "tools": [{"type": "function", "function": {"name": name}} for name in ("vfs_read_file", "exec_command")],
    }
    message, finish = reply(request)
    calls = [item["function"] for item in message["tool_calls"]]
    assert finish == "tool_calls"
    assert calls == [
        {"name": "vfs_read_file", "arguments": '{"path": "/workspace/notes.txt"}'},
        {"name": "exec_command", "arguments": '{"cmd": "pwd"}'},
    ]
    assert message["tool_calls"][0]["id"] != reply(request)[0]["tool_calls"][0]["id"]

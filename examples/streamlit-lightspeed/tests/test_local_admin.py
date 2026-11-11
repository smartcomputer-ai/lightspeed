import httpx
import pytest

import local_admin
from local_admin import (
    PLATFORM_ORIGIN, PLATFORM_URL, SESSION_COOKIE, TEST_SESSIONS, TEST_UNIVERSE,
    LocalAdmin, load_local_admin, platform_session_cookie, save_local_admin, start_handoff, state_dir,
)

ADMIN = LocalAdmin("admin@example.test", "fixture-password")
COOKIE = f"{SESSION_COOKIE}=fixture-session; Path=/; HttpOnly; SameSite=Lax"
GATEWAY = "http://127.0.0.1:18080/rpc"


@pytest.mark.parametrize("role", ["admin", "user,admin", "user"])
def test_sign_in_uses_the_normal_platform_session(role):
    def respond(request):
        if request.method == "POST":
            assert str(request.url) == f"{PLATFORM_ORIGIN}/api/auth/sign-in/email"
            assert request.headers["Origin"] == PLATFORM_ORIGIN
            assert request.content == b'{"email":"admin@example.test","password":"fixture-password"}'
            return httpx.Response(200, headers=[("Set-Cookie", COOKIE), ("Set-Cookie", "other=value; Path=/")])
        assert str(request.url) == f"{PLATFORM_ORIGIN}/api/v1/me"
        assert f"{SESSION_COOKIE}=fixture-session" in request.headers["Cookie"]
        return httpx.Response(200, json={"user": {"role": role}})

    with httpx.Client(transport=httpx.MockTransport(respond)) as client:
        assert platform_session_cookie(client, ADMIN) == (None if role == "user" else COOKIE)


def test_handoff_restricts_and_consumes_ticket(monkeypatch):
    now = 1
    calls = []

    def sign_in(_client, admin):
        calls.append(admin)
        return COOKIE

    monkeypatch.setattr(local_admin, "monotonic", lambda: now)
    monkeypatch.setattr(local_admin, "platform_session_cookie", sign_in)
    with start_handoff() as server, httpx.Client(trust_env=False) as browser:
        try:
            link = server.issue(ADMIN)
            assert "fixture" not in link
            assert browser.get(link, headers={"Host": "elsewhere.test"}).status_code == 403
            assert browser.get(link, headers={"Sec-Fetch-Site": "cross-site"}).status_code == 403
            assert browser.get(link, headers={"Sec-Purpose": "prefetch"}).status_code == 403
            assert calls == []
            response = browser.get(link, headers={"Sec-Fetch-Site": "same-site"})
            assert response.status_code == 303
            assert response.headers["Location"] == TEST_SESSIONS
            assert response.headers["Referrer-Policy"] == "no-referrer"
            assert response.headers["Cache-Control"] == "no-store"
            assert response.headers.get_list("Set-Cookie") == [COOKIE]
            assert "Set-Cookie" not in browser.get(link).headers

            expired = server.issue(ADMIN)
            now += local_admin.TICKET_LIFETIME
            assert "Set-Cookie" not in browser.get(expired).headers
            assert calls == [ADMIN]
        finally:
            server.shutdown()


def test_config_is_private_and_scoped_to_the_local_demo(tmp_path):
    directory = state_dir(tmp_path, GATEWAY, TEST_UNIVERSE)
    save_local_admin(directory, ADMIN)
    assert load_local_admin(tmp_path, GATEWAY, PLATFORM_URL, TEST_UNIVERSE) == ADMIN
    assert load_local_admin(tmp_path, GATEWAY, "https://platform.example/app/", TEST_UNIVERSE) is None
    assert load_local_admin(tmp_path, GATEWAY, PLATFORM_URL, "another-universe") is None
    assert load_local_admin(tmp_path, "http://127.0.0.1:28080/rpc", PLATFORM_URL, TEST_UNIVERSE) is None

    config = directory / "platform-admin.json"
    config.chmod(0o644)
    assert load_local_admin(tmp_path, GATEWAY, PLATFORM_URL, TEST_UNIVERSE) is None
    config.chmod(0o600)
    real_config = config.rename(directory / "real-config.json")
    config.symlink_to(real_config)
    assert load_local_admin(tmp_path, GATEWAY, PLATFORM_URL, TEST_UNIVERSE) is None

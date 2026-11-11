"""One-time browser sign-in for the loopback Streamlit demo."""

import json
import os
import secrets
import stat
from dataclasses import dataclass, field
from hashlib import sha256
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from tempfile import NamedTemporaryFile
from threading import Lock, Thread
from time import monotonic
from urllib.parse import parse_qs, urlsplit

import httpx

LOOPBACK = "127.0.0.1"
PLATFORM_ORIGIN = f"http://{LOOPBACK}:5173"
PLATFORM_URL = f"{PLATFORM_ORIGIN}/app/"
TEST_UNIVERSE = "6c696768-7473-4065-8064-000000000010"
TEST_SESSIONS = f"{PLATFORM_URL}u/test/sessions"
SESSION_COOKIE = "better-auth.session_token"
TICKET_LIFETIME = 300
MAX_TICKETS = 128


@dataclass(frozen=True)
class LocalAdmin:
    email: str
    password: str = field(repr=False)


def state_dir(root: Path, endpoint: str, universe: str) -> Path:
    identity = sha256(f"{endpoint}\n{universe}".encode()).hexdigest()[:20]
    return root / ".lightspeed-dev/streamlit-bootstrap" / identity


def private_read(path: Path) -> str:
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)) as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_mode & 0o077 or info.st_uid != os.getuid():
            raise ValueError(f"Expected an owner-only regular file at {path}.")
        return handle.read().strip()


def save_local_admin(directory: Path, admin: LocalAdmin) -> None:
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    with NamedTemporaryFile(dir=directory, mode="w", delete=False) as handle:
        temporary = Path(handle.name)
        try:
            json.dump({"email": admin.email, "password": admin.password}, handle)
            handle.close()
            temporary.replace(directory / "platform-admin.json")
        finally:
            temporary.unlink(missing_ok=True)


def local_test_target(endpoint: str, platform_url: str | None, universe: str | None) -> bool:
    gateway = urlsplit(endpoint)
    return (
        gateway.scheme == "http" and gateway.hostname == LOOPBACK
        and platform_url == PLATFORM_URL and universe == TEST_UNIVERSE
    )


def load_local_admin(
    root: Path, endpoint: str, platform_url: str | None, universe: str | None,
) -> LocalAdmin | None:
    if not local_test_target(endpoint, platform_url, universe):
        return None
    path = state_dir(root, endpoint, TEST_UNIVERSE) / "platform-admin.json"
    try:
        data = json.loads(private_read(path))
    except (OSError, ValueError):
        return None
    if isinstance(data, dict) and all(isinstance(data.get(key), str) and data[key] for key in ("email", "password")):
        return LocalAdmin(data["email"], data["password"])
    return None


def platform_session_cookie(client: httpx.Client, admin: LocalAdmin) -> str | None:
    response = client.post(
        f"{PLATFORM_ORIGIN}/api/auth/sign-in/email",
        json={"email": admin.email, "password": admin.password},
        headers={"Origin": PLATFORM_ORIGIN},
    )
    if not response.is_success:
        return None
    cookie = next(
        (value for value in response.headers.get_list("set-cookie") if value.startswith(f"{SESSION_COOKIE}=")),
        None,
    )
    if not cookie:
        return None
    profile = client.get(f"{PLATFORM_ORIGIN}/api/v1/me")
    if not profile.is_success:
        return None
    body = profile.json()
    user = body.get("user") if isinstance(body, dict) else None
    role = user.get("role") if isinstance(user, dict) else None
    return cookie if isinstance(role, str) and "admin" in role.split(",") else None


class AdminHandoff(ThreadingHTTPServer):
    def __init__(self) -> None:
        super().__init__((LOOPBACK, 0), AdminRequest)
        self.host = f"{LOOPBACK}:{self.server_port}"
        self.tickets: dict[str, tuple[float, LocalAdmin]] = {}
        self.lock = Lock()

    def issue(self, admin: LocalAdmin) -> str:
        now = monotonic()
        ticket = secrets.token_urlsafe(32)
        with self.lock:
            self.tickets = {key: entry for key, entry in self.tickets.items() if entry[0] > now}
            if len(self.tickets) >= MAX_TICKETS:
                self.tickets.pop(next(iter(self.tickets)))
            self.tickets[ticket] = (now + TICKET_LIFETIME, admin)
        return f"http://{self.host}/open?ticket={ticket}"

    def consume(self, ticket: str) -> LocalAdmin | None:
        with self.lock:
            entry = self.tickets.pop(ticket, None)
        return entry[1] if entry and entry[0] > monotonic() else None


class AdminRequest(BaseHTTPRequestHandler):
    server: AdminHandoff
    timeout = 5

    def log_message(self, *_args: object) -> None:
        # A request URL contains a sign-in ticket.
        pass

    def do_GET(self) -> None:
        if (
            self.headers.get("Host") != self.server.host
            or self.headers.get("Sec-Fetch-Site") == "cross-site"
            or self.headers.get("Purpose") or self.headers.get("Sec-Purpose")
        ):
            self.send_error(403)
            return
        route = urlsplit(self.path)
        if route.path != "/open":
            self.send_error(404)
            return
        tickets = parse_qs(route.query).get("ticket", [])
        admin = self.server.consume(tickets[0]) if len(tickets) == 1 else None
        cookie = None
        if admin:
            try:
                with httpx.Client(trust_env=False, timeout=5) as client:
                    cookie = platform_session_cookie(client, admin)
            except (httpx.HTTPError, ValueError):
                pass
        self.send_response(303)
        self.send_header("Location", TEST_SESSIONS)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Referrer-Policy", "no-referrer")
        if cookie:
            self.send_header("Set-Cookie", cookie)
        self.end_headers()


def start_handoff() -> AdminHandoff:
    handoff = AdminHandoff()
    Thread(target=handoff.serve_forever, daemon=True).start()
    return handoff

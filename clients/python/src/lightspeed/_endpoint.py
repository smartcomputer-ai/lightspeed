"""One API base normalization policy for runtime and Platform requests."""
from urllib.parse import urlsplit, urlunsplit

import httpx


def normalize_endpoint(endpoint: str | httpx.URL) -> str:
    raw = str(endpoint)
    if "\\" in raw or raw != raw.strip() or any(ord(character) < 32 or ord(character) == 127 for character in raw):
        raise ValueError("Deployment URL must not contain whitespace or control characters")
    parsed = urlsplit(raw)
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise ValueError("Deployment URL must be an absolute HTTP or HTTPS URL")
    if parsed.username is not None or parsed.password is not None or "?" in raw or "#" in raw:
        raise ValueError("Deployment URL must not contain credentials, query parameters or a fragment")
    # Accessing port also validates malformed ports before any request.
    _ = parsed.port
    path = parsed.path.rstrip("/")
    if not path.endswith("/api/v1"):
        path += "/api/v1"
    return str(httpx.URL(urlunsplit((parsed.scheme, parsed.netloc, path, "", "")))).rstrip("/")

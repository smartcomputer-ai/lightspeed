"""Import one CLI connection without combining it with ambient credentials."""
from __future__ import annotations

import ipaddress
import json
import os
import stat
import tempfile
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

import httpx

from ._endpoint import normalize_endpoint
from ._http import HTTP_CONTRACT_REVISION

_ROTATION_LOCK = threading.Lock()


def _config_directory(value: str | os.PathLike[str] | None) -> Path:
    if value is not None:
        return Path(value).expanduser().absolute()
    if directory := os.environ.get("LIGHTSPEED_CONFIG_DIR"):
        return Path(directory).absolute()
    if root := os.environ.get("XDG_CONFIG_HOME"):
        return Path(root).absolute() / "lightspeed"
    return Path.home() / ".config" / "lightspeed"


def _read_config(directory: Path) -> dict[str, Any]:
    if os.name == "posix":
        parent = directory.stat()
        if parent.st_uid != os.getuid() or parent.st_mode & 0o022:
            raise ValueError("CLI configuration directory must be owned by this user and not writable by others")
    path = directory / "connections.json"
    if path.is_symlink():
        raise ValueError("CLI configuration must not be a symbolic link")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or (os.name == "posix" and (info.st_uid != os.getuid() or info.st_mode & 0o022)):
            raise ValueError("CLI configuration must be a regular file owned by this user and not writable by others")
        with os.fdopen(fd, "r", closefd=False) as source:
            value = json.load(source)
        if not isinstance(value, dict):
            raise ValueError("Invalid CLI connection configuration")
        return value
    finally:
        os.close(fd)


def _private_file(fd: int) -> os.stat_result:
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode):
        raise ValueError("Credential must be a regular file")
    if os.name == "posix" and (info.st_mode & 0o077 or info.st_uid != os.getuid()):
        raise ValueError("Credential file must belong to the current user and have owner-only permissions (chmod 600)")
    return info


def _open_secret(path: Path) -> int:
    if path.is_symlink():
        raise ValueError("Credential file must not be a symbolic link")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        _private_file(fd)
        return fd
    except BaseException:
        os.close(fd)
        raise


def _token(value: str) -> str:
    value = value.strip()
    if not value or len(value) > 4096 or any(not 0x21 <= ord(character) <= 0x7e for character in value):
        raise ValueError("Credential must be a nonempty single bearer token")
    return value


def _read_fd(fd: int) -> str:
    os.lseek(fd, 0, os.SEEK_SET)
    value = os.read(fd, 16385)
    if len(value) > 16384:
        raise ValueError("Credential file is too large")
    return _token(value.decode("utf-8"))


def read_secret(path: Path) -> str:
    fd = _open_secret(path)
    try:
        return _read_fd(fd)
    finally:
        os.close(fd)


def _optional_string(value: Any, field: str) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"Saved connection {field} must be a nonempty string")
    return value


@dataclass
class ImportedConnection:
    name: str
    endpoint: str
    credential_kind: str
    server_kind: str
    credential_file: Path | None
    owned_credential: bool
    directory: Path
    single: bool
    allow_insecure_http: bool
    universe: str | None
    account_id: str | None

    def _bound(self) -> bool:
        try:
            config = _read_config(self.directory)
            saved = config.get("connections", {}).get(self.name)
            if not isinstance(saved, dict):
                return False
            return (normalize_endpoint(saved["endpoint"]) == self.endpoint
                    and saved.get("credentialKind") == self.credential_kind
                    and saved.get("serverKind") == self.server_kind
                    and saved.get("single") is self.single
                    and saved.get("allowInsecureHttp", False) is self.allow_insecure_http
                    and saved.get("ownedCredential", False) is self.owned_credential
                    and (os.path.abspath(saved["credentialFile"]) if saved.get("credentialFile") else None) == (str(self.credential_file) if self.credential_file else None))
        except (OSError, ValueError, TypeError, KeyError, AttributeError):
            return False

    def credential(self) -> str | None:
        if not self._bound():
            raise ValueError("Saved connection changed or was removed; create a new SDK client")
        return read_secret(self.credential_file) if self.credential_file is not None else None

    def _path_referenced(self) -> bool:
        try:
            config = _read_config(self.directory)
            return any(os.path.abspath(saved["credentialFile"]) == str(self.credential_file)
                       for saved in config["connections"].values()
                       if isinstance(saved, dict) and isinstance(saved.get("credentialFile"), str))
        except FileNotFoundError:
            return False
        except (OSError, ValueError, TypeError, KeyError, AttributeError):
            # An unreadable configuration cannot establish that the file is orphaned.
            return True

    def _owns_session_file(self) -> bool:
        path = self.credential_file
        directory = self.directory / "credentials"
        return (self.credential_kind == "platformSession" and self.owned_credential and path is not None
                and path.parent == directory and not directory.is_symlink())

    async def accept_response(self, request: httpx.Request, response: httpx.Response) -> None:
        if not self._owns_session_file() or not (renewed := response.headers.get("set-auth-token")):
            return
        replacement = _token(renewed)
        bearer = request.headers.get("authorization", "")
        if not bearer.startswith("Bearer "):
            return
        previous = bearer[7:]
        if replacement == previous:
            return
        assert self.credential_file is not None
        path = self.credential_file
        # Atomic replacement keeps readers in the CLI and other processes from
        # observing a partially written token. The binding pins this exact file.
        with _ROTATION_LOCK:
            if not self._bound():
                return
            try:
                source: int | None = _open_secret(path)
            except FileNotFoundError:
                return
            temporary: Path | None = None
            try:
                assert source is not None
                original = _private_file(source)
                if _read_fd(source) != previous:
                    return
                fd, filename = tempfile.mkstemp(prefix=".session-", suffix=".tmp", dir=path.parent)
                temporary = Path(filename)
                with os.fdopen(fd, "wb") as output:
                    output.write(replacement.encode("utf-8"))
                    output.flush()
                    os.fsync(output.fileno())
                    created = os.fstat(output.fileno())
                if not self._bound():
                    return
                try:
                    latest = path.lstat()
                    if (latest.st_dev, latest.st_ino) != (original.st_dev, original.st_ino) or read_secret(path) != previous:
                        return
                except FileNotFoundError:
                    return
                os.close(source)
                source = None
                os.replace(temporary, path)
                temporary = None
                if not self._bound() and not self._path_referenced():
                    try:
                        orphan = path.lstat()
                        if (orphan.st_dev, orphan.st_ino) == (created.st_dev, created.st_ino):
                            path.unlink()
                    except FileNotFoundError:
                        pass
            finally:
                if source is not None:
                    os.close(source)
                if temporary is not None:
                    temporary.unlink(missing_ok=True)

    def verify(self, discovery: dict[str, Any]) -> None:
        if discovery.get("contractRevision") != HTTP_CONTRACT_REVISION:
            raise ValueError("Deployment HTTP contract revision is incompatible with this SDK")
        runtime = discovery.get("runtime")
        if runtime != self.server_kind:
            raise ValueError("Deployment kind does not match the saved connection")
        caller = discovery.get("caller")
        if not isinstance(caller, dict) or caller.get("single") is not self.single:
            raise ValueError("Deployment authentication does not match the saved connection")
        if runtime == "platform":
            platform = discovery.get("platform")
            credential = platform.get("credential") if isinstance(platform, dict) else None
            principal = platform.get("principal") if isinstance(platform, dict) else None
            if not isinstance(principal, dict) or principal.get("kind") not in ("user", "application") or not isinstance(principal.get("id"), str):
                raise ValueError("Platform discovery omitted its authenticated principal")
            expected = {"cliSession"} if self.credential_kind == "platformSession" else {"personalKey", "applicationKey"}
            if not isinstance(credential, dict) or credential.get("kind") not in expected or not isinstance(credential.get("id"), str):
                raise ValueError("Platform credential kind does not match the saved connection")
            expected_principal = "application" if credential["kind"] == "applicationKey" else "user"
            if principal.get("kind") != expected_principal:
                raise ValueError("Platform credential and principal kinds do not agree")
        elif runtime != "runtime":
            raise ValueError("Unsupported deployment kind")


def load_connection(name: str | None, config_dir: str | os.PathLike[str] | None, universe: str | None, account: str | None) -> ImportedConnection:
    directory = _config_directory(config_dir)
    config = _read_config(directory)
    selected = name if name is not None else config.get("selected")
    if not isinstance(selected, str) or not selected:
        raise ValueError("No CLI connection selected; run lightspeed login or connect first")
    connections = config.get("connections")
    saved = connections.get(selected) if isinstance(connections, dict) else None
    if not isinstance(saved, dict):
        raise ValueError(f"Unknown CLI connection {selected!r}")
    raw_endpoint = saved.get("endpoint")
    if not isinstance(raw_endpoint, str):
        raise ValueError("Saved connection has no deployment endpoint")
    endpoint = normalize_endpoint(raw_endpoint)
    allow_insecure = saved.get("allowInsecureHttp", False)
    if type(allow_insecure) is not bool:
        raise ValueError("Saved allowInsecureHttp must be a boolean")
    parsed = urlsplit(endpoint)
    host = parsed.hostname or ""
    try:
        loopback = ipaddress.ip_address(host).is_loopback
    except ValueError:
        loopback = host == "localhost"
    if parsed.scheme != "https" and not loopback and not allow_insecure:
        raise ValueError("Saved deployment requires HTTPS except loopback or explicit allowInsecureHttp")
    kind = saved.get("credentialKind")
    server = saved.get("serverKind")
    single = saved.get("single")
    if type(single) is not bool or kind not in ("none", "runtimeToken", "platformApiKey", "platformSession") or server not in ("runtime", "platform"):
        raise ValueError("Saved connection lacks explicit credential and deployment kind; reconnect with the CLI")
    if (server == "runtime" and kind not in ("none", "runtimeToken")) or (server == "platform" and kind not in ("platformApiKey", "platformSession")):
        raise ValueError("Saved credential kind does not match its deployment")
    path_value = _optional_string(saved.get("credentialFile"), "credentialFile")
    if path_value is not None and not os.path.isabs(path_value):
        raise ValueError("Saved credential file must be an absolute path")
    path = Path(os.path.abspath(path_value)) if path_value is not None else None
    if single != (kind == "none") or single != (path is None):
        raise ValueError("Saved authentication mode and credential file do not agree")
    account_id = _optional_string(account if account is not None else saved.get("accountId"), "accountId")
    if account_id is not None and server != "platform":
        raise ValueError("Accounts require a Platform deployment")
    selection = _optional_string(universe if universe is not None else saved.get("universe"), "universe")
    if account is not None and account != saved.get("accountId") and universe is None:
        selection = None
    owned = saved.get("ownedCredential", False)
    if type(owned) is not bool:
        raise ValueError("Saved ownedCredential must be a boolean")
    result = ImportedConnection(selected, endpoint, kind, server, path, owned, directory, single, allow_insecure, selection, account_id)
    if kind == "platformSession" and owned and not result._owns_session_file():
        raise ValueError("Owned CLI session must live directly in this configuration's credentials directory")
    result.credential()  # Validate before sending any credential over the network.
    return result

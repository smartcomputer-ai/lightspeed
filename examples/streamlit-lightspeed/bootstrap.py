"""Prepare the gateway, resources, and app configuration for the Streamlit demo."""

from __future__ import annotations

import argparse
import asyncio
import fcntl
import hashlib
import json
import os
import stat
import subprocess
import sys
from collections.abc import Awaitable
from dataclasses import dataclass
from pathlib import Path
from tempfile import NamedTemporaryFile
from urllib.parse import urlsplit
from uuid import UUID, uuid4

import httpx
from dotenv import dotenv_values, set_key

from lightspeed_ai import AsyncLightspeedClient, LightspeedRpcError, LightspeedTransportError
from model_fixture import MODEL, NOTE

EXAMPLE = Path(__file__).resolve().parent
ROOT = EXAMPLE.parents[1]
HANDOFF = ROOT / ".lightspeed/cli/connection.json"
DEFAULT_GATEWAY = "http://127.0.0.1:18080/rpc"
DEFAULT_PLATFORM = "http://127.0.0.1:5173/app/"
DEFAULT_DAEMON = "ws://127.0.0.1:19091/"
APP_GROUPS = {"session", "vfs", "environments"}
PROVIDER = "streamlit-fixture"
MODEL_ROUTE = {"providerId": PROVIDER, "apiKind": "openai:completions", "model": MODEL}


class SetupError(Exception):
    pass


@dataclass(frozen=True)
class EnvironmentFile:
    path: Path
    original: str | None
    values: dict[str, str | None]

    @classmethod
    def read(cls, path: Path) -> EnvironmentFile:
        path = path.absolute()
        if path.is_symlink():
            raise SetupError("Use a regular file as the app environment file.")
        original = path.read_text() if path.exists() else None
        return cls(path, original, dotenv_values(path, interpolate=False))

    def setting(self, key: str, default: str = "") -> str:
        return (os.environ.get(key, self.values.get(key, default)) or "").strip()


def save_state(path: Path, state: dict) -> None:
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state) + "\n")
    temporary.replace(path)


def http_url(value: str, label: str) -> str:
    url = urlsplit(value)
    if url.scheme not in {"http", "https"} or not url.hostname or url.username or url.password:
        raise SetupError(f"{label} must be an HTTP or HTTPS URL without credentials.")
    return value


def loopback(url: str) -> bool:
    return urlsplit(url).hostname in {"127.0.0.1", "::1", "localhost"}


def private_read(path: Path) -> str:
    mode = path.lstat().st_mode
    if not stat.S_ISREG(mode) or mode & 0o077:
        raise SetupError(f"The dev handoff must use owner-only regular files. Check {path}.")
    return path.read_text()


def launcher_handoff() -> dict:
    if not HANDOFF.exists():
        return {}
    return json.loads(private_read(HANDOFF))


def write_env(path: Path, original: str | None, values: dict[str, str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    template = (EXAMPLE / ".env.example").read_text()
    with NamedTemporaryFile(dir=path.parent, mode="w", delete=False) as draft:
        temp = Path(draft.name)
        draft.write(original if original is not None else template)
    try:
        for key, value in values.items():
            set_key(temp, key, value)
        current = path.read_text() if path.exists() else None
        if current != original:
            raise SetupError("The environment file changed during setup. Review it and rerun init-env.sh.")
        temp.replace(path)
    finally:
        temp.unlink(missing_ok=True)


async def optional[T](call: Awaitable[T]) -> T | None:
    try:
        return await call
    except LightspeedRpcError as error:
        if error.kind != "notFound":
            raise
        return None


async def fixture_endpoint(directory: Path, instance: str) -> str:
    port_file = directory / "fixture.port"
    port = int(port_file.read_text()) if port_file.exists() else 0
    async with httpx.AsyncClient(timeout=0.5, trust_env=False) as http:
        async def ready(candidate: int) -> bool:
            if not candidate:
                return False
            try:
                response = await http.get(f"http://127.0.0.1:{candidate}/health")
                return response.is_success and response.json() == {"instance": instance}
            except (httpx.TransportError, ValueError):
                return False

        if await ready(port):
            return f"http://127.0.0.1:{port}/v1"
        with (directory / "fixture.log").open("a") as log:
            process = subprocess.Popen(
                [sys.executable, str(EXAMPLE / "model_fixture.py"), str(port_file), str(port), instance],
                stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True,
            )
        for _ in range(40):
            if process.poll() is not None:
                break
            if port_file.exists():
                candidate = port_file.read_text().strip()
                if candidate.isdigit() and await ready(int(candidate)):
                    (directory / "fixture.pid").write_text(str(process.pid))
                    return f"http://127.0.0.1:{candidate}/v1"
            await asyncio.sleep(0.1)
        if process.poll() is None:
            process.terminate()
        raise SetupError(f"The local fixture did not start. See {directory / 'fixture.log'}.")


async def ensure_model(
    client: AsyncLightspeedClient, managed: bool, directory: Path, universe: str, profile: dict | None,
) -> str:
    if profile and (profile.get("config") or {}).get("model"):
        return "Existing profile model"
    defaults = (await client.read_model_defaults())["defaults"]
    current = defaults.get("agentRun")
    if current and (not managed or current["providerId"] != PROVIDER):
        return "Existing agent-run model"
    if not managed:
        raise SetupError("Set Models → Defaults → Agent runs in the selected universe and rerun setup.")

    base_url = await fixture_endpoint(directory, universe)
    provider_id = f"model:{PROVIDER}"
    existing = await optional(client.call("auth/providers/read", {"providerId": provider_id}))
    if existing is None:
        await client.call("auth/providers/create", {
            "providerId": provider_id,
            "displayName": "Streamlit fixture",
            "config": {"type": "modelEndpoint", "endpoint": {
                "baseUrl": base_url, "apiKinds": [MODEL_ROUTE["apiKind"]],
            }},
        })
    if current is None:
        await client.call("models/defaults/put", {
            "slot": "agentRun", "expectedRevision": defaults["revision"], "model": MODEL_ROUTE,
        })
    return "Local fixture model"


async def ensure_profile(
    client: AsyncLightspeedClient, profile: str, existing: dict | None,
    args: argparse.Namespace, launcher: bool,
) -> None:
    if existing is not None:
        features = (existing.get("config") or {}).get("features") or {}
        workspaces = (features.get("vfs") or {}).get("workspaces", [])
        environments = (features.get("environments") or {}).get("environments", [])
        if not workspaces or not any(
            item.get("default") and item.get("environmentId") and item.get("access") in {"exec", "jobs"}
            and (not args.environment_id or item.get("environmentId") == args.environment_id)
            for item in environments
        ):
            raise SetupError(
                f"Profile {profile} needs a VFS attachment and a default command-capable environment. "
                "Update it in Lightspeed or select a different LIGHTSPEED_PROFILE_ID.",
            )
        return

    environment_id = args.environment_id
    if environment_id:
        await client.read_environment(environment_id)
    else:
        endpoint = args.envd_endpoint or (DEFAULT_DAEMON if launcher else None)
        if not endpoint:
            raise SetupError("Pass --environment-id or a reachable --envd-endpoint for this gateway.")
        response = await client.call("environments/external/create", {
            "requestId": f"streamlit-{profile}",
            "endpoint": endpoint,
            "displayName": "Streamlit demo daemon",
        })
        environment_id = response["result"]["environment"]["environmentId"]

    workspace_id = "demo-workspace"
    if await optional(client.read_workspace(workspace_id)) is None:
        blob_ref = await client.put_blob(NOTE)
        size = len(NOTE.encode())
        snapshot = await client.call("vfs/snapshots/commit", {"manifest": {
            "schema_version": "lightspeed.vfs.snapshot.v1",
            "root": {"entries": {"notes.txt": {
                "kind": "file", "blob_ref": blob_ref, "size_bytes": size,
                "media_type": "text/plain", "executable": False,
            }}},
            "totals": {"files": 1, "bytes": size},
        }})
        await client.create_workspace(
            workspace_id=workspace_id, display_name="Streamlit demo workspace",
            snapshot_ref=snapshot["result"]["snapshotRef"],
        )
    await client.create_profile(profile={
        "profileId": profile,
        "displayName": "Streamlit internals",
        "config": {"features": {
            "vfs": {"workspaces": [{"workspaceId": workspace_id, "path": "/workspace", "access": "read"}]},
            "environments": {"environments": [{"environmentId": environment_id, "default": True, "access": "exec"}]},
        }},
    })


def local_postgres(state: dict, state_path: Path) -> str:
    password = UUID(state.setdefault("postgres_password", uuid4().hex)).hex
    save_state(state_path, state)
    role = f"ls_demo_{UUID(state['id']).hex[:16]}"
    script = f"""
BEGIN;
DO $$ BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN
        CREATE ROLE {role} LOGIN PASSWORD '{password}';
    END IF;
END $$;
GRANT USAGE ON SCHEMA public TO {role};
GRANT SELECT (universe_id, session_id, lifecycle_status, head_seq, created_at_ms, updated_at_ms)
    ON public.sessions TO {role};
GRANT SELECT (universe_id, session_id, seq, event_kind, observed_at_ms)
    ON public.session_events TO {role};
GRANT SELECT (universe_id, session_id, through_seq, format_version, byte_len, created_at_ms)
    ON public.session_checkpoints TO {role};
COMMIT;
"""
    try:
        subprocess.run(
            ["docker", "exec", "-i", "lightspeed-postgres", "psql", "-Xq", "-v", "ON_ERROR_STOP=1",
             "-U", "lightspeed", "-d", "lightspeed"],
            input=script, text=True, capture_output=True, check=True, timeout=15,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise SetupError(
            "Could not provision the local PostgreSQL reader. Check the dev stack, "
            "supply LIGHTSPEED_DEMO_POSTGRES_DSN, or pass --no-local-postgres.",
        ) from error
    return f"postgresql://{role}:{password}@127.0.0.1:{os.environ.get('POSTGRES_PORT', '15432')}/lightspeed"


async def initialize(args: argparse.Namespace) -> None:
    output = EnvironmentFile.read(args.env_file)
    handoff = launcher_handoff()
    endpoint = http_url(
        args.gateway or output.setting("LIGHTSPEED_API_URL") or handoff.get("endpoint") or DEFAULT_GATEWAY,
        "Gateway",
    )
    launcher = loopback(endpoint) and endpoint == handoff.get("endpoint")
    if output.values.get("LIGHTSPEED_API_KEY") and (output.values.get("LIGHTSPEED_API_URL") or DEFAULT_GATEWAY) != endpoint:
        raise SetupError("The existing app key belongs to another gateway. Use a new --env-file.")
    admin_key = os.environ.get("LIGHTSPEED_DEMO_ADMIN_API_KEY")
    if not admin_key and launcher and handoff.get("credentialFile"):
        admin_key = private_read(Path(handoff["credentialFile"])).strip()
    if not admin_key:
        raise SetupError("Start authenticated ./dev.sh or set LIGHTSPEED_DEMO_ADMIN_API_KEY for this gateway.")

    directory = ROOT / ".lightspeed-dev/streamlit-bootstrap" / hashlib.sha256(str(output.path).encode()).hexdigest()[:20]
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    with (directory / "lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        state_path = directory / "state.json"
        state = json.loads(state_path.read_text()) if state_path.exists() else {"id": str(uuid4()), "endpoint": endpoint}
        if state["endpoint"] != endpoint:
            raise SetupError("This environment file has setup state for another gateway. Use a new --env-file.")
        save_state(state_path, state)
        await provision(args, output, directory, state, endpoint, admin_key, launcher)


async def provision(
    args: argparse.Namespace, output: EnvironmentFile, directory: Path, state: dict,
    endpoint: str, admin_key: str, launcher: bool,
) -> None:
    setting = output.setting
    existing_key = output.values.get("LIGHTSPEED_API_KEY") or ""
    universe = str(UUID(args.universe)) if args.universe else None
    if existing_key:
        async with AsyncLightspeedClient(endpoint, api_key=existing_key) as app:
            access = (await app.initialize())["caller"]
        scope = access["scope"]
        if scope["kind"] != "universe" or not APP_GROUPS.issubset(access["groups"]):
            raise SetupError("Use an existing universe key with session, VFS, and environment access or a fresh --env-file.")
        if universe and universe != scope["universeId"]:
            raise SetupError("The existing app key selects another universe. Use a new --env-file.")
        universe = scope["universeId"]

    if state.get("universe") and universe and state["universe"] != universe:
        raise SetupError("This environment file has setup state for another universe. Use a new --env-file.")
    universe = universe or state.get("universe") or state["id"]
    managed = universe == state["id"]
    if managed and not launcher:
        raise SetupError("Use --universe with an existing universe for a gateway outside the local dev launcher.")
    state["universe"] = universe
    save_state(directory / "state.json", state)
    profile = setting("LIGHTSPEED_PROFILE_ID", "internals-demo") or "internals-demo"
    platform = args.platform_url if args.platform_url is not None else setting(
        "LIGHTSPEED_DEMO_PLATFORM_URL", DEFAULT_PLATFORM if launcher else "",
    )
    if platform:
        http_url(platform, "Lightspeed web app")
    async with AsyncLightspeedClient(endpoint, api_key=admin_key) as admin:
        if (await admin.initialize())["caller"]["scope"]["kind"] != "deployment":
            raise SetupError("Initialization requires a deployment API key.")
        if managed:
            await admin.call("deployment/universes/create", {
                "universeId": universe, "slug": f"streamlit-demo-{universe[:8]}",
            })
        async with AsyncLightspeedClient(endpoint, api_key=admin_key, universe=universe) as client:
            response = await optional(client.read_profile(profile))
            existing = response["profile"] if response is not None else None
            model = await ensure_model(client, managed, directory, universe, existing)
            await ensure_profile(client, profile, existing, args, launcher)

        dsn = setting("LIGHTSPEED_DEMO_POSTGRES_DSN")
        if not dsn and launcher and not args.no_local_postgres:
            dsn = local_postgres(state, directory / "state.json")
        app_key = existing_key
        created_prefix = None
        if not app_key:
            created = (await admin.call("deployment/api-keys/create", {
                "scope": {"kind": "universe", "universeId": universe},
                "displayName": "Streamlit demo",
                "groups": sorted(APP_GROUPS),
            }))["result"]
            app_key = created["secret"]
            created_prefix = created["apiKey"]["keyPrefix"]
        try:
            write_env(output.path, output.original, {
                "LIGHTSPEED_API_URL": endpoint,
                "LIGHTSPEED_API_KEY": app_key,
                "LIGHTSPEED_UNIVERSE": "",
                "LIGHTSPEED_PROFILE_ID": profile,
                "LIGHTSPEED_DEMO_PLATFORM_URL": platform,
                "LIGHTSPEED_DEMO_TEMPORAL_ADDRESS": setting(
                    "LIGHTSPEED_DEMO_TEMPORAL_ADDRESS",
                    os.environ.get("TEMPORAL_ADDRESS", "127.0.0.1:7233") if launcher else "",
                ),
                "LIGHTSPEED_DEMO_TEMPORAL_NAMESPACE": setting(
                    "LIGHTSPEED_DEMO_TEMPORAL_NAMESPACE", os.environ.get("TEMPORAL_NAMESPACE", "default"),
                ),
                "LIGHTSPEED_DEMO_TEMPORAL_UI": setting("LIGHTSPEED_DEMO_TEMPORAL_UI", "http://127.0.0.1:8233" if launcher else ""),
                "LIGHTSPEED_DEMO_POSTGRES_DSN": dsn,
            })
        except (SetupError, OSError):
            if created_prefix:
                await admin.call("deployment/api-keys/revoke", {"keyPrefix": created_prefix})
            raise
    print(f"Configured {output.path} for universe {universe}.")
    print(f"{model} · profile {profile} · read-only PostgreSQL {'configured' if dsn else 'not configured'}")
    if output.path == EXAMPLE / ".env":
        print("Run uv run --locked streamlit run app.py from the example directory.")
    else:
        print("Run uv run --locked --env-file <your environment file> streamlit run app.py from the example directory.")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--env-file", type=Path, default=EXAMPLE / ".env", help="Environment file to write")
    parser.add_argument("--gateway", help="Gateway /rpc URL")
    parser.add_argument("--universe", help="Use an existing universe with a configured model")
    parser.add_argument("--platform-url", help="Lightspeed web app URL")
    environment = parser.add_mutually_exclusive_group()
    environment.add_argument("--environment-id", help="Attach an existing environment")
    environment.add_argument("--envd-endpoint", help="Register a direct daemon endpoint reachable from the gateway")
    parser.add_argument("--no-local-postgres", action="store_true", help="Use only a supplied PostgreSQL DSN")
    os.umask(0o077)
    try:
        asyncio.run(initialize(parser.parse_args()))
    except LightspeedRpcError as error:
        sys.exit(f"Lightspeed rejected setup ({error.kind}): {error.message}")
    except LightspeedTransportError as error:
        sys.exit(f"Could not reach the Lightspeed gateway: {error}")
    except BlockingIOError:
        sys.exit("Another initialization is running for this environment file.")
    except (SetupError, OSError, ValueError) as error:
        sys.exit(str(error))


if __name__ == "__main__":
    main()

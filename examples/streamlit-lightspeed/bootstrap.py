"""Provision the Lightspeed resources used by the Streamlit demo."""

from __future__ import annotations

import argparse
import asyncio
import fcntl
import hashlib
import json
import os
import stat
import sys
from collections.abc import Awaitable
from pathlib import Path
from tempfile import NamedTemporaryFile
from urllib.parse import urlsplit
from uuid import UUID

from dotenv import dotenv_values

from lightspeed_ai import AsyncLightspeedClient, LightspeedRpcError, LightspeedTransportError
from lightspeed_ai.types import AgentProfile, AgentProfileInput, CallerAccess, ModelConfig, SessionConfig

EXAMPLE = Path(__file__).resolve().parent
ROOT = EXAMPLE.parents[1]
HANDOFF = ROOT / ".lightspeed/cli/connection.json"
DEFAULT_GATEWAY = "http://127.0.0.1:18080/rpc"
DEFAULT_DAEMON = "ws://127.0.0.1:19091/"
WORKSPACE = "demo-workspace"
NOTE = "This file lives in the Lightspeed demo VFS workspace.\n"
APP_GROUPS = {"session", "vfs", "environments"}


class SetupError(Exception):
    pass


def gateway_url(value: str) -> str:
    url = urlsplit(value)
    if url.scheme not in {"http", "https"} or not url.hostname or url.username or url.password:
        raise SetupError("Gateway must be an HTTP or HTTPS URL without credentials.")
    return value


def private_read(path: Path) -> str:
    mode = path.lstat().st_mode
    if not stat.S_ISREG(mode) or mode & 0o077:
        raise SetupError(f"Expected an owner-only regular file at {path}.")
    return path.read_text().strip()


def launcher_handoff() -> dict:
    return json.loads(private_read(HANDOFF)) if HANDOFF.exists() else {}


async def optional[T](call: Awaitable[T]) -> T | None:
    try:
        return await call
    except LightspeedRpcError as error:
        if error.kind != "notFound":
            raise
        return None


async def resolve_universe(admin: AsyncLightspeedClient, selector: str) -> str:
    try:
        universe = str(UUID(selector))
    except ValueError:
        response = await admin.call("deployment/universes/list", {})
        for entry in response["result"].get("universes", []):
            if entry.get("slug") == selector:
                return entry["universeId"]
        raise SetupError(f"Universe {selector!r} was not found. Pass --universe with an existing UUID or slug.")
    await admin.call("deployment/universes/read", {"universeId": universe})
    return universe


async def read_access(endpoint: str, key: str) -> CallerAccess | None:
    if not key:
        return None
    try:
        async with AsyncLightspeedClient(endpoint, api_key=key) as app:
            return (await app.initialize())["caller"]
    except LightspeedRpcError as error:
        if error.kind != "unauthenticated":
            raise
        return None


def usable_key(access: CallerAccess | None, universe: str) -> bool:
    if access is None:
        return False
    scope = access["scope"]
    return (
        scope["kind"] == "universe" and scope["universeId"] == universe
        and APP_GROUPS.issubset(access["groups"])
    )


async def ensure_app_key(
    admin: AsyncLightspeedClient, endpoint: str, universe: str, access: CallerAccess | None,
) -> str:
    if usable_key(access, universe):
        return "Existing Streamlit configuration already has a usable universe key."

    identity = hashlib.sha256(f"{endpoint}\n{universe}".encode()).hexdigest()[:20]
    directory = ROOT / ".lightspeed-dev/streamlit-bootstrap" / identity
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    key_path = directory / "api-key"
    with (directory / "lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if key_path.exists() and usable_key(await read_access(endpoint, private_read(key_path)), universe):
            return f"Scoped app key file: {key_path}"
        key = (await admin.call("deployment/api-keys/create", {
            "scope": {"kind": "universe", "universeId": universe},
            "displayName": "Streamlit demo",
            "groups": sorted(APP_GROUPS),
        }))["result"]
        try:
            with NamedTemporaryFile(dir=directory, mode="w", delete=False) as handle:
                temporary = Path(handle.name)
                handle.write(key["secret"] + "\n")
            try:
                temporary.replace(key_path)
            finally:
                temporary.unlink(missing_ok=True)
        except OSError:
            await admin.call("deployment/api-keys/revoke", {"keyPrefix": key["apiKey"]["keyPrefix"]})
            raise
    return f"Scoped app key file: {key_path}"


def model_override(args: argparse.Namespace) -> ModelConfig | None:
    selected = (args.model_provider, args.model_api_kind, args.model)
    if all(selected):
        return {"providerId": selected[0], "apiKind": selected[1], "model": selected[2]}
    if any(selected) or os.environ.get("LIGHTSPEED_DEMO_MODEL_API_KEY"):
        raise SetupError("Pass --model-provider, --model-api-kind, and --model together.")
    return None


async def ensure_model_provider(client: AsyncLightspeedClient, model: ModelConfig) -> None:
    credential = os.environ.get("LIGHTSPEED_DEMO_MODEL_API_KEY")
    if not credential:
        return
    provider = f"model:{model['providerId']}"
    if await optional(client.call("auth/providers/read", {"providerId": provider})) is None:
        await client.call("auth/providers/create", {
            "providerId": provider, "config": {"type": "modelApiKey"}, "credential": credential,
        })


async def ensure_workspace(client: AsyncLightspeedClient) -> None:
    if await optional(client.read_workspace(WORKSPACE)) is not None:
        return
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
        workspace_id=WORKSPACE, display_name="Streamlit demo workspace",
        snapshot_ref=snapshot["result"]["snapshotRef"],
    )


async def ensure_profile(
    client: AsyncLightspeedClient, profile: str, args: argparse.Namespace, launcher: bool,
    model: ModelConfig | None,
) -> ModelConfig | None:
    response = await optional(client.read_profile(profile))
    if response is not None:
        return await update_profile_model(client, response["profile"], args, model)

    environment = args.environment_id
    if environment:
        await client.read_environment(environment)
    else:
        endpoint = args.envd_endpoint or (DEFAULT_DAEMON if launcher else None)
        if not endpoint:
            raise SetupError("Pass --environment-id or a reachable --envd-endpoint for this gateway.")
        response = await client.call("environments/external/create", {
            "requestId": f"streamlit-{profile}", "endpoint": endpoint, "displayName": "Streamlit demo daemon",
        })
        environment = response["result"]["environment"]["environmentId"]

    await ensure_workspace(client)
    config: SessionConfig = {"features": {
        "vfs": {"workspaces": [{"workspaceId": WORKSPACE, "path": "/workspace", "access": "read"}]},
        "environments": {"environments": [{"environmentId": environment, "default": True, "access": "exec"}]},
    }}
    if model:
        config["model"] = model
    await client.create_profile(profile={"profileId": profile, "displayName": "Streamlit internals", "config": config})
    return model


async def update_profile_model(
    client: AsyncLightspeedClient, profile: AgentProfile, args: argparse.Namespace, model: ModelConfig | None,
) -> ModelConfig | None:
    config = profile.get("config") or {}
    features = config.get("features") or {}
    workspaces = (features.get("vfs") or {}).get("workspaces", [])
    environments = (features.get("environments") or {}).get("environments", [])
    default = next((
        item for item in environments
        if item.get("default") and item.get("environmentId") and item.get("access") in {"exec", "jobs"}
    ), None)
    if not workspaces or default is None:
        raise SetupError(
            f"Profile {profile['profileId']} needs a VFS attachment and a default command-capable environment. "
            "Update it in Lightspeed or pass another --profile."
        )
    environment_id = default.get("environmentId")
    if args.environment_id and args.environment_id != environment_id:
        raise SetupError("The existing profile uses another environment. Pass another --profile.")
    if args.envd_endpoint and environment_id:
        environment = (await client.read_environment(environment_id))["environment"]
        source = environment["source"]
        if source["type"] != "external" or source["endpoint"] != args.envd_endpoint:
            raise SetupError("The existing profile uses another daemon endpoint. Pass another --profile.")
    current = config.get("model")
    if model and model != current:
        config = {**config, "model": model}
        replacement: AgentProfileInput = {
            "profileId": profile["profileId"], "config": config,
            "displayName": profile.get("displayName"), "description": profile.get("description"),
            "instructions": profile.get("instructions"), "retention": profile.get("retention"),
            "metadata": profile.get("metadata", {}),
        }
        await client.put_profile(expected_revision=profile["revision"], profile=replacement)
        return model
    return current


async def initialize(args: argparse.Namespace) -> None:
    values = dotenv_values(args.env_file, interpolate=False)
    handoff = launcher_handoff()
    configured_gateway = values.get("LIGHTSPEED_API_URL") or DEFAULT_GATEWAY
    endpoint = gateway_url(
        args.gateway or os.environ.get("LIGHTSPEED_API_URL") or values.get("LIGHTSPEED_API_URL")
        or handoff.get("endpoint") or DEFAULT_GATEWAY,
    )
    launcher = endpoint == handoff.get("endpoint") and urlsplit(endpoint).hostname in {"127.0.0.1", "::1", "localhost"}
    admin_key = os.environ.get("LIGHTSPEED_DEMO_ADMIN_API_KEY")
    if not admin_key and launcher and handoff.get("credentialFile"):
        admin_key = private_read(Path(handoff["credentialFile"]))
    if not admin_key:
        raise SetupError("Start authenticated ./dev.sh or set LIGHTSPEED_DEMO_ADMIN_API_KEY for this gateway.")

    model = model_override(args)
    profile = args.profile or os.environ.get("LIGHTSPEED_PROFILE_ID") or values.get("LIGHTSPEED_PROFILE_ID") or "internals-demo"
    app_key = values.get("LIGHTSPEED_API_KEY") if endpoint == configured_gateway else None
    access = await read_access(endpoint, app_key or "")
    selector = args.universe or os.environ.get("LIGHTSPEED_UNIVERSE")
    if not selector and endpoint == configured_gateway:
        selector = values.get("LIGHTSPEED_UNIVERSE")
    if not selector and access and access["scope"]["kind"] == "universe":
        selector = access["scope"]["universeId"]
    if not selector and launcher:
        selector = "test"
    if not selector:
        raise SetupError("Pass --universe to select the Lightspeed universe to configure.")

    async with AsyncLightspeedClient(endpoint, api_key=admin_key) as admin:
        if (await admin.initialize())["caller"]["scope"]["kind"] != "deployment":
            raise SetupError("Initialization requires a deployment API key.")
        universe = await resolve_universe(admin, selector)
        async with AsyncLightspeedClient(endpoint, api_key=admin_key, universe=universe) as client:
            if model:
                await ensure_model_provider(client, model)
            effective_model = await ensure_profile(client, profile, args, launcher, model)
            if effective_model is None:
                effective_model = (await client.read_model_defaults())["defaults"].get("agentRun")
        key_message = await ensure_app_key(admin, endpoint, universe, access)

    print(f"Configured Lightspeed universe {universe} on {endpoint}.")
    print(f"Profile: {profile}")
    if effective_model:
        print(f"Model route: {effective_model['providerId']} / {effective_model['model']}")
    else:
        print("Select an agent-run model in Lightspeed or rerun with the --model options.")
    print(key_message)
    print("Use this universe's scoped key and profile in the Streamlit connection settings.")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--env-file", type=Path, default=EXAMPLE / ".env", help="Read existing connection settings from this file")
    parser.add_argument("--gateway", help="Gateway /rpc URL")
    parser.add_argument("--universe", help="Existing Lightspeed universe UUID or slug")
    parser.add_argument("--profile", help="Demo profile ID")
    parser.add_argument("--model-provider", help="Model provider ID, for example openai")
    parser.add_argument("--model-api-kind", help="Model API kind, for example openai:responses")
    parser.add_argument("--model", help="Model name for the demo profile")
    environment = parser.add_mutually_exclusive_group()
    environment.add_argument("--environment-id", help="Attach an existing environment")
    environment.add_argument("--envd-endpoint", help="Register a direct daemon endpoint reachable from the gateway")
    os.umask(0o077)
    try:
        asyncio.run(initialize(parser.parse_args()))
    except LightspeedRpcError as error:
        sys.exit(f"Lightspeed rejected setup ({error.kind}): {error.message}")
    except LightspeedTransportError as error:
        sys.exit(f"Could not reach the Lightspeed gateway: {error}")
    except BlockingIOError:
        sys.exit("Another initialization is running for this universe.")
    except (SetupError, OSError, ValueError) as error:
        sys.exit(str(error))


if __name__ == "__main__":
    main()

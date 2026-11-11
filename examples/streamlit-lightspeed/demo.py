"""Local gateway and infrastructure readers for the Streamlit example."""

from __future__ import annotations

import asyncio
import json
import os
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from datetime import timedelta
from pathlib import Path
from time import perf_counter, time_ns
from typing import Any
from uuid import uuid4

import httpx
import psycopg
from dotenv import dotenv_values
from psycopg.rows import dict_row
from temporalio.api.enums.v1 import EventType
from temporalio.client import Client as TemporalClient

from lightspeed_ai import AsyncLightspeedClient, generate_submission_id
from lightspeed_ai.types import EventCursor, RunView, SessionEventView, SessionView

from local_admin import PLATFORM_URL

TERMINAL = {"completed", "failed", "cancelled"}
MAX_TURNS = 12
MAX_EVENTS = 250
MAX_RPCS = 120
MAX_PREVIEW_BYTES = 16_384


@dataclass(frozen=True)
class Settings:
    endpoint: str
    api_key: str | None = field(repr=False)
    universe: str | None = None
    profile: str | None = None
    platform_url: str | None = None
    temporal_address: str | None = None
    temporal_namespace: str = "default"
    temporal_ui: str | None = None
    postgres_dsn: str | None = field(default=None, repr=False)

    @classmethod
    def load(cls, directory: Path) -> Settings:
        env = dotenv_values(directory / ".env", interpolate=False)

        def value(name: str, default: str = "") -> str | None:
            return (os.environ.get(name, env.get(name, default)) or "").strip() or None

        return cls(
            endpoint=value("LIGHTSPEED_API_URL") or "http://127.0.0.1:18080/rpc",
            api_key=value("LIGHTSPEED_API_KEY"),
            universe=value("LIGHTSPEED_UNIVERSE"),
            profile=value("LIGHTSPEED_PROFILE_ID"),
            platform_url=value("LIGHTSPEED_DEMO_PLATFORM_URL", PLATFORM_URL),
            temporal_address=value("LIGHTSPEED_DEMO_TEMPORAL_ADDRESS"),
            temporal_namespace=value("LIGHTSPEED_DEMO_TEMPORAL_NAMESPACE") or "default",
            temporal_ui=value("LIGHTSPEED_DEMO_TEMPORAL_UI"),
            postgres_dsn=value("LIGHTSPEED_DEMO_POSTGRES_DSN"),
        )


@dataclass(frozen=True)
class Rpc:
    method: str
    request_id: str | int
    latency_ms: float
    outcome: str
    observed_at_ms: int


@dataclass
class Turn:
    prompt: str
    submission_id: str = field(default_factory=generate_submission_id)
    run_id: str | None = None
    status: str = "submitting"
    cursor: EventCursor | None = None
    events: list[SessionEventView] = field(default_factory=list)
    calls: list[Rpc] = field(default_factory=list)
    detail: RunView | None = None
    error: str | None = None
    can_retry: bool = False
    observation_paused: bool = False

    def observe(self, event: SessionEventView) -> None:
        if self.events and event["cursor"]["seq"] <= self.events[-1]["cursor"]["seq"]:
            return
        self.events.append(event)
        del self.events[:-MAX_EVENTS]
        match event["kind"]["type"]:
            case "runAccepted":
                self.status = "queued"
            case "runStarted" | "approvalRunUnparked":
                self.status = "running"
            case "approvalRunParked":
                self.status = "waiting for approval"

    def save_cursor(self, cursor: EventCursor | None, _page: object) -> None:
        self.cursor = cursor

    @property
    def active(self) -> bool:
        return self.run_id is not None and self.status not in TERMINAL

    @property
    def observing(self) -> bool:
        return self.active and not self.observation_paused


@dataclass
class Chat:
    session_id: str = field(default_factory=lambda: f"streamlit-{uuid4().hex}")
    opened: bool = False
    cursor: EventCursor | None = None
    turns: list[Turn] = field(default_factory=list)

    @property
    def latest(self) -> Turn | None:
        return self.turns[-1] if self.turns else None


class TimingTransport(httpx.AsyncBaseTransport):
    """Record only request metadata. Read the response so timing covers its body."""

    def __init__(self, calls: list[Rpc]) -> None:
        self.calls = calls
        self.transport = httpx.AsyncHTTPTransport()

    async def handle_async_request(self, request: httpx.Request) -> httpx.Response:
        rpc = json.loads(request.content)
        start = perf_counter()
        outcome = "Cancelled"
        try:
            response = await self.transport.handle_async_request(request)
            try:
                await response.aread()
                outcome = f"HTTP {response.status_code}"
                if response.is_success:
                    body = response.json()
                    if isinstance(body, dict) and isinstance(body.get("error"), dict):
                        error = body["error"]
                        data = error.get("data")
                        kind = data.get("kind") if isinstance(data, dict) else None
                        outcome = f"RPC {kind or error.get('code', 'error')}"
            except ValueError:
                outcome = "Invalid JSON"
            finally:
                await response.aclose()
            return response
        except httpx.RequestError as error:
            outcome = type(error).__name__
            raise
        finally:
            self.calls.append(Rpc(
                rpc["method"], rpc["id"], (perf_counter() - start) * 1000, outcome, time_ns() // 1_000_000,
            ))
            del self.calls[:-MAX_RPCS]

    async def aclose(self) -> None:
        await self.transport.aclose()


class Gateway:
    def __init__(self, settings: Settings) -> None:
        self.settings = settings

    def execute[T](
        self,
        operation: Callable[[AsyncLightspeedClient], Awaitable[T]],
        calls: list[Rpc] | None = None,
    ) -> T:
        async def invoke() -> T:
            transport = TimingTransport(calls) if calls is not None else None
            async with httpx.AsyncClient(transport=transport, timeout=8.0, trust_env=False) as http:
                async with AsyncLightspeedClient(
                    self.settings.endpoint, api_key=self.settings.api_key, universe=self.settings.universe,
                    request_id=lambda: uuid4().hex, http_client=http,
                ) as client:
                    return await operation(client)

        return asyncio.run(invoke())

    def identify_universe(self) -> str | None:
        caller = self.execute(lambda client: client.initialize())["caller"]["scope"]
        if caller["kind"] == "universe":
            return caller["universeId"]
        return self.settings.universe

    def submit(self, chat: Chat, turn: Turn) -> None:
        async def submit(client: AsyncLightspeedClient) -> None:
            if not chat.opened:
                if self.settings.profile:
                    await client.start_session(
                        session_id=chat.session_id, display_name="Streamlit internals",
                        profile={"kind": "named", "profileId": self.settings.profile},
                    )
                else:
                    await client.start_session(session_id=chat.session_id, display_name="Streamlit internals")
                chat.opened = True
            result = await client.start_run(chat.session_id, turn.prompt, submission_id=turn.submission_id)
            turn.run_id = result["run"]["id"]
            status = result["run"]["status"]
            # Even a terminal admission needs a full run read for the reply.
            turn.status = "finishing" if status in TERMINAL else status

        self.execute(submit, turn.calls)

    def poll(self, chat: Chat, turn: Turn) -> None:
        run_id = turn.run_id
        if run_id is None:
            return

        async def follow(client: AsyncLightspeedClient) -> None:
            try:
                async with asyncio.timeout(1.5):
                    result = await client.await_run(
                        chat.session_id, run_id, after=turn.cursor, wait_ms=1000,
                        on_event=turn.observe, heartbeat=turn.save_cursor,
                    )
            except TimeoutError:
                return
            turn.detail = result["run"]
            turn.status = turn.detail["status"]
            turn.cursor = result["cursor"]

        try:
            self.execute(follow, turn.calls)
        finally:
            chat.cursor = turn.cursor

    def cancel(self, chat: Chat, turn: Turn) -> None:
        if turn.run_id is not None:
            run_id = turn.run_id
            self.execute(lambda client: client.cancel_run(chat.session_id, run_id), turn.calls)

    def open_session(self, session_id: str) -> Chat:
        async def load(client: AsyncLightspeedClient) -> Chat:
            await client.read_session(session_id)
            runs = (await client.list_runs(session_id, limit=MAX_TURNS)).get("runs", [])
            history = await client.call("session/events/read", {
                "sessionId": session_id, "direction": "backward", "limit": MAX_EVENTS,
            })
            events = history["result"].get("events", [])
            cursor = events[-1]["cursor"] if events else None
            turns = []
            for item in reversed(runs):
                run = (await client.read_run(session_id, item["id"]))["run"]
                prompt = "\n".join(
                    part["text"] for part in run["source"]["items"] if part["type"] == "text"
                ) or "(Non-text input)"
                turns.append(Turn(
                    prompt, run_id=run["id"], status=run["status"], cursor=cursor, detail=run,
                    events=[event for event in events if event["joins"].get("runId") == run["id"]],
                ))
            return Chat(session_id, opened=True, cursor=cursor, turns=turns)

        return self.execute(load)

    def session(self, session_id: str) -> SessionView:
        return self.execute(lambda client: client.read_session(session_id))["session"]

    def workspace(self, attachment: dict) -> tuple[dict, dict]:
        async def load(client: AsyncLightspeedClient) -> tuple[dict, dict]:
            if workspace_id := attachment.get("workspaceId"):
                head = (await client.read_workspace(workspace_id))["workspace"]
                snapshot_ref = head["headSnapshotRef"]
            else:
                snapshot_ref = attachment["snapshotRef"]
                head = {"headSnapshotRef": snapshot_ref}
            response = await client.call("vfs/snapshots/read", {"snapshotRef": snapshot_ref})
            return dict(head), response["result"]["manifest"]

        return self.execute(load)

    def preview(self, blob_ref: str) -> str:
        async def load(client: AsyncLightspeedClient) -> str:
            data = await client.read_blob_bytes(blob_ref)
            if len(data) > MAX_PREVIEW_BYTES:
                raise ValueError("File exceeds the text preview limit")
            return data.decode("utf-8")

        return self.execute(load)

    def environments(self, session: SessionView) -> list[dict]:
        attachments = (session.get("config") or {}).get("features", {}).get("environments") or {}
        ids = {item["environmentId"] for item in attachments.get("environments", []) if item.get("environmentId")}
        if active := session.get("activeEnvironmentId"):
            ids.add(active)

        async def load(client: AsyncLightspeedClient) -> list[dict]:
            environments = []
            for environment_id in sorted(ids)[:12]:
                result = await client.read_environment(environment_id)
                environment = result["environment"]
                environments.append({
                    "id": environment_id, "active": environment_id == active,
                    "name": environment.get("displayName"), "state": environment["state"],
                    "status": environment["status"], "route": environment["source"]["type"],
                    "connected": environment["source"].get("connected"),
                    "daemon": environment.get("daemon"),
                })
            return environments

        return self.execute(load)


def workspace_files(manifest: dict) -> list[dict]:
    if manifest.get("schema_version") != "lightspeed.vfs.snapshot.v1":
        raise ValueError("Unrecognized VFS snapshot manifest")
    files: list[dict] = []
    pending = [("", manifest["root"]["entries"])]
    while pending and len(files) < 100:
        prefix, entries = pending.pop()
        for name, item in sorted(entries.items()):
            path = f"{prefix}{name}"
            if item["kind"] == "directory":
                pending.append((f"{path}/", item["entries"]))
            elif item["kind"] == "file":
                files.append({
                    "path": path, "bytes": item["size_bytes"], "ref": item["blob_ref"],
                    "media": item.get("media_type"),
                })
                if len(files) == 100:
                    break
    return sorted(files, key=lambda item: item["path"])


async def temporal_history(settings: Settings, workflow_id: str) -> list[dict]:
    async with asyncio.timeout(4):
        client = await TemporalClient.connect(settings.temporal_address or "", namespace=settings.temporal_namespace)
        handle = client.get_workflow_handle(workflow_id)
        history = handle.fetch_history_events(page_size=100, rpc_timeout=timedelta(seconds=3))
        scheduled: dict[int, str] = {}
        rows = []
        outcomes = {
            EventType.EVENT_TYPE_ACTIVITY_TASK_STARTED: "activity_task_started_event_attributes",
            EventType.EVENT_TYPE_ACTIVITY_TASK_COMPLETED: "activity_task_completed_event_attributes",
            EventType.EVENT_TYPE_ACTIVITY_TASK_FAILED: "activity_task_failed_event_attributes",
            EventType.EVENT_TYPE_ACTIVITY_TASK_TIMED_OUT: "activity_task_timed_out_event_attributes",
            EventType.EVENT_TYPE_ACTIVITY_TASK_CANCELED: "activity_task_canceled_event_attributes",
        }
        async for event in history:
            activity = None
            if event.event_type == EventType.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED:
                activity = event.activity_task_scheduled_event_attributes.activity_type.name
                scheduled[event.event_id] = activity
            elif attribute := outcomes.get(event.event_type):
                activity = scheduled.get(getattr(event, attribute).scheduled_event_id)
            if activity:
                rows.append({
                    "event": event.event_id, "time": event.event_time.ToDatetime().isoformat() + "Z",
                    "activity": activity,
                    "state": EventType.Name(event.event_type).removeprefix("EVENT_TYPE_ACTIVITY_TASK_").lower(),
                })
            if event.event_id >= 1000:
                break
        return rows[-100:]


async def postgres_state(settings: Settings, universe: str, session_id: str) -> dict[str, Any]:
    async with asyncio.timeout(4):
        async with await psycopg.AsyncConnection.connect(
            settings.postgres_dsn or "", row_factory=dict_row, connect_timeout=3,
        ) as conn:
            await conn.set_read_only(True)
            await conn.execute("SET LOCAL statement_timeout = '2000ms'")
            params = (universe, session_id)
            catalog = await conn.execute(
                "SELECT session_id, lifecycle_status, head_seq, created_at_ms, updated_at_ms "
                "FROM public.sessions WHERE universe_id = %s AND session_id = %s", params,
            )
            events = await conn.execute(
                "SELECT seq, event_kind, observed_at_ms FROM public.session_events "
                "WHERE universe_id = %s AND session_id = %s ORDER BY seq DESC LIMIT 50", params,
            )
            checkpoint = await conn.execute(
                "SELECT through_seq, format_version, byte_len, created_at_ms FROM public.session_checkpoints "
                "WHERE universe_id = %s AND session_id = %s", params,
            )
            return {
                "session": await catalog.fetchone(),
                "events": await events.fetchall(),
                "checkpoint": await checkpoint.fetchone(),
            }

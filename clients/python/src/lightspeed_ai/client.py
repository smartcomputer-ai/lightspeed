from __future__ import annotations

import asyncio
import base64
import inspect
import json
import os
import re
from collections.abc import AsyncIterator, Awaitable, Callable
from contextlib import asynccontextmanager
from copy import deepcopy
from urllib.parse import quote, urlencode
from types import TracebackType
from typing import Any, Self, TypedDict, cast
from uuid import uuid4

import httpx

from ._platform import PlatformApi, PLATFORM_OPERATIONS
from ._endpoint import normalize_endpoint
from ._generated import OPERATION_INFO, GeneratedClient
from ._http import HTTP_OPERATIONS
from ._options import UNSET, HeaderProvider, HeadersInput, Unset
from .errors import LightspeedApiError, LightspeedTransportError
from .types import (
    AgentNotification,
    UniverseWatchParams,
    BlobPutParams,
    BlobPutResult,
    EventCursor,
    InputItem,
    RunStartConfig,
    RunStartSource,
    RunTerminalNotificationInput,
    RunStartParams,
    RunStartResponse,
    RunSteerParams,
    RunSteerResponse,
    RunView,
    SessionEventsReadParams,
    SessionEventDirection,
    SessionEventsReadResponse,
    SessionEventView,
)

type Callback[**P] = Callable[P, None | Awaitable[None]]

TERMINAL_STATUSES = frozenset(("completed", "failed", "cancelled"))
"""The run statuses that never change again."""
_TERMINAL_EVENTS = frozenset(("runCompleted", "runFailed", "runCancelled"))
_WAIT_MS = 30_000
_WAIT_MIN_MS = 1_000
_WAIT_REQUEST_GRACE = 30.0
_WAIT_READ_ATTEMPTS = 6
_WAIT_RETRY_BASE = 1.0


class AwaitRunResult(TypedDict):
    """What `await_run` returns."""

    run: RunView
    """The finished run, as `read_run` returns it. Its status is one of `TERMINAL_STATUSES`."""
    cursor: EventCursor | None
    """Where to resume reading the session's events. None if none were read."""


def generate_submission_id() -> str:
    """A fresh submission ID. Keep the one you send: a retry needs it to find the same run."""
    return str(uuid4())


def _input_items(value: str | list[InputItem]) -> list[InputItem]:
    if isinstance(value, str):
        return [{"type": "text", "text": value}]
    return value


def _prepare_request(operation: str, params: dict[str, Any], endpoint: str | httpx.URL, universe: str | None) -> tuple[str, str, dict[str, Any] | None]:
    binding = HTTP_OPERATIONS[operation]
    remaining = deepcopy(params)
    path = binding["path"]
    def segment(value: Any, name: str) -> str:
        if type(value) is int:
            return str(value)
        if not isinstance(value, str) or not value or value in (".", ".."):
            raise ValueError(f"{name} must be a nonempty path identifier")
        return quote(value, safe="")
    for field in binding["pathFields"]:
        owner = remaining
        parts = field["field"].split(".")
        for part in parts[:-1]:
            owner = owner.get(part, {})
        value = owner.pop(parts[-1], None)
        path = path.replace("{" + field["parameter"] + "}", segment(value, field["field"]))
    if "{universe}" in path:
        path = path.replace("{universe}", segment(universe, "universe"))
    base = normalize_endpoint(endpoint)
    query: list[tuple[str, str]] = []
    for parameter in binding["parameters"]:
        if parameter["in"] != "query":
            continue
        value = remaining.pop(parameter["name"], None)
        if value is None:
            continue
        encoding = parameter.get("x-lightspeed-encoding")
        if encoding == "repeated":
            if not isinstance(value, list):
                raise ValueError(f"{parameter['name']} must be an array")
            query.extend((parameter["name"], str(item).lower() if isinstance(item, bool) else str(item)) for item in value)
        elif encoding == "json":
            query.append((parameter["name"], json.dumps(value, separators=(",", ":"), ensure_ascii=False)))
        else:
            query.append((parameter["name"], str(value).lower() if isinstance(value, bool) else str(value)))
    body = None if binding["method"] in ("GET", "HEAD") or binding["transfer"] == "binaryUpload" else remaining
    if body is None and remaining:
        raise ValueError(f"Unexpected parameters for {operation}: {', '.join(remaining)}")
    url = base + path.removeprefix("/api/v1")
    return binding["method"], url + ("?" + urlencode(query) if query else ""), body


async def _retry_read[T](read: Callable[[], Awaitable[T]], deadline: float) -> T:
    for attempt in range(_WAIT_READ_ATTEMPTS):
        try:
            async with asyncio.timeout(deadline):
                return await read()
        except (TimeoutError, LightspeedTransportError) as error:
            if isinstance(error, LightspeedTransportError) and not error.transient:
                raise
            if attempt == _WAIT_READ_ATTEMPTS - 1:
                raise
            await asyncio.sleep(_WAIT_RETRY_BASE * 2**attempt)
    raise AssertionError("read attempts must be positive")


async def _notify[**P](callback: Callback[P] | None, *args: P.args, **kwargs: P.kwargs) -> None:
    if callback is not None:
        result = callback(*args, **kwargs)
        if inspect.isawaitable(result):
            await result


class AsyncLightspeedClient(GeneratedClient):
    """A client for the Lightspeed API.

    `call` and named helpers return direct response DTOs. Helpers take
    the IDs of their target as positional arguments.

    Use it as an async context manager, or call `aclose` when done, to release
    its connections. An `http_client` you pass in stays yours to close.

    Args:
        endpoint: A runtime or Platform origin, deployment base path, or `/api/v1` URL.
        token: Sent as the bearer credential. Omit it only for a gateway in
            single mode.
        universe: The universe a request addresses, as its UUID or its handle.
        actor: Attributes requests to this opaque ID. Direct runtime tokens may
            always name an actor; attribution does not restrict access. See `with_actor`.
        headers: Extra headers, or a function returning them for each request,
            which may be async. These override the options above.
        http_client: An `httpx.AsyncClient` to send with, for custom
            transports, timeouts, or tests. Without one the client makes its
            own, with a 60 second timeout.
    """

    def __init__(
        self,
        endpoint: str | httpx.URL,
        *,
        token: str | None = None,
        universe: str | None = None,
        actor: str | None = None,
        headers: HeadersInput | HeaderProvider | None = None,
        http_client: httpx.AsyncClient | None = None,
    ) -> None:
        self._endpoint = normalize_endpoint(endpoint)
        self._token = token
        self._universe = universe
        self._actor = actor
        self._headers = headers
        self._http_client = http_client if http_client is not None else httpx.AsyncClient(timeout=60.0)
        self._owns_client = http_client is None
        self._credential_provider: Callable[[], str | None] | None = None
        self._response_hook: Callable[[httpx.Request, httpx.Response], Awaitable[None]] | None = None
        self.account_id: str | None = None
        self.platform = PlatformApi(self._invoke_platform)

    @classmethod
    async def from_connection(
        cls, name: str | None = None, *, config_dir: str | os.PathLike[str] | None = None,
        universe: str | None = None, account: str | None = None,
        http_client: httpx.AsyncClient | None = None,
    ) -> Self:
        """Import and verify a named or selected CLI connection.

        Uses only that connection's pinned endpoint and credential file. It never
        starts browser login or reads ambient token/endpoint overrides. Account
        selection is available as `account_id` for explicit native API parameters.
        """
        from ._connection import load_connection
        imported = load_connection(name, config_dir, universe, account)
        client = cls(imported.endpoint, universe=imported.universe, http_client=http_client)
        client._credential_provider = imported.credential
        client.account_id = imported.account_id
        initial_response: tuple[httpx.Request, httpx.Response] | None = None
        async def collect(request: httpx.Request, response: httpx.Response) -> None:
            nonlocal initial_response
            initial_response = (request, response)
        client._response_hook = collect
        try:
            discovery = await client._invoke("initialize", {})
            if not isinstance(discovery, dict):
                raise ValueError("Invalid deployment discovery response")
            imported.verify(discovery)
            if initial_response is not None:
                await imported.accept_response(*initial_response)
            client._response_hook = imported.accept_response
            if client._universe is None and not (imported.server_kind == "platform" and imported.account_id is not None):
                default = discovery.get("defaultUniverse")
                if isinstance(default, str) and default:
                    client._universe = default
            return client
        except BaseException:
            await client.aclose()
            raise

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self, exc_type: type[BaseException] | None, exc: BaseException | None, traceback: TracebackType | None
    ) -> None:
        await self.aclose()

    async def aclose(self) -> None:
        """Close the HTTP client, unless the caller supplied it."""
        if self._owns_client:
            await self._http_client.aclose()

    def with_actor(self, actor: str) -> AsyncLightspeedClient:
        """A client like this one whose requests are attributed to `actor`.

        It shares this client's connections, so closing it closes nothing.
        """
        client = AsyncLightspeedClient(
            self._endpoint, token=self._token, universe=self._universe, actor=actor,
            headers=self._headers, http_client=self._http_client,
        )
        client._credential_provider = self._credential_provider
        client._response_hook = self._response_hook
        client.account_id = self.account_id
        return client

    async def _request_headers(self, headers: HeadersInput | None = None) -> httpx.Headers:
        request_headers = httpx.Headers({"accept": "application/json"})
        token = self._credential_provider() if self._credential_provider is not None else self._token
        if token is not None:
            request_headers["authorization"] = f"Bearer {token}"
        if self._actor is not None:
            request_headers["x-lightspeed-actor"] = self._actor
        defaults = self._headers() if callable(self._headers) else self._headers
        if inspect.isawaitable(defaults):
            defaults = await defaults
        if defaults is not None:
            request_headers.update(defaults)
        if headers is not None:
            request_headers.update(headers)
        if "x-lightspeed-universe" in request_headers:
            raise ValueError("Select the universe with the client universe option")
        return request_headers

    async def _build_request(
        self, operation: str, params: dict[str, Any], headers: HeadersInput | None = None,
        body: bytes | AsyncIterator[bytes] | None = None, content_length: int | None = None,
    ) -> httpx.Request:
        method, url, data = _prepare_request(operation, params, self._endpoint, self._universe)
        request_headers = await self._request_headers(headers)
        transfer = HTTP_OPERATIONS[operation]["transfer"]
        request_headers["accept"] = {"sse": "text/event-stream", "binaryDownload": "application/octet-stream"}.get(transfer, "application/json")
        if transfer == "binaryUpload":
            if body is None:
                raise ValueError("A binary upload requires body bytes or a stream")
            length = len(body) if isinstance(body, bytes) else content_length
            if type(length) is not int or length < 0:
                raise ValueError("A binary upload requires a valid content_length")
            request_headers["content-type"] = "application/octet-stream"
            request_headers["content-length"] = str(length)
            return self._http_client.build_request(method, url, headers=request_headers, content=body)
        return self._http_client.build_request(method, url, headers=request_headers, json=data)

    async def _send(self, request: httpx.Request, *, stream: bool = False) -> httpx.Response:
        response = await self._http_client.send(request, stream=stream, follow_redirects=False)
        try:
            if response.is_redirect or response.history or response.url != request.url:
                raise LightspeedTransportError("Deployment API redirects are not allowed", status=response.status_code)
            if self._response_hook is not None:
                await self._response_hook(request, response)
            return response
        except BaseException:
            await response.aclose()
            raise

    @staticmethod
    def _response_json(response: httpx.Response) -> Any:
        try:
            body = response.json() if response.content else None
        except ValueError as error:
            if response.is_success:
                raise LightspeedTransportError("Lightspeed API response was not valid JSON", status=response.status_code, body=response.text) from error
            body = response.text
        if not response.is_success:
            if (isinstance(body, dict) and body.get("status") == response.status_code
                    and all(isinstance(body.get(key), str) for key in ("type", "title", "detail", "kind"))):
                raise LightspeedApiError(body)
            raise LightspeedTransportError(f"Lightspeed API request failed with HTTP {response.status_code}", status=response.status_code, body=body)
        return body

    async def _invoke(
        self, method: str, params: dict[str, Any], headers: HeadersInput | None = None,
        body: bytes | AsyncIterator[bytes] | None = None, content_length: int | None = None,
    ) -> Any:
        if method in PLATFORM_OPERATIONS:
            if body is not None or content_length is not None:
                raise ValueError("Native Platform operations require JSON parameters")
            return await self._invoke_platform(method, params, headers)
        if method not in OPERATION_INFO:
            raise ValueError(f"Unknown Lightspeed operation {method!r}")
        if OPERATION_INFO[method]["streaming"]:
            raise ValueError("Use watch() for universe/watch")
        try:
            request = await self._build_request(method, params, headers, body, content_length)
            response = await self._send(request)
        except httpx.RequestError as error:
            raise LightspeedTransportError(f"Lightspeed API request failed: {error}", transient=True) from error
        if response.is_success and HTTP_OPERATIONS[method]["transfer"] == "binaryDownload":
            return response.content
        return self._response_json(response)

    async def _invoke_platform(self, operation: str, params: dict[str, Any], headers: HeadersInput | None = None) -> Any:
        if operation not in PLATFORM_OPERATIONS:
            raise ValueError(f"Unknown Platform operation {operation!r}")
        spec = PLATFORM_OPERATIONS[operation]
        body = dict(params)
        def segment(match: re.Match[str]) -> str:
            value = body.pop(match.group(1), None)
            if not isinstance(value, str) or not value or value in (".", ".."):
                raise ValueError(f"Invalid path parameter {match.group(1)}")
            return quote(value, safe="")
        path = re.sub(r"\{([^}]+)\}", segment, spec["path"])
        if spec["method"] == "GET" and body:
            raise ValueError("Unexpected query parameters")
        url = str(self._endpoint).rstrip("/") + path.removeprefix("/api/v1")
        try:
            request = self._http_client.build_request(spec["method"], url, headers=await self._request_headers(headers), json=body if body else None)
            response = await self._send(request)
        except httpx.RequestError as error:
            raise LightspeedTransportError(f"Platform API request failed: {error}", transient=True) from error
        return self._response_json(response)

    @asynccontextmanager
    async def open_download(self, operation: str, params: dict[str, Any], *, headers: HeadersInput | None = None) -> AsyncIterator[httpx.Response]:
        """Stream a binary response; the context manager always closes it."""
        if HTTP_OPERATIONS[operation]["transfer"] != "binaryDownload":
            raise ValueError("open_download requires a binary download operation")
        response = None
        try:
            request = await self._build_request(operation, params, headers)
            response = await self._send(request, stream=True)
            if not response.is_success:
                await response.aread()
                self._response_json(response)
            yield response
        except httpx.RequestError as error:
            raise LightspeedTransportError("Download interrupted", transient=True) from error
        finally:
            if response is not None:
                await response.aclose()

    async def upload_blob(self, blob_ref: str, body: bytes | AsyncIterator[bytes], *, content_length: int | None = None, headers: HeadersInput | None = None) -> BlobPutResult:
        """Upload bytes or a stream with a declared length to the expected digest."""
        return await self.call("blobs/upload", {"blobRef": blob_ref}, body=body, content_length=content_length, headers=headers)

    async def watch(
        self, params: UniverseWatchParams | None = None, *,
        headers: HeadersInput | None = None, reconnect: bool = True,
    ) -> AsyncIterator[AgentNotification]:
        """Watch list rows. Refetch on watch/reset and periodically; there is no resume cursor.

        Reconnects on stream completion and transient transport errors. Cancel
        the task or close the iterator to release the connection.
        """
        attempts = 0
        while True:
            response = None
            try:
                request = await self._build_request("universe/watch", dict(params or {}), headers)
                request.extensions["timeout"] = {"connect": 15.0, "read": 45.0, "write": 15.0, "pool": 15.0}
                response = await self._send(request, stream=True)
                if not response.is_success:
                    await response.aread()
                    self._response_json(response)
                if not response.headers.get("content-type", "").startswith("text/event-stream"):
                    raise LightspeedTransportError("Watch response was not an event stream", status=response.status_code)
                if response:
                    event = "message"
                    data: list[str] = []
                    size = 0
                    async for line in response.aiter_lines():
                        if line == "":
                            if not data:
                                continue
                            try:
                                message = json.loads("\n".join(data))
                            except ValueError as error:
                                raise LightspeedTransportError("Invalid watch event") from error
                            data = []
                            size = 0
                            if not isinstance(message, dict):
                                raise LightspeedTransportError("Invalid watch event")
                            if event == "watch/end":
                                break
                            if event not in cast(list[str], OPERATION_INFO["universe/watch"]["notifications"]):
                                raise LightspeedTransportError("Unknown watch notification")
                            attempts = 0
                            yield cast(AgentNotification, {"method": event, "params": message})
                            event = "message"
                        elif line.startswith("event:"):
                            event = line[6:].removeprefix(" ") or "message"
                        elif line == "data" or line.startswith("data:"):
                            field = line[5:] if line.startswith("data:") else ""
                            if field.startswith(" "):
                                field = field[1:]
                            data.append(field)
                            size += len(field)
                        if size + len(line) > 2 * 1024 * 1024:
                            raise LightspeedTransportError("Watch event exceeded its size limit")
            except httpx.RequestError as error:
                if not reconnect:
                    raise LightspeedTransportError("Watch stream interrupted", transient=True) from error
            except LightspeedTransportError as error:
                if not reconnect or not error.transient:
                    raise
            finally:
                if response is not None:
                    await response.aclose()
            if not reconnect:
                return
            await asyncio.sleep(min(30.0, 2.0 ** min(attempts, 5)))
            attempts += 1

    async def start_run(
        self,
        session_id: str,
        input: str | list[InputItem] | RunStartSource,
        *,
        submission_id: str | None | Unset = UNSET,
        config: RunStartConfig | None | Unset = UNSET,
        notify_on_terminal: RunTerminalNotificationInput | None | Unset = UNSET,
        headers: HeadersInput | None = None,
    ) -> RunStartResponse:
        """Start a run from plain text, input items, or a complete RunStartSource.

        A submission ID is generated if you give none, but only one you keep
        lets a retry find the same run after a lost response. Repeat a retry
        with the same input.
        """
        params: RunStartParams = {
            "sessionId": session_id,
            "source": input if isinstance(input, dict) else {"type": "input", "items": _input_items(input)},
            "submissionId": generate_submission_id() if isinstance(submission_id, Unset) else submission_id,
        }
        if not isinstance(config, Unset):
            params["config"] = config
        if not isinstance(notify_on_terminal, Unset):
            params["notifyOnTerminal"] = notify_on_terminal
        return await self.call("session/runs/start", params, headers=headers)

    async def steer_run(
        self,
        session_id: str,
        run_id: str,
        input: str | list[InputItem],
        *,
        submission_id: str | None | Unset = UNSET,
        headers: HeadersInput | None = None,
    ) -> RunSteerResponse:
        """Send input to a run in progress, which its next model turn receives."""
        params: RunSteerParams = {
            "sessionId": session_id,
            "runId": run_id,
            "items": _input_items(input),
            "submissionId": generate_submission_id() if isinstance(submission_id, Unset) else submission_id,
        }
        return await self.call("session/runs/steer", params, headers=headers)

    async def read_events(
        self,
        session_id: str,
        *,
        after: EventCursor | None | Unset = UNSET,
        before: EventCursor | None | Unset = UNSET,
        direction: SessionEventDirection | Unset = UNSET,
        limit: int | None | Unset = UNSET,
        wait_ms: int | None | Unset = UNSET,
        headers: HeadersInput | None = None,
    ) -> SessionEventsReadResponse:
        """Read one page of durable session events with forward or backward pagination.

        Args:
            after: Read events after this cursor. Without it, from the beginning.
            before: Exclusive upper cursor for backward reads.
            direction: Forward from after, or the latest window before before.
            limit: The most events to return.
            wait_ms: Hold the request open this long for an event to arrive.
                Values above the server's limit are clamped.
        """
        params: SessionEventsReadParams = {"sessionId": session_id}
        if not isinstance(after, Unset):
            params["after"] = after
        if not isinstance(before, Unset):
            params["before"] = before
        if not isinstance(direction, Unset):
            params["direction"] = direction
        if not isinstance(limit, Unset):
            params["limit"] = limit
        if not isinstance(wait_ms, Unset):
            params["waitMs"] = wait_ms
        return await self.call("session/events/read", params, headers=headers)

    async def await_run(
        self,
        session_id: str,
        run_id: str,
        *,
        after: EventCursor | None = None,
        limit: int | None | Unset = UNSET,
        wait_ms: int | None = None,
        on_event: Callback[[SessionEventView]] | None = None,
        on_page: Callback[[SessionEventsReadResponse]] | None = None,
        heartbeat: Callback[[EventCursor | None, SessionEventsReadResponse]] | None = None,
        headers: HeadersInput | None = None,
    ) -> AwaitRunResult:
        """Wait until a run finishes and return it with the cursor reached.

        Repeats reads that fail on the network for about 30 seconds, and raises
        `LightspeedApiError` of kind `sessionExecutionStopped` when nothing can
        finish the run. A run that has already finished returns at once,
        whatever the cursor.

        Giving up on the wait does not cancel the run: bound it with
        `asyncio.timeout`, or cancel the task, and read or wait for the run
        again later. To cancel the run itself, call `cancel_run`.

        Args:
            after: A saved cursor to resume from. Without it, the wait reads
                the session from its beginning.
            limit: The most events to read per page.
            wait_ms: How long each read holds open for new events, 30 seconds
                by default and at least one.
            on_event: Called for this run's own events. May be async.
            on_page: Called for every page read, which holds every event in
                the session, not only this run's. May be async.
            heartbeat: Called after each page with the cursor reached. May be
                async.
        """
        wait_ms = max(wait_ms if wait_ms is not None else _WAIT_MS, _WAIT_MIN_MS)
        cursor = after
        current_wait_ms = 0

        async def read_run() -> RunView:
            result = await _retry_read(
                lambda: self.read_run(session_id, run_id, headers=headers), _WAIT_REQUEST_GRACE,
            )
            return result["run"]

        while True:
            page_after = cursor
            page_wait_ms = current_wait_ms
            page = await _retry_read(
                lambda: self.read_events(
                    session_id, after=page_after, limit=limit, wait_ms=page_wait_ms, headers=headers,
                ),
                page_wait_ms / 1000 + _WAIT_REQUEST_GRACE,
            )
            await _notify(on_page, page)
            events = page.get("events", [])
            finished = False
            for event in events:
                cursor = event["cursor"]
                if event["joins"].get("runId") == run_id:
                    await _notify(on_event, event)
                kind = event["kind"]
                if kind["type"] in _TERMINAL_EVENTS and kind.get("runId") == run_id:
                    finished = True
                    break
            if not finished:
                cursor = page.get("nextCursor") or cursor
            await _notify(heartbeat, cursor, page)
            if finished:
                run = await read_run()
                if run["status"] not in TERMINAL_STATUSES:
                    raise LightspeedTransportError(
                        f"Lightspeed API reported run {run_id} as {run['status']} after its terminal event",
                    )
                return {"run": run, "cursor": cursor}

            if not events:
                response = await _retry_read(
                    lambda: self.read_session(session_id, headers=headers), _WAIT_REQUEST_GRACE,
                )
                session = response["session"]
                summary = next((item for item in session.get("runs", []) if item["id"] == run_id), None)
                if summary is None or summary["status"] in TERMINAL_STATUSES:
                    run = await read_run()
                    if run["status"] in TERMINAL_STATUSES:
                        return {"run": run, "cursor": cursor}
                if failure := session.get("executionFailure"):
                    message = f"session {session_id} execution has stopped ({failure})"
                    raise LightspeedApiError({
                        "type": "about:blank", "title": "Session execution stopped", "status": 409,
                        "detail": message, "kind": "sessionExecutionStopped",
                    })
            current_wait_ms = wait_ms

    async def read_blob_bytes(self, blob_ref: str, *, headers: HeadersInput | None = None) -> bytes:
        """A blob's bytes. Refs come from run output, tool calls, and message entries."""
        return await self.call("blobs/read", {"blobRef": blob_ref}, headers=headers)

    async def read_blob_text(self, blob_ref: str, *, headers: HeadersInput | None = None) -> str:
        """A blob as text, raising `UnicodeDecodeError` if it is not valid UTF-8.

        Text is not always what a blob holds: model output can be provider
        JSON or media.
        """
        return (await self.read_blob_bytes(blob_ref, headers=headers)).decode("utf-8-sig")

    async def put_blob(self, data: str | bytes, *, headers: HeadersInput | None = None) -> str:
        """Store bytes, or text as UTF-8, and return the reference to use them by."""
        if isinstance(data, str):
            data = data.encode("utf-8")
        params: BlobPutParams = {"blobs": [{"bytesBase64": base64.b64encode(data).decode("ascii")}]}
        outcome = await self.call("blobs/put", params, headers=headers)
        blobs = outcome.get("blobs", [])
        if not blobs:
            raise LightspeedTransportError("Lightspeed API stored no blob")
        return blobs[0]["blobRef"]

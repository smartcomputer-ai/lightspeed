from __future__ import annotations

import asyncio
import base64
import inspect
from collections.abc import Awaitable, Callable
from itertools import count
from types import TracebackType
from typing import Any, Self, TypedDict
from uuid import uuid4

import httpx

from ._generated import METHOD_INFO, GeneratedClient
from ._options import UNSET, HeaderProvider, HeadersInput, Unset
from .errors import LightspeedRpcError, LightspeedTransportError
from .types import (
    BlobPutParams,
    EventCursor,
    InputItem,
    RunStartConfig,
    RunStartParams,
    RunStartResponse,
    RunSteerParams,
    RunSteerResponse,
    RunView,
    SessionEventsReadParams,
    SessionEventsReadResponse,
    SessionEventView,
)

type RequestId = str | int
type Callback[**P] = Callable[P, None | Awaitable[None]]

TERMINAL_STATUSES = frozenset(("completed", "failed", "cancelled"))
_TERMINAL_EVENTS = frozenset(("runCompleted", "runFailed", "runCancelled"))
_WAIT_MS = 30_000
_WAIT_MIN_MS = 1_000
_WAIT_REQUEST_GRACE = 30.0
_WAIT_READ_ATTEMPTS = 6
_WAIT_RETRY_BASE = 1.0


class AwaitRunResult(TypedDict):
    run: RunView
    cursor: EventCursor | None


def generate_submission_id() -> str:
    return str(uuid4())


def _input_items(value: str | list[InputItem]) -> list[InputItem]:
    if isinstance(value, str):
        return [{"type": "text", "text": value}]
    return value


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
    """Async gateway client. An injected HTTP client remains owned by its caller."""

    def __init__(
        self,
        endpoint: str | httpx.URL,
        *,
        api_key: str | None = None,
        universe: str | None = None,
        actor: str | None = None,
        headers: HeadersInput | HeaderProvider | None = None,
        request_id: Callable[[], RequestId] | None = None,
        http_client: httpx.AsyncClient | None = None,
    ) -> None:
        self._endpoint = endpoint
        self._api_key = api_key
        self._universe = universe
        self._actor = actor
        self._headers = headers
        self._http_client = http_client if http_client is not None else httpx.AsyncClient(timeout=60.0)
        self._owns_client = http_client is None
        ids = count(1)
        self._request_id = request_id if request_id is not None else ids.__next__

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self, exc_type: type[BaseException] | None, exc: BaseException | None, traceback: TracebackType | None
    ) -> None:
        await self.aclose()

    async def aclose(self) -> None:
        if self._owns_client:
            await self._http_client.aclose()

    def with_actor(self, actor: str) -> AsyncLightspeedClient:
        """Attribute calls to an actor using this client's connection and credential."""
        return AsyncLightspeedClient(
            self._endpoint, api_key=self._api_key, universe=self._universe, actor=actor,
            headers=self._headers, request_id=self._request_id, http_client=self._http_client,
        )

    async def _invoke(
        self, method: str, params: dict[str, Any], headers: HeadersInput | None = None
    ) -> dict[str, Any]:
        if method not in METHOD_INFO:
            raise ValueError(f"Unknown Lightspeed method {method!r}")
        request_id = self._request_id()
        request_headers = httpx.Headers({"content-type": "application/json", "accept": "application/json"})
        if self._api_key is not None:
            request_headers["authorization"] = f"Bearer {self._api_key}"
        if self._universe is not None:
            request_headers["x-lightspeed-universe"] = self._universe
        if self._actor is not None:
            request_headers["x-lightspeed-actor"] = self._actor
        defaults = self._headers() if callable(self._headers) else self._headers
        if inspect.isawaitable(defaults):
            defaults = await defaults
        if defaults is not None:
            request_headers.update(defaults)
        if headers is not None:
            request_headers.update(headers)
        try:
            request = self._http_client.build_request(
                "POST", self._endpoint, headers=request_headers,
                json={"id": request_id, "method": method, "params": params},
            )
            if METHOD_INFO[method]["scope"] in ("connection", "deployment"):
                request.headers.pop("x-lightspeed-universe", None)
            response = await self._http_client.send(request)
        except httpx.RequestError as error:
            raise LightspeedTransportError(
                f"Lightspeed API request failed: {error}", transient=True,
            ) from error
        except httpx.InvalidURL as error:
            raise LightspeedTransportError(f"Invalid Lightspeed gateway URL {self._endpoint!s}") from error

        try:
            body = response.json() if response.content else None
        except ValueError as error:
            if response.is_success:
                raise LightspeedTransportError(
                    "Lightspeed API response was not valid JSON", status=response.status_code, body=response.text,
                ) from error
            body = response.text
        if not response.is_success:
            hint = f": check that {self._endpoint!s} is the gateway's /rpc URL" if response.status_code == 404 else ""
            raise LightspeedTransportError(
                f"Lightspeed API request failed with HTTP {response.status_code}{hint}",
                status=response.status_code, body=body,
            )
        if not isinstance(body, dict):
            raise LightspeedTransportError("Lightspeed API response was not a JSON object", body=body)
        if "id" in body and (type(body["id"]) is not type(request_id) or body["id"] != request_id):
            raise LightspeedTransportError("Lightspeed API response id did not match the request id", body=body)
        rpc_error = body.get("error")
        if rpc_error:
            if (
                not isinstance(rpc_error, dict)
                or type(rpc_error.get("code")) is not int
                or not isinstance(rpc_error.get("message"), str)
            ):
                raise LightspeedTransportError("Lightspeed API response had an invalid error", body=body)
            raise LightspeedRpcError(rpc_error)
        result = body.get("result")
        if not isinstance(result, dict):
            raise LightspeedTransportError("Lightspeed API response was missing its outcome object", body=body)
        return result

    async def start_run(
        self,
        session_id: str,
        input: str | list[InputItem],
        *,
        submission_id: str | None | Unset = UNSET,
        config: RunStartConfig | None | Unset = UNSET,
        headers: HeadersInput | None = None,
    ) -> RunStartResponse:
        """Submit work. Keep the same submission ID and input when retrying."""
        params: RunStartParams = {
            "sessionId": session_id,
            "source": {"type": "input", "items": _input_items(input)},
            "submissionId": generate_submission_id() if isinstance(submission_id, Unset) else submission_id,
        }
        if not isinstance(config, Unset):
            params["config"] = config
        return (await self.call("session/runs/start", params, headers=headers))["result"]

    async def steer_run(
        self,
        session_id: str,
        run_id: str,
        input: str | list[InputItem],
        *,
        submission_id: str | None | Unset = UNSET,
        headers: HeadersInput | None = None,
    ) -> RunSteerResponse:
        """Send input to an active run."""
        params: RunSteerParams = {
            "sessionId": session_id,
            "runId": run_id,
            "items": _input_items(input),
            "submissionId": generate_submission_id() if isinstance(submission_id, Unset) else submission_id,
        }
        return (await self.call("session/runs/steer", params, headers=headers))["result"]

    async def read_events(
        self,
        session_id: str,
        *,
        after: EventCursor | None | Unset = UNSET,
        limit: int | None | Unset = UNSET,
        wait_ms: int | None | Unset = UNSET,
        headers: HeadersInput | None = None,
    ) -> SessionEventsReadResponse:
        """Read one forward page of durable session events."""
        params: SessionEventsReadParams = {"sessionId": session_id}
        if not isinstance(after, Unset):
            params["after"] = after
        if not isinstance(limit, Unset):
            params["limit"] = limit
        if not isinstance(wait_ms, Unset):
            params["waitMs"] = wait_ms
        return (await self.call("session/events/read", params, headers=headers))["result"]

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
        """Follow a run to its terminal state. Cancelling this wait leaves the run alone."""
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
                    raise LightspeedRpcError({
                        "code": -32016, "message": message,
                        "data": {"kind": "sessionExecutionStopped", "message": message},
                    })
            current_wait_ms = wait_ms

    async def read_blob_bytes(self, blob_ref: str, *, headers: HeadersInput | None = None) -> bytes:
        outcome = await self.call("blobs/read", {"blobRef": blob_ref}, headers=headers)
        return base64.b64decode(outcome["result"]["bytesBase64"], validate=True)

    async def read_blob_text(self, blob_ref: str, *, headers: HeadersInput | None = None) -> str:
        return (await self.read_blob_bytes(blob_ref, headers=headers)).decode("utf-8-sig")

    async def put_blob(self, data: str | bytes, *, headers: HeadersInput | None = None) -> str:
        if isinstance(data, str):
            data = data.encode("utf-8")
        params: BlobPutParams = {"blobs": [{"bytesBase64": base64.b64encode(data).decode("ascii")}]}
        outcome = await self.call("blobs/put", params, headers=headers)
        blobs = outcome["result"].get("blobs", [])
        if not blobs:
            raise LightspeedTransportError("Lightspeed API stored no blob")
        return blobs[0]["blobRef"]

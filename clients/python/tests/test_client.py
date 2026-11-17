import asyncio
import json
import re
from urllib.parse import unquote
from collections import deque
from pathlib import Path

import httpx
import pytest

from lightspeed_ai import (
    CLIENT_HELPERS,
    MANUAL_CLIENT_HELPERS,
    OPERATION_INFO,
    OPERATIONS,
    NOTIFICATIONS,
    AsyncLightspeedClient,
    LightspeedApiError,
    LightspeedTransportError,
)
from lightspeed_ai import client as implementation
from lightspeed_ai._http import HTTP_OPERATIONS

PARITY = json.loads((Path(__file__).resolve().parents[2] / "fixtures/gateway-parity.json").read_text())


@pytest.mark.parametrize("item", PARITY["calls"], ids=lambda item: item["method"])
async def test_shared_wire_cases(item):
    async def handle(request):
        assert request.method == item["verb"]
        assert str(request.url) == "http://localhost" + item["path"]
        assert (json.loads(request.content) if request.content else None) == item["body"]
        for name, expected in item["expectHeaders"].items():
            assert request.headers.get(name) == expected
        return httpx.Response(item["status"], json=item["response"])

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with AsyncLightspeedClient(
            "http://localhost/api/v1", token="option", actor="actor", universe="fixture-universe",
            headers={"Authorization": "Bearer configured"},
            http_client=http,
        ) as client:
            if item["status"] >= 400:
                with pytest.raises(LightspeedApiError) as caught:
                    await client.call(item["method"], item["params"], headers=item.get("headers"))
                assert caught.value.payload == item["response"]
                assert caught.value.kind == item["response"]["kind"]
            else:
                assert await client.call(
                    item["method"], item["params"], headers=item.get("headers"),
                ) == item["response"]
        assert not http.is_closed


def decode_request(request):
    """Expose logical values for the run-wait tests; wire fixtures assert URLs independently."""
    for name, binding in HTTP_OPERATIONS.items():
        names = re.findall(r"\{([^}]+)\}", binding["path"])
        pattern = re.sub(r"\{[^}]+\}", "([^/]+)", binding["path"])
        match = re.fullmatch(pattern, request.url.raw_path.decode().split("?")[0])
        if request.method != binding["method"] or not match:
            continue
        params = json.loads(request.content) if request.content and binding["transfer"] == "json" else {}
        path_values = dict(zip(names, match.groups()))
        for field in binding["pathFields"]:
            owner = params
            parts = field["field"].split(".")
            for part in parts[:-1]:
                owner = owner.setdefault(part, {})
            owner[parts[-1]] = unquote(path_values[field["parameter"]])
        for parameter in binding["parameters"]:
            key = parameter["name"]
            if parameter["in"] != "query" or key not in request.url.params:
                continue
            value = request.url.params[key]
            if parameter.get("x-lightspeed-encoding") == "json":
                value = json.loads(value)
            elif parameter.get("x-lightspeed-encoding") == "repeated":
                value = request.url.params.get_list(key)
            elif key in ("waitMs", "limit"):
                value = int(value)
            params[key] = value
        if name == "session/events/read" and "waitMs" in params:
            params.setdefault("after", None)
        return {"method": name, "params": params}
    raise AssertionError(f"Unexpected request {request.method} {request.url}")


class ScriptedGateway:
    def __init__(self, *responses):
        self.responses = deque(responses)
        self.calls = []

    def __call__(self, request):
        call = decode_request(request)
        self.calls.append(call)
        response = self.responses.popleft()
        if isinstance(response, Exception):
            raise response
        if isinstance(response, httpx.Response):
            return response
        return httpx.Response(200, json=response)


async def test_shared_wait_case():
    item = PARITY["wait"]
    gateway = ScriptedGateway(*(row["result"] for row in item["requests"]))
    events = []

    async def on_event(event):
        events.append(event["cursor"]["seq"])

    async with httpx.AsyncClient(transport=httpx.MockTransport(gateway)) as http:
        client = AsyncLightspeedClient("http://localhost/api/v1", universe="u", http_client=http)
        result = await client.await_run(item["sessionId"], item["runId"], after=item["after"], wait_ms=1000, on_event=on_event)
    assert [{"method": call["method"], "params": call["params"]} for call in gateway.calls] == [
        {key: value for key, value in row.items() if key != "result"} for row in item["requests"]
    ]
    assert events == item["observedSequences"]
    assert result == {"cursor": item["finalCursor"], **item["requests"][-1]["result"]}


async def test_mutations_keep_ids_and_omit_unset_fields():
    gateway = ScriptedGateway({}, httpx.ReadError("response lost"), {}, {}, {}, {})
    async with httpx.AsyncClient(transport=httpx.MockTransport(gateway)) as http:
        client = AsyncLightspeedClient("http://localhost/api/v1", universe="u", http_client=http)
        await client.start_session(display_name="Test", delete_after_close_ms=None)
        with pytest.raises(LightspeedTransportError) as caught:
            await client.start_run("s", "prompt", submission_id="stable")
        assert caught.value.transient
        assert len(gateway.calls) == 2
        await client.start_run("s", "prompt", submission_id="stable")
        await client.start_run("s", "prompt", submission_id=None, config=None)
        await client.steer_run("s", "1", "more")
        await client.read_events("s")
    assert gateway.calls[0]["params"] == {"displayName": "Test", "deleteAfterCloseMs": None}
    assert gateway.calls[1]["params"] == gateway.calls[2]["params"] == {
        "sessionId": "s", "source": {"type": "input", "items": [{"type": "text", "text": "prompt"}]},
        "submissionId": "stable",
    }
    assert gateway.calls[3]["params"]["submissionId"] is None
    assert gateway.calls[3]["params"]["config"] is None
    assert gateway.calls[4]["params"]["submissionId"]
    assert gateway.calls[5]["params"] == {"sessionId": "s"}


async def test_wait_retries_transient_reads_and_finds_terminal_run_behind_cursor(monkeypatch):
    monkeypatch.setattr(implementation, "_WAIT_RETRY_BASE", 0)
    cursor = {"seq": 42}
    run = {"id": "1", "status": "completed"}
    gateway = ScriptedGateway(
        httpx.Response(502, text="proxy"), {"complete": True},
        {"session": {"runs": [run]}}, {"run": run},
    )
    async with httpx.AsyncClient(transport=httpx.MockTransport(gateway)) as http:
        client = AsyncLightspeedClient("http://localhost/api/v1", universe="u", http_client=http)
        assert await client.await_run("s", "1", after=cursor) == {"run": run, "cursor": cursor}
    assert gateway.calls[0]["params"] == gateway.calls[1]["params"]


async def test_wait_cancellation_sends_no_mutation():
    entered = asyncio.Event()
    calls = []

    async def handle(request):
        calls.append(decode_request(request)["method"])
        entered.set()
        await asyncio.Future()

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        client = AsyncLightspeedClient("http://localhost/api/v1", universe="u", http_client=http)
        task = asyncio.create_task(client.await_run("s", "1"))
        await asyncio.wait_for(entered.wait(), timeout=1)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
    assert calls == ["session/events/read"]


async def test_actor_view_shares_client_without_taking_ownership():
    actors = []

    async def handle(request):
        actors.append(request.headers.get("x-lightspeed-actor"))
        return httpx.Response(200, json={"session": {}})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        client = AsyncLightspeedClient("http://localhost/api/v1", universe="u", http_client=http)
        async with client.with_actor("alice") as view:
            await view.read_session("s")
        await client.read_session("s")
    assert actors == ["alice", None]


@pytest.mark.parametrize("status, body, transient", [(200, "<bad>", False), (503, "<proxy>", True)])
async def test_transport_error_carries_status_and_body(status, body, transient):
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(status, text=body))) as http:
        client = AsyncLightspeedClient("http://localhost/api/v1", universe="u", http_client=http)
        with pytest.raises(LightspeedTransportError) as caught:
            await client.initialize()
    assert (caught.value.status, caught.value.body, caught.value.transient) == (status, body, transient)


async def test_blob_bytes_text_and_upload():
    gateway = ScriptedGateway(httpx.Response(200, content=b"\xff"), httpx.Response(200, content=b"\xff"), {"blobs": [{"blobRef": "sha256:ref", "bytes": 2}]})
    async with httpx.AsyncClient(transport=httpx.MockTransport(gateway)) as http:
        client = AsyncLightspeedClient("http://localhost/api/v1", universe="u", http_client=http)
        assert await client.read_blob_bytes("ref") == b"\xff"
        with pytest.raises(UnicodeDecodeError):
            await client.read_blob_text("ref")
        assert await client.put_blob("é") == "sha256:ref"
    assert gateway.calls[-1]["params"] == {"blobs": [{"bytesBase64": "w6k="}]}


def completed(seq, run_id="run_1"):
    return {
        "cursor": {"seq": seq}, "sessionId": "session_1", "observedAtMs": seq, "joins": {"runId": run_id},
        "kind": {"type": "runCompleted", "runId": run_id, "output": None},
    }


def turn_started(seq, run_id="run_1"):
    return {
        "cursor": {"seq": seq}, "sessionId": "session_1", "observedAtMs": seq,
        "joins": {"runId": run_id, "turnId": "1"}, "kind": {"type": "turnStarted", "runId": run_id, "turnId": "1"},
    }


def page(events, head_seq=None):
    last = events[-1]["cursor"] if events else None
    return {
        "events": events, "headCursor": {"seq": head_seq if head_seq is not None else (last or {"seq": 0})["seq"]},
        "nextCursor": last, "complete": True,
    }


FINISHED_RUN = {"id": "run_1", "status": "completed", "outputText": "done", "source": {"type": "input", "items": []}}


class MethodGateway:
    """Answers each method from a handler and records every call."""

    def __init__(self, **handlers):
        self.handlers = {name.replace("__", "/"): handler for name, handler in handlers.items()}
        self.calls = []

    def __call__(self, request):
        call = decode_request(request)
        self.calls.append(call)
        result = self.handlers[call["method"]](call["params"])
        if isinstance(result, Exception):
            raise result
        if isinstance(result, httpx.Response):
            return result
        return httpx.Response(200, json=result)

    def params(self, method):
        return [call["params"] for call in self.calls if call["method"] == method]

    def client(self):
        return AsyncLightspeedClient("http://lightspeed.local/api/v1", universe="u", http_client=httpx.AsyncClient(
            transport=httpx.MockTransport(self),
        ))


def api_refusal(status, kind):
    def refuse(_params):
        return httpx.Response(status, json={"type": "about:blank", "title": kind, "detail": kind, "kind": kind, "status": status})
    return refuse


async def test_wait_resumes_from_cursors_and_reports_only_its_own_events():
    pages = deque([
        page([turn_started(1, "other_run"), turn_started(2)]), page([completed(3, "other_run"), completed(4)]),
    ])
    gateway = MethodGateway(
        session__events__read=lambda _: pages.popleft(), session__runs__read=lambda _: {"run": FINISHED_RUN},
    )
    events, heartbeats, page_sizes = [], [], []

    result = await gateway.client().await_run(
        "session_1", "run_1", wait_ms=1234,
        on_event=lambda event: events.append(event["cursor"]["seq"]),
        on_page=lambda read: page_sizes.append(len(read["events"])),
        heartbeat=lambda cursor, _page: heartbeats.append(cursor),
    )

    assert result == {"run": FINISHED_RUN, "cursor": {"seq": 4}}
    assert events == [2, 4]
    assert page_sizes == [2, 2]
    assert heartbeats == [{"seq": 2}, {"seq": 4}]
    assert gateway.params("session/events/read") == [
        {"sessionId": "session_1", "after": None, "waitMs": 0},
        {"sessionId": "session_1", "after": {"seq": 2}, "waitMs": 1234},
    ]


async def test_wait_never_advances_to_a_head_that_moved_past_the_page_it_read():
    # The terminal event landed between the empty read and the head query.
    pages = deque([page([], 1), page([completed(1)])])
    gateway = MethodGateway(
        session__events__read=lambda _: pages.popleft(),
        session__read=lambda _: {"session": {"runs": [{"id": "run_1", "status": "running"}]}},
        session__runs__read=lambda _: {"run": FINISHED_RUN},
    )

    result = await gateway.client().await_run("session_1", "run_1")

    assert result["cursor"] == {"seq": 1}
    assert [params["after"] for params in gateway.params("session/events/read")] == [None, None]


async def test_wait_keeps_waiting_while_the_run_is_still_going():
    pages = deque([page([], 1), page([], 1), page([completed(2)])])
    gateway = MethodGateway(
        session__events__read=lambda _: pages.popleft(),
        session__read=lambda _: {"session": {"runs": [{"id": "run_1", "status": "running"}]}},
        session__runs__read=lambda _: {"run": FINISHED_RUN},
    )

    result = await gateway.client().await_run("session_1", "run_1", after={"seq": 1}, wait_ms=5000)

    assert result["cursor"] == {"seq": 2}
    assert gateway.params("session/events/read") == [
        {"sessionId": "session_1", "after": {"seq": 1}, "waitMs": wait_ms} for wait_ms in (0, 5000, 5000)
    ]


async def test_wait_rejects_when_the_sessions_execution_has_stopped():
    gateway = MethodGateway(
        session__events__read=lambda _: page([], 3),
        session__read=lambda _: {
            "session": {"executionFailure": "terminated", "runs": [{"id": "run_1", "status": "running"}]},
        },
    )

    with pytest.raises(LightspeedApiError) as caught:
        await gateway.client().await_run("session_1", "run_1", after={"seq": 3})
    assert (caught.value.kind, caught.value.status) == ("sessionExecutionStopped", 409)


async def test_wait_rejects_for_a_run_the_session_does_not_have():
    gateway = MethodGateway(
        session__events__read=lambda _: page([], 3),
        session__read=lambda _: {"session": {"runs": []}},
        session__runs__read=api_refusal(404, "notFound"),
    )

    with pytest.raises(LightspeedApiError) as caught:
        await gateway.client().await_run("session_1", "missing")
    assert caught.value.kind == "notFound"


async def test_wait_gives_up_after_a_bounded_number_of_attempts(monkeypatch):
    monkeypatch.setattr(implementation, "_WAIT_RETRY_BASE", 0)
    failure = httpx.ConnectError("connection refused")
    gateway = MethodGateway(session__events__read=lambda _: failure)

    with pytest.raises(LightspeedTransportError) as caught:
        await gateway.client().await_run("session_1", "run_1")
    assert caught.value.transient
    assert caught.value.__cause__ is failure
    assert len(gateway.calls) == 6


async def test_wait_does_not_repeat_a_read_the_server_refused():
    gateway = MethodGateway(session__events__read=api_refusal(403, "forbidden"))

    with pytest.raises(LightspeedApiError) as caught:
        await gateway.client().await_run("session_1", "run_1")
    assert caught.value.kind == "forbidden"
    assert len(gateway.calls) == 1


async def test_wait_abandons_a_connection_that_stalls(monkeypatch):
    monkeypatch.setattr(implementation, "_WAIT_RETRY_BASE", 0)
    monkeypatch.setattr(implementation, "_WAIT_REQUEST_GRACE", 0.01)
    attempts = 0

    async def handle(request):
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            await asyncio.Future()
        call = decode_request(request)
        result = page([completed(1)]) if attempts == 2 else {"run": FINISHED_RUN}
        return httpx.Response(200, json=result)

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        client = AsyncLightspeedClient("http://lightspeed.local/api/v1", universe="u", http_client=http)
        result = await client.await_run("session_1", "run_1")

    assert result["cursor"] == {"seq": 1}
    assert attempts == 3


async def test_http_unauthenticated_problem():
    refusal = {"type": "about:blank", "title": "Unauthorized", "detail": "Unauthorized", "kind": "unauthenticated", "status": 401}
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(401, json=refusal))) as http:
        client = AsyncLightspeedClient("http://runtime/api/v1", http_client=http)
        with pytest.raises(LightspeedApiError) as caught:
            await client.initialize()
    assert (caught.value.kind, caught.value.status) == ("unauthenticated", 401)


async def test_a_401_that_is_not_the_gateways_is_reported_by_its_status():
    page = httpx.Response(401, text="<html>Sign in</html>")
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: page)) as http:
        client = AsyncLightspeedClient("http://lightspeed.local/api/v1", universe="u", http_client=http)
        with pytest.raises(LightspeedTransportError) as caught:
            await client.initialize()
    assert (caught.value.status, caught.value.transient) == (401, False)


def test_operation_lists_and_documentation_come_from_the_contract():
    contract = json.loads((Path(__file__).resolve().parents[3] / "crates/api/contract/http.json").read_text())
    assert list(OPERATIONS) == [entry["operationId"] for entry in contract["operations"]] == list(OPERATION_INFO)
    assert set(NOTIFICATIONS) == set(HTTP_OPERATIONS["universe/watch"]["events"]["events"])
    assert set(CLIENT_HELPERS.values()) == set(OPERATIONS)
    assert len(CLIENT_HELPERS) == len(OPERATIONS)
    assert set(MANUAL_CLIENT_HELPERS) <= set(CLIENT_HELPERS)


def test_every_direct_helper_is_available_and_documented():
    for name, method in CLIENT_HELPERS.items():
        helper = getattr(AsyncLightspeedClient, name)
        assert callable(helper)
        assert helper.__doc__, name
        if name not in MANUAL_CLIENT_HELPERS:
            assert helper.__doc__.startswith(OPERATION_INFO[method]["summary"]), name


def test_readme_documents_every_helper():
    readme = (Path(__file__).resolve().parents[1] / "README.md").read_text()
    hand_written = [
        "start_run", "steer_run", "await_run", "read_events", "read_blob_text", "read_blob_bytes", "put_blob",
    ]
    missing = [name for name in [*CLIENT_HELPERS, *hand_written] if f"`{name}`" not in readme]
    assert not missing


class WatchBody(httpx.AsyncByteStream):
    def __init__(self, content):
        self.content = content
        self.closed = False

    async def __aiter__(self):
        for byte in self.content.encode():
            yield bytes([byte])

    async def aclose(self):
        self.closed = True


async def test_watch_auth_multiline_utf8_final_response_and_close():
    event = {"method": "workspaces/removed", "params": {"workspaceId": "hé"}}
    body = WatchBody(': heartbeat\r\nevent: watch/reset\r\ndata: {}\r\n\r\n'
                     + 'event: workspaces/removed\ndata: ' + json.dumps(event["params"], ensure_ascii=False) + '\n\n'
                     + 'event: watch/end\ndata: {}\n\n')

    def handle(request):
        assert request.method == "GET"
        assert request.headers["accept"] == "text/event-stream"
        assert request.headers["authorization"] == "Bearer secret"
        assert "x-lightspeed-universe" not in request.headers
        assert str(request.url) == "http://core/api/v1/universes/u/events?kinds=workspace"
        return httpx.Response(200, headers={"content-type": "text/event-stream"}, stream=body)

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        client = AsyncLightspeedClient("http://core/api/v1", token="secret", universe="u", http_client=http)
        events = [item async for item in client.watch({"kinds": ["workspace"]}, reconnect=False)]
    assert events == [{"method": "watch/reset", "params": {}}, event]
    assert body.closed


async def test_watch_iterator_close_and_typed_refusal():
    body = WatchBody('event: watch/reset\ndata: {}\n\n')
    gateway = ScriptedGateway(httpx.Response(200, headers={"content-type": "text/event-stream"}, stream=body),
                              api_refusal(403, "forbidden")({}),
                              httpx.ReadError("lost"))
    async with httpx.AsyncClient(transport=httpx.MockTransport(gateway)) as http:
        client = AsyncLightspeedClient("http://core/api/v1", universe="u", http_client=http)
        watch = client.watch()
        assert (await anext(watch))["method"] == "watch/reset"
        await watch.aclose()
        assert body.closed
        with pytest.raises(LightspeedApiError):
            await anext(client.watch())
        with pytest.raises(LightspeedTransportError) as caught:
            await anext(client.watch(reconnect=False))
        assert caught.value.transient
    assert len(gateway.calls) == 3


def test_deleted_universe_problem_is_gone() -> None:
    assert LightspeedApiError({"type": "about:blank", "title": "Gone", "detail": "Deleted", "kind": "gone", "status": 410}).kind == "gone"


async def test_binary_upload_and_streaming_download():
    def handle(request):
        if request.method == "PUT":
            assert request.content == b"abc"
            assert request.headers["content-length"] == "3"
            assert request.headers["content-type"] == "application/octet-stream"
            return httpx.Response(200, json={"blobRef": "sha256:test", "bytes": 3})
        return httpx.Response(200, content=b"abc")
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        client = AsyncLightspeedClient("http://runtime/api/v1", universe="u", http_client=http)
        assert await client.upload_blob("sha256:test", b"abc") == {"blobRef": "sha256:test", "bytes": 3}
        async with client.open_download("blobs/read", {"blobRef": "sha256:test"}) as response:
            assert await response.aread() == b"abc"
        assert response.is_closed


async def test_named_administration_helpers_preserve_targets_nulls_and_query_maps():
    requests = []
    async def handle(request):
        requests.append(request)
        return httpx.Response(200, json={"result": len(requests)})
    bot = {"botId": "bot/one", "profileId": "profile", "displayName": "Bot"}
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        client = AsyncLightspeedClient("https://runtime.test/base", universe="u", http_client=http)
        await client.list_universes(metadata={"team": "build"})
        await client.put_universe_configuration("target", protected=False, display_name=None)
        await client.put_credential_grant("grant/one", token="secret", expires_at_ms=None)
        await client.put_bot(bot=bot, expected_revision=7)
        await client.read_bot_event("bot/one", seq=12)
        await client.put_environment_provider("provider/one", controller_endpoint="https://controller.test", credential=None, clear_credential=True)
    assert json.loads(requests[0].url.params["metadata"]) == {"team": "build"}
    assert requests[0].url.path == "/base/api/v1/universes"
    assert requests[1].url.path == "/base/api/v1/universes/target/configuration"
    assert json.loads(requests[1].content) == {"protected": False, "displayName": None}
    assert requests[2].url.raw_path == b"/base/api/v1/universes/u/credentials/grants/grant%2Fone"
    assert json.loads(requests[2].content) == {"token": "secret", "expiresAtMs": None}
    assert requests[3].url.raw_path == b"/base/api/v1/universes/u/bots/bot%2Fone"
    assert json.loads(requests[3].content) == {"bot": {key: value for key, value in bot.items() if key != "botId"}, "expectedRevision": 7}
    assert bot["botId"] == "bot/one", "helper must not mutate caller-owned nested input"
    assert requests[4].url.raw_path == b"/base/api/v1/universes/u/bots/bot%2Fone/events/12"
    assert requests[5].url.raw_path == b"/base/api/v1/environment-providers/provider%2Fone"
    assert json.loads(requests[5].content) == {"controllerEndpoint": "https://controller.test", "credential": None, "clearCredential": True}


async def test_manual_run_and_event_helpers_expose_every_wire_field():
    requests = []
    async def handle(request):
        requests.append(request)
        return httpx.Response(200, json={"run": {}, "events": []})
    source = {"type": "input", "items": [{"type": "text", "text": "Ready"}]}
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        client = AsyncLightspeedClient("https://runtime.test", universe="u", http_client=http)
        await client.start_run("session", source, submission_id="retry", config=None, notify_on_terminal={"token": "done"})
        await client.read_events("session", direction="backward", before={"seq": 22}, after=None, limit=9, wait_ms=0)
    assert json.loads(requests[0].content) == {"source": source, "submissionId": "retry", "config": None, "notifyOnTerminal": {"token": "done"}}
    assert dict(requests[1].url.params) == {"direction": "backward", "before": '{"seq":22}', "limit": "9", "waitMs": "0"}


async def test_named_binary_helpers_use_bytes_streams_and_valid_declared_lengths():
    requests = []
    async def body():
        yield b"a"
        yield b"bc"
    async def handle(request):
        requests.append(request)
        if request.method == "PUT":
            assert await request.aread() == b"abc"
            assert request.headers["content-length"] == "3"
            assert request.headers["content-type"] == "application/octet-stream"
            return httpx.Response(200, json={"blobRef": "sha256:bytes", "bytes": 3})
        return httpx.Response(200, content=b"\x00\xff")
    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        client = AsyncLightspeedClient("https://runtime.test", universe="u", http_client=http)
        with pytest.raises(ValueError, match="content_length"):
            await client.upload_blob("sha256:bytes", body())
        assert requests == []
        assert await client.upload_blob("sha256:bytes", body(), content_length=3) == {"blobRef": "sha256:bytes", "bytes": 3}
        assert await client.read_workspace_file("workspace", "/file") == b"\x00\xff"
    assert requests[-1].url.params["path"] == "/file"

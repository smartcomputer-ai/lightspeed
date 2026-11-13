import asyncio
import json
from collections import deque
from pathlib import Path

import httpx
import pytest

from lightspeed_ai import (
    CLIENT_HELPERS,
    METHOD_INFO,
    METHODS,
    NOTIFICATIONS,
    AsyncLightspeedClient,
    LightspeedRpcError,
    LightspeedTransportError,
    rpc_error_kind,
)
from lightspeed_ai import client as implementation

PARITY = json.loads((Path(__file__).resolve().parents[2] / "fixtures/gateway-parity.json").read_text())


@pytest.mark.parametrize("item", PARITY["calls"], ids=lambda item: item["method"])
async def test_shared_wire_cases(item):
    async def handle(request):
        assert json.loads(request.content) == {"id": 1, "method": item["method"], "params": item["params"]}
        for name, expected in item["expectHeaders"].items():
            assert request.headers.get(name) == expected
        return httpx.Response(200, json={"id": 1, **item["response"]})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        async with AsyncLightspeedClient(
            "http://localhost/rpc", api_key="option", actor="actor", universe="option",
            headers={"Authorization": "Bearer configured", "x-lightspeed-universe": "default-universe"},
            http_client=http,
        ) as client:
            if error := item["response"].get("error"):
                with pytest.raises(LightspeedRpcError) as caught:
                    await client.call(item["method"], item["params"], headers=item.get("headers"))
                assert caught.value.payload == error
                assert caught.value.kind == error["data"]["kind"]
            else:
                assert await client.call(
                    item["method"], item["params"], headers=item.get("headers"),
                ) == item["response"]["result"]
        assert not http.is_closed


class ScriptedGateway:
    def __init__(self, *responses):
        self.responses = deque(responses)
        self.calls = []

    def __call__(self, request):
        call = json.loads(request.content)
        self.calls.append(call)
        response = self.responses.popleft()
        if isinstance(response, Exception):
            raise response
        if isinstance(response, httpx.Response):
            return response
        return httpx.Response(200, json={"id": call["id"], "result": {"result": response, "notifications": []}})


async def test_shared_wait_case():
    item = PARITY["wait"]
    gateway = ScriptedGateway(*(row["result"] for row in item["requests"]))
    events = []

    async def on_event(event):
        events.append(event["cursor"]["seq"])

    async with httpx.AsyncClient(transport=httpx.MockTransport(gateway)) as http:
        client = AsyncLightspeedClient("http://localhost/rpc", http_client=http)
        result = await client.await_run(item["sessionId"], item["runId"], after=item["after"], wait_ms=1000, on_event=on_event)
    assert [{"method": call["method"], "params": call["params"]} for call in gateway.calls] == [
        {key: value for key, value in row.items() if key != "result"} for row in item["requests"]
    ]
    assert events == item["observedSequences"]
    assert result == {"cursor": item["finalCursor"], **item["requests"][-1]["result"]}


async def test_mutations_keep_ids_and_omit_unset_fields():
    gateway = ScriptedGateway({}, httpx.ReadError("response lost"), {}, {}, {}, {})
    async with httpx.AsyncClient(transport=httpx.MockTransport(gateway)) as http:
        client = AsyncLightspeedClient("http://localhost/rpc", http_client=http)
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
        client = AsyncLightspeedClient("http://localhost/rpc", http_client=http)
        assert await client.await_run("s", "1", after=cursor) == {"run": run, "cursor": cursor}
    assert gateway.calls[0]["params"] == gateway.calls[1]["params"]


async def test_wait_cancellation_sends_no_mutation():
    entered = asyncio.Event()
    calls = []

    async def handle(request):
        calls.append(json.loads(request.content)["method"])
        entered.set()
        await asyncio.Future()

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        client = AsyncLightspeedClient("http://localhost/rpc", http_client=http)
        task = asyncio.create_task(client.await_run("s", "1"))
        await entered.wait()
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
    assert calls == ["session/events/read"]


async def test_actor_view_shares_client_without_taking_ownership():
    actors = []

    async def handle(request):
        actors.append(request.headers.get("x-lightspeed-actor"))
        return httpx.Response(200, json={"result": {"result": {"session": {}}, "notifications": []}})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        client = AsyncLightspeedClient("http://localhost/rpc", http_client=http)
        async with client.with_actor("alice") as view:
            await view.read_session("s")
        await client.read_session("s")
    assert actors == ["alice", None]


@pytest.mark.parametrize("status, body, transient", [(200, "<bad>", False), (503, "<proxy>", True)])
async def test_transport_error_carries_status_and_body(status, body, transient):
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(status, text=body))) as http:
        client = AsyncLightspeedClient("http://localhost/rpc", http_client=http)
        with pytest.raises(LightspeedTransportError) as caught:
            await client.initialize()
    assert (caught.value.status, caught.value.body, caught.value.transient) == (status, body, transient)


async def test_blob_bytes_text_and_upload():
    gateway = ScriptedGateway({"bytesBase64": "/w=="}, {"bytesBase64": "/w=="}, {"blobs": [{"blobRef": "sha256:ref", "bytes": 2}]})
    async with httpx.AsyncClient(transport=httpx.MockTransport(gateway)) as http:
        client = AsyncLightspeedClient("http://localhost/rpc", http_client=http)
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
        call = json.loads(request.content)
        self.calls.append(call)
        result = self.handlers[call["method"]](call["params"])
        if isinstance(result, Exception):
            raise result
        if isinstance(result, httpx.Response):
            return result
        return httpx.Response(200, json={"id": call["id"], "result": {"result": result, "notifications": []}})

    def params(self, method):
        return [call["params"] for call in self.calls if call["method"] == method]

    def client(self):
        return AsyncLightspeedClient("http://lightspeed.local/rpc", http_client=httpx.AsyncClient(
            transport=httpx.MockTransport(self),
        ))


def rpc_refusal(code, kind):
    def refuse(_params):
        return httpx.Response(200, json={"error": {"code": code, "message": kind, "data": {"kind": kind, "message": kind}}})
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

    with pytest.raises(LightspeedRpcError) as caught:
        await gateway.client().await_run("session_1", "run_1", after={"seq": 3})
    assert (caught.value.kind, caught.value.code) == ("sessionExecutionStopped", -32016)


async def test_wait_rejects_for_a_run_the_session_does_not_have():
    gateway = MethodGateway(
        session__events__read=lambda _: page([], 3),
        session__read=lambda _: {"session": {"runs": []}},
        session__runs__read=rpc_refusal(-32004, "notFound"),
    )

    with pytest.raises(LightspeedRpcError) as caught:
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
    gateway = MethodGateway(session__events__read=rpc_refusal(-32003, "forbidden"))

    with pytest.raises(LightspeedRpcError) as caught:
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
        call = json.loads(request.content)
        result = page([completed(1)]) if attempts == 2 else {"run": FINISHED_RUN}
        return httpx.Response(200, json={"id": call["id"], "result": {"result": result, "notifications": []}})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handle)) as http:
        client = AsyncLightspeedClient("http://lightspeed.local/rpc", http_client=http)
        result = await client.await_run("session_1", "run_1")

    assert result["cursor"] == {"seq": 1}
    assert attempts == 3


async def test_wrong_url_is_reported_with_a_hint_and_is_not_transient():
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(404))) as http:
        client = AsyncLightspeedClient("http://lightspeed.local/rpc", http_client=http)
        with pytest.raises(LightspeedTransportError) as caught:
            await client.initialize()
    assert "http://lightspeed.local/rpc is the gateway's /rpc URL" in str(caught.value)
    assert (caught.value.status, caught.value.transient) == (404, False)


async def test_the_gateways_401_is_the_servers_unauthenticated_error():
    # No valid key: the gateway answers before reading the body, so there is no request ID.
    refusal = {"id": None, "error": {
        "code": -32001, "message": "request is not authenticated",
        "data": {"kind": "unauthenticated", "message": "request is not authenticated"},
    }}
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(401, json=refusal))) as http:
        client = AsyncLightspeedClient("http://lightspeed.local/rpc", http_client=http)
        with pytest.raises(LightspeedRpcError) as caught:
            await client.initialize()
    assert (caught.value.kind, caught.value.code) == ("unauthenticated", -32001)


async def test_a_401_that_is_not_the_gateways_is_reported_by_its_status():
    page = httpx.Response(401, text="<html>Sign in</html>")
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: page)) as http:
        client = AsyncLightspeedClient("http://lightspeed.local/rpc", http_client=http)
        with pytest.raises(LightspeedTransportError) as caught:
            await client.initialize()
    assert (caught.value.status, caught.value.transient) == (401, False)


def test_error_kind_takes_the_servers_kind_over_the_code():
    assert rpc_error_kind(-32014) == "modelDefaultUnset"
    assert rpc_error_kind(-1) == "unknown"
    shared = LightspeedRpcError({
        "code": -32602, "message": "too large", "data": {"kind": "audioBlobTooLarge", "message": "too large"},
    })
    assert (shared.kind, shared.code, shared.message) == ("audioBlobTooLarge", -32602, "too large")
    assert LightspeedRpcError({"code": -32602, "message": "bad"}).kind == "invalidRequest"


def test_method_lists_and_documentation_come_from_the_manifest():
    manifest = json.loads((Path(__file__).resolve().parents[3] / "crates/api/contract/methods.json").read_text())
    assert list(METHODS) == [entry["method"] for entry in manifest["methods"]] == list(METHOD_INFO)
    assert list(NOTIFICATIONS) == manifest["notifications"]
    assert set(CLIENT_HELPERS.values()) <= set(METHODS)


def test_every_direct_helper_is_available_and_documented():
    for name, method in CLIENT_HELPERS.items():
        helper = getattr(AsyncLightspeedClient, name)
        assert callable(helper)
        assert helper.__doc__.startswith(METHOD_INFO[method]["summary"]), name


def test_readme_documents_every_helper():
    readme = (Path(__file__).resolve().parents[1] / "README.md").read_text()
    hand_written = [
        "start_run", "steer_run", "await_run", "read_events", "read_blob_text", "read_blob_bytes", "put_blob",
    ]
    missing = [name for name in [*CLIENT_HELPERS, *hand_written] if f"`{name}`" not in readme]
    assert not missing

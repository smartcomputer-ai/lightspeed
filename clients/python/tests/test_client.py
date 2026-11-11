import asyncio
import json
from collections import deque
from pathlib import Path

import httpx
import pytest

from lightspeed_ai import AsyncLightspeedClient, CLIENT_HELPERS, LightspeedRpcError, LightspeedTransportError
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


def test_all_direct_helpers_are_available():
    assert all(callable(getattr(AsyncLightspeedClient, name)) for name in CLIENT_HELPERS)

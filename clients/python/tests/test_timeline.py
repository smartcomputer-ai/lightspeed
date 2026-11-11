from lightspeed_ai import run_timeline


def test_timeline_merges_call_entries_and_preserves_in_flight_calls():
    call = {"kind": {"type": "toolCall", "callId": "first", "name": "read"}}
    result = {"kind": {"type": "toolResult", "callId": "first"}}
    message = {"kind": {"type": "message", "role": "assistant"}, "text": "done"}
    details = {"callId": "first", "toolName": "read", "status": "succeeded"}
    pending = {"callId": "second", "toolName": "write", "status": "running"}
    assert run_timeline({"entries": [call, result, message], "toolBatches": [
        {"id": "batch", "calls": [details, pending]},
    ]}) == [
        {"type": "tool", "callId": "first", "name": "read", "batchId": "batch", "call": details,
         "callEntry": call, "resultEntry": result},
        {"type": "message", "role": "assistant", "entry": message},
        {"type": "tool", "callId": "second", "name": "write", "batchId": "batch", "call": pending,
         "callEntry": None, "resultEntry": None},
    ]

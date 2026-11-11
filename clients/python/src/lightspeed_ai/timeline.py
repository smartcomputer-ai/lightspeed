from typing import Literal, TypedDict

from .types import ContextEntryView, ContextMessageRoleView, RunView, ToolCallView


class MessageItem(TypedDict):
    type: Literal["message"]
    role: ContextMessageRoleView
    entry: ContextEntryView


class ContextItem(TypedDict):
    type: Literal["reasoning", "other"]
    entry: ContextEntryView


class ToolItem(TypedDict):
    type: Literal["tool"]
    callId: str
    name: str
    batchId: str | None
    call: ToolCallView | None
    callEntry: ContextEntryView | None
    resultEntry: ContextEntryView | None


type RunTimelineItem = MessageItem | ContextItem | ToolItem


def run_timeline(run: RunView) -> list[RunTimelineItem]:
    """Merge a run's context and tool calls in order of appearance."""
    views = {
        call["callId"]: (batch["id"], call)
        for batch in run.get("toolBatches", [])
        for call in batch.get("calls", [])
    }
    items: list[RunTimelineItem] = []
    tools: dict[str, ToolItem] = {}

    def tool_for(call_id: str, name: str) -> ToolItem:
        if call_id not in tools:
            view = views.get(call_id)
            item: ToolItem = {
                "type": "tool", "callId": call_id, "name": view[1]["toolName"] if view else name,
                "batchId": view[0] if view else None, "call": view[1] if view else None,
                "callEntry": None, "resultEntry": None,
            }
            tools[call_id] = item
            items.append(item)
        return tools[call_id]

    for entry in run.get("entries", []):
        kind = entry["kind"]
        match kind["type"]:
            case "message":
                items.append({"type": "message", "role": kind["role"], "entry": entry})
            case "reasoningState":
                items.append({"type": "reasoning", "entry": entry})
            case "toolCall":
                tool_for(kind["callId"], kind["name"])["callEntry"] = entry
            case "toolResult":
                tool_for(kind["callId"], "unknown")["resultEntry"] = entry
            case _:
                items.append({"type": "other", "entry": entry})
    for call_id, (_, call) in views.items():
        tool_for(call_id, call["toolName"])
    return items

"""Map selected run evidence onto a fixed diagram of the hosted runtime."""

from dataclasses import dataclass

import plotly.graph_objects as go

from demo import Turn

NODES = {
    "sdk": ("Python SDK", 0.7, 3.3),
    "gateway": ("RPC gateway", 3.1, 3.3),
    "temporal": ("Temporal", 5.3, 3.3),
    "worker": ("Worker / harness", 7.6, 3.3),
    "model": ("Model provider", 10.4, 3.3),
    "db": ("PostgreSQL", 3.1, 1.3),
    "vfs": ("VFS", 7.6, 1.3),
    "cas": ("Content store", 10.4, 1.3),
    "routing": ("Environment routing", 5.3, 0.1),
    "daemon": ("Daemon / machine", 10.4, 0.1),
}

# The two SDK arrows are offset to make submission and observation legible.
EDGES = {
    "rpc": ("sdk", "gateway", "request", (0, 0.16)),
    "reply": ("gateway", "sdk", "events / reply", (0, -0.16)),
    "admit": ("gateway", "temporal", "signal", (0, 0)),
    "dispatch": ("temporal", "worker", "tasks", (0, 0)),
    "generate": ("worker", "model", "inference", (0, 0)),
    "read": ("gateway", "db", "read events", (0, 0)),
    "append": ("worker", "db", "append events", (0, 0)),
    "files": ("worker", "vfs", "file tools", (0, 0)),
    "blobs": ("vfs", "cas", "file bytes", (0, 0)),
    "payloads": ("worker", "cas", "payloads", (0, 0)),
    "route": ("worker", "routing", "process tools", (0, 0)),
    "execute": ("routing", "daemon", "daemon I/O", (0, 0)),
}

# State is based on an SDK request or a durable event of this turn.
# A specific downstream hop inferred from that evidence is shown dashed.
OBSERVED = "#00796b"
INFERRED = "#935b04"
NEUTRAL = "#8c9bab"
INK = "#21354b"
_EXEC_TOOLS = {"execcommand", "exec_command", "writestdin", "write_stdin", "shell", "bash", "process_exec", "process_write"}
_VFS_BYTE_TOOLS = {"vfsread", "vfswrite", "vfsedit", "vfs_read_file", "vfs_write_file", "vfs_edit_file"}


@dataclass(frozen=True)
class Step:
    time: int
    label: str
    edges: dict[str, str]


def steps_for(turn: Turn) -> list[Step]:
    steps = []
    for call in turn.calls:
        edges = {"rpc": "observed"}
        if call.outcome.startswith(("HTTP ", "RPC ")):
            edges["reply"] = "observed"
        if call.method == "session/events/read" and call.outcome == "HTTP 200":
            edges["read"] = "inferred"
        steps.append(Step(call.observed_at_ms, f"{call.method} · {call.outcome}", edges))

    tools: dict[str, tuple[str, str]] = {}
    for event in turn.events:
        kind = event["kind"]
        name = kind["type"]
        edges = {"append": "inferred"}
        if name == "runAccepted":
            edges["admit"] = "inferred"
        elif name in {"runStarted", "turnStarted"}:
            edges["dispatch"] = "inferred"
        elif name in {"turnGenerationRequested", "turnGenerationCompleted"}:
            edges.update(dispatch="inferred", generate="inferred")
        elif name == "toolBatchStarted":
            tools.update({
                call["callId"]: (call["toolName"], call.get("toolId") or "") for call in kind["calls"]
            })
        elif name in {"toolCallStarted", "toolCallCompleted"}:
            tool, tool_id = tools.get(kind["callId"], ("", ""))
            names = {tool.lower(), tool_id.lower()}
            if tool:
                name += f" · {tool}"
            if any(value.startswith("vfs") for value in names):
                edges["files"] = "inferred"
            if kind["type"] == "toolCallCompleted" and names & _VFS_BYTE_TOOLS:
                edges["blobs"] = "inferred"
            if names & _EXEC_TOOLS:
                edges.update(route="inferred", execute="inferred")
        elif name in {"runCompleted", "runFailed", "runCancelled"}:
            edges["dispatch"] = "inferred"
        steps.append(Step(event["observedAtMs"], f"#{event['cursor']['seq']} {name}", edges))
    if turn.detail and (turn.detail.get("output") or {}).get("blobRef"):
        steps.append(Step(turn.detail.get("finishedAtMs") or 0, "Terminal output reference", {"payloads": "inferred"}))
    return sorted(steps, key=lambda step: step.time)


def diagram(steps: list[Step], selected: str, route: str | None = None) -> go.Figure:
    figure = go.Figure()
    states = {edge: state for step in steps for edge, state in step.edges.items()}
    current = set(steps[-1].edges) if steps else set()
    edges = dict(EDGES)
    if route == "external":
        edges["direct"] = ("worker", "daemon", "direct daemon I/O", (0, 0))
        states.pop("route", None)
        if state := states.pop("execute", None):
            states["direct"] = state
        current.discard("route")
        if "execute" in current:
            current.remove("execute")
            current.add("direct")
    active: set[str] = set()
    for key, (origin, target, label, (ox, oy)) in edges.items():
        state = states.get(key)
        color = {"observed": OBSERVED, "inferred": INFERRED}.get(state, NEUTRAL)
        if state:
            active.update((origin, target))
        _, x1, y1 = NODES[origin]
        _, x2, y2 = NODES[target]
        dx, dy = x2 - x1, y2 - y1
        length = (dx * dx + dy * dy) ** 0.5
        start_x, start_y = x1 + ox + 0.31 * dx / length, y1 + oy + 0.16 * dy / length
        end_x, end_y = x2 + ox - 0.35 * dx / length, y2 + oy - 0.24 * dy / length
        figure.add_shape(
            type="line", x0=start_x, x1=end_x, y0=start_y, y1=end_y, layer="below",
            line={"color": color, "width": 3.0 if key in current else 1.8 if state else 1.0, "dash": "dash" if state == "inferred" else "solid"},
        )
        figure.add_annotation(
            x=end_x, y=end_y, ax=end_x - 0.13 * dx / length, ay=end_y - 0.13 * dy / length,
            xref="x", yref="y", axref="x", ayref="y", text="", showarrow=True,
            arrowcolor=color, arrowhead=2, arrowwidth=1.4,
        )
        figure.add_annotation(
            x=(start_x + end_x) / 2, y=(start_y + end_y) / 2,
            xshift={"read": 29, "files": -26, "payloads": 13, "direct": 10}.get(key, 0),
            yshift={"rpc": -1, "reply": -12, "files": 0, "direct": 8}.get(key, 7),
            text=label, showarrow=False, font={"size": 10, "color": INK},
            bgcolor="#f4f7fa",
        )

    figure.add_trace(go.Scatter(
        x=[node[1] for node in NODES.values()], y=[node[2] for node in NODES.values()],
        text=[node[0] for node in NODES.values()], customdata=list(NODES),
        mode="markers+text", textposition="top center", textfont={"size": 12, "color": INK},
        marker={
            "size": [21 if key == selected else 16 for key in NODES],
            "symbol": ["square" if key in {"db", "vfs", "cas"} else "circle" for key in NODES],
            "color": [INK if key == selected else "#418b85" if key in active else "#c9d5e2" for key in NODES],
            "line": {"color": "#ffffff", "width": 2},
        },
        selected={"marker": {"opacity": 1}}, unselected={"marker": {"opacity": 1}},
        hovertemplate="%{text}<extra></extra>",
    ))
    figure.update_layout(
        height=375, xaxis={"range": [-0.6, 11.8], "visible": False, "fixedrange": True},
        yaxis={"range": [-0.45, 4.02], "visible": False, "fixedrange": True},
        plot_bgcolor="#f4f7fa", paper_bgcolor="#f4f7fa", margin={"l": 8, "r": 8, "t": 12, "b": 8},
        showlegend=False, clickmode="event+select",
    )
    return figure

"""Run from this directory with uv run --locked streamlit run app.py."""

import asyncio
from dataclasses import asdict
from pathlib import Path
from urllib.parse import quote

import streamlit as st

from lightspeed_ai import LightspeedRpcError, LightspeedTransportError, run_timeline

from demo import (
    MAX_PREVIEW_BYTES, MAX_TURNS, Chat, Gateway, Settings, Turn, postgres_state, temporal_history, workspace_files,
)
from flow import NODES, diagram, steps_for
from local_admin import LOOPBACK, load_local_admin, start_handoff

st.set_page_config(page_title="Lightspeed internals", layout="wide")
EXAMPLE = Path(__file__).resolve().parent
settings = Settings.load(EXAMPLE)
gateway = Gateway(settings)


def rpc_message(error: LightspeedRpcError | LightspeedTransportError) -> str:
    if isinstance(error, LightspeedRpcError):
        return f"{error.kind} · {error.message[:300]}"
    return f"Gateway request failed{f' (HTTP {error.status})' if error.status else ''}."


def submit(chat: Chat, turn: Turn) -> None:
    turn.error = None
    turn.can_retry = False
    try:
        gateway.submit(chat, turn)
    except LightspeedTransportError as error:
        turn.error = rpc_message(error)
        turn.can_retry = True
        turn.status = "submission unconfirmed"
    except LightspeedRpcError as error:
        turn.error = rpc_message(error)
        turn.status = "rejected"


def show_chat(chat: Chat) -> None:
    st.subheader("Session chat")
    with st.container(height=490):
        if not chat.turns:
            st.info("Send a prompt to start a Lightspeed session.")
        for turn in chat.turns:
            with st.chat_message("user"):
                st.text(turn.prompt)
            with st.chat_message("assistant"):
                if turn.detail and turn.detail.get("outputText"):
                    st.markdown(turn.detail["outputText"])
                elif turn.detail and turn.detail.get("failure"):
                    st.error(turn.detail["failure"].get("message", turn.status))
                else:
                    st.write(turn.status.capitalize())
                if turn.error:
                    st.warning(turn.error)

    turn = chat.latest
    if turn and turn.can_retry and st.button("Retry same submission", key="retry_submit"):
        submit(chat, turn)
        st.rerun()
    if turn and turn.active and turn.observation_paused and st.button("Resume observation"):
        turn.observation_paused = False
        turn.error = None
        st.rerun()
    if turn and turn.active and st.button("Cancel run"):
        try:
            gateway.cancel(chat, turn)
            turn.observation_paused = False
            turn.error = None
        except (LightspeedRpcError, LightspeedTransportError) as error:
            turn.error = rpc_message(error)
        st.rerun()

    locked = turn is not None and (turn.active or turn.can_retry)
    prompt = st.chat_input("Message Lightspeed", disabled=locked)
    if prompt and prompt.strip():
        turn = Turn(prompt.strip(), cursor=chat.cursor)
        chat.turns.append(turn)
        del chat.turns[:-MAX_TURNS]
        submit(chat, turn)
        st.rerun()


def panel_result(key: str, load, *, refresh: bool = False):
    """Keep inspection separate from the selected turn's SDK timings."""
    cache = st.session_state.inspections
    if refresh or key not in cache:
        try:
            cache[key] = load(), None
        except Exception as error:
            # Details from infrastructure clients can contain connection strings.
            cache[key] = None, type(error).__name__
    return cache[key]


def read_panel(key: str, load):
    refresh = st.button("Refresh evidence", key=f"refresh:{key}")
    data, error = panel_result(key, load, refresh=refresh)
    if error:
        st.warning(f"Evidence unavailable ({error}). Check the local service, configuration, and access.")
    return data


def show_temporal(chat: Chat, universe: str | None) -> None:
    st.caption("Session workflow history · may include activity from other turns.")
    if not universe or not settings.temporal_address:
        st.info("Configure a local Temporal address and a universe to read workflow history.")
        return
    workflow_id = f"{universe}/{chat.session_id}"
    st.code(workflow_id, language=None)
    if settings.temporal_ui:
        namespace = quote(settings.temporal_namespace, safe="")
        st.link_button("Open Temporal UI", f"{settings.temporal_ui.rstrip('/')}/namespaces/{namespace}/workflows")
    rows = read_panel("temporal", lambda: asyncio.run(temporal_history(settings, workflow_id)))
    if rows:
        st.dataframe(rows, hide_index=True)
    elif rows is not None:
        st.caption("No activity records in the inspected history window.")


def show_db(chat: Chat, universe: str | None) -> None:
    if not universe or not settings.postgres_dsn:
        st.info("Set LIGHTSPEED_DEMO_POSTGRES_DSN to enable the read-only metadata view.")
        return
    rows = read_panel("db", lambda: asyncio.run(postgres_state(settings, universe, chat.session_id)))
    if rows is not None:
        st.caption("Session catalog and latest checkpoint")
        st.json({"session": rows["session"], "checkpoint": rows["checkpoint"]})
        if rows["events"]:
            st.caption("Recent durable records")
            st.dataframe(rows["events"], hide_index=True)


def show_vfs(chat: Chat) -> None:
    def attachments():
        session = gateway.session(chat.session_id)
        feature = (session.get("config") or {}).get("features", {}).get("vfs") or {}
        return feature.get("workspaces", [])

    mounts = read_panel("vfs_mounts", attachments)
    if mounts is None:
        return
    if not mounts:
        st.info("This session has no VFS workspace attachments.")
        return
    index = st.selectbox(
        "Workspace attachment", range(len(mounts)),
        format_func=lambda index: f"{mounts[index]['path']} · {mounts[index].get('workspaceId') or 'Snapshot'}",
    )
    mount = mounts[index]
    def load_workspace():
        head, manifest = gateway.workspace(mount)
        return head, workspace_files(manifest)

    workspace = read_panel(f"workspace:{index}:{mount['path']}", load_workspace)
    if workspace is None:
        return
    head, files = workspace
    st.json({"attachment": mount, "head": head}, expanded=False)
    if not files:
        st.caption("This snapshot has no files.")
        return
    st.dataframe(files, column_order=["path", "bytes", "media"], hide_index=True)
    eligible = {item["path"]: item["ref"] for item in files if item["bytes"] <= MAX_PREVIEW_BYTES}
    if not eligible:
        st.caption("The files in this snapshot exceed the preview limit.")
        return
    path = st.selectbox("Preview a small UTF-8 file", list(eligible))
    if st.button("Read preview"):
        try:
            st.code(gateway.preview(eligible[path]), language=None)
        except (ValueError, LightspeedRpcError, LightspeedTransportError):
            st.warning("This file could not be previewed as UTF-8.")


def session_environments(chat: Chat):
    return gateway.environments(gateway.session(chat.session_id))


def show_environment(chat: Chat, turn: Turn | None) -> None:
    rows = read_panel("environments", lambda: session_environments(chat))
    if rows:
        st.json(rows, expanded=1)
    elif rows is not None:
        st.info("No environments are attached to this session.")
    st.caption("External daemons are dialed directly. Registered daemons connect through the environment gateway.")
    if turn and turn.detail:
        calls = [
            {
                "name": item["name"], "status": item["call"]["status"],
                "durationMs": item["call"].get("durationMs"),
            }
            for item in run_timeline(turn.detail) if item["type"] == "tool" and item["call"]
        ]
        if calls:
            st.caption("Tool calls in the selected run")
            st.dataframe(calls, hide_index=True)


def show_details(node: str, chat: Chat, turn: Turn | None) -> None:
    st.markdown(f"**{NODES[node][0]} evidence**")
    if node in {"sdk", "gateway"}:
        if turn and turn.calls:
            st.dataframe([asdict(call) for call in turn.calls[-20:]], hide_index=True)
        else:
            st.caption("Send a message to capture SDK request timings.")
    elif node in {"worker", "model"}:
        if turn and turn.events:
            st.dataframe([
                {"seq": event["cursor"]["seq"], "event": event["kind"]["type"], "time_ms": event["observedAtMs"]}
                for event in turn.events[-30:]
                if node == "worker" or event["kind"]["type"].startswith("turnGeneration")
            ], hide_index=True)
        if turn and turn.detail:
            st.json({"status": turn.detail["status"], "usage": turn.detail.get("usage")}, expanded=False)
    elif not chat.opened:
        st.caption("Start or reopen a session to inspect this component.")
    elif node == "temporal":
        show_temporal(chat, st.session_state.universe)
    elif node == "db":
        show_db(chat, st.session_state.universe)
    elif node == "vfs":
        show_vfs(chat)
    elif node == "cas":
        output = turn.detail.get("output") if turn and turn.detail else None
        if output:
            st.json(output)
        else:
            st.caption("The run has no visible terminal output reference yet.")
    else:
        show_environment(chat, turn)


def show_flow(chat: Chat) -> None:
    st.subheader("Run data flow")
    turns = {turn.submission_id: turn for turn in reversed(chat.turns)}
    follow_latest = st.toggle("Follow latest", value=True)
    turn = chat.latest
    if not follow_latest and turns:
        selected = st.selectbox(
            "Run", list(turns),
            format_func=lambda key: f"{turns[key].run_id or 'Pending'} · {turns[key].prompt[:40]}",
        )
        turn = turns[selected]
    steps = steps_for(turn) if turn else []
    index = len(steps)
    if not follow_latest and len(steps) > 1:
        index = st.slider("Timeline step", 1, len(steps), value=len(steps), key=f"step:{turn.submission_id}")
    step = steps[index - 1] if steps else None
    st.caption(step.label if step else "Select a component to explore the architecture.")
    node = st.session_state.get("node_picker") or "gateway"
    environments, _ = st.session_state.inspections.get("environments", (None, None))
    if node in {"routing", "daemon"} and chat.opened:
        environments, _ = panel_result("environments", lambda: session_environments(chat))
    route = next((item["route"] for item in environments or [] if item["active"]), None)

    map_key = f"map:{chat.session_id}"

    def pick_node():
        points = st.session_state[map_key]["selection"]["points"]
        clicked = points[-1].get("customdata") if points else None
        if isinstance(clicked, str) and clicked in NODES:
            st.session_state.node_picker = clicked

    st.plotly_chart(
        diagram(steps[:index], node, route), key=map_key, on_select=pick_node, selection_mode="points",
        config={"displayModeBar": False}, theme=None, width="stretch",
    )
    st.caption("Teal · observed RPC  Amber dashed · inferred route  Gray · architecture  Bold · selected step")
    node = st.selectbox(
        "Inspect component", list(NODES), format_func=lambda key: NODES[key][0],
        key="node_picker", index=None, placeholder="RPC gateway",
    ) or "gateway"
    show_details(node, chat, turn)


def replace_chat(chat: Chat) -> None:
    st.session_state.chat = chat
    st.session_state.inspections = {}
    st.session_state.session_to_open = chat.session_id if chat.opened else ""


@st.cache_resource
def admin_handoff():
    return start_handoff()


def show_platform_link(universe: str | None = None) -> None:
    if not settings.platform_url:
        return
    url = settings.platform_url
    streamlit_host = f"{LOOPBACK}:{st.get_option('server.port')}"
    if (
        st.get_option("server.address") == LOOPBACK
        and st.context.headers.get("Host") == streamlit_host
        and st.context.headers.get("Origin") == f"http://{streamlit_host}"
    ):
        admin = load_local_admin(EXAMPLE.parents[1], settings.endpoint, url, universe)
        if admin:
            try:
                url = admin_handoff().issue(admin)
            except OSError:
                st.sidebar.caption("Local admin sign-in is unavailable.")
    st.sidebar.link_button("Open Lightspeed", url, width="stretch")


st.title("Lightspeed internals")
if not settings.api_key:
    show_platform_link()
    st.info("Run ./init-env.sh to prepare Lightspeed, then set LIGHTSPEED_API_KEY in this example's .env file.")
    st.stop()

if "universe" not in st.session_state:
    try:
        st.session_state.universe = gateway.identify_universe()
    except (LightspeedRpcError, LightspeedTransportError) as error:
        show_platform_link()
        st.error(rpc_message(error))
        st.button("Retry connection")
        st.stop()

show_platform_link(st.session_state.universe)
if "chat" not in st.session_state:
    replace_chat(Chat())


def new_session():
    replace_chat(Chat())
    st.session_state.pop("open_error", None)


def open_session():
    session_id = st.session_state.session_to_open.strip()
    if session_id:
        try:
            replace_chat(gateway.open_session(session_id))
            st.session_state.pop("open_error", None)
        except (LightspeedRpcError, LightspeedTransportError) as error:
            st.session_state.open_error = rpc_message(error)


with st.sidebar:
    st.button("New session", on_click=new_session)
    with st.form("open_session"):
        st.text_input("Session ID", key="session_to_open")
        st.form_submit_button("Open session", on_click=open_session)
    if error := st.session_state.get("open_error"):
        st.warning(error)
    if st.session_state.chat.opened:
        st.caption("Current session")
        st.code(st.session_state.chat.session_id, language=None)

chat = st.session_state.chat
observing = chat.latest is not None and chat.latest.observing


@st.fragment(run_every=2 if observing else None)
def workspace() -> None:
    if chat.latest and chat.latest.observing:
        turn = chat.latest
        try:
            gateway.poll(chat, turn)
            turn.error = None
        except (LightspeedRpcError, LightspeedTransportError) as error:
            turn.error = rpc_message(error)
            if isinstance(error, LightspeedRpcError):
                # The SDK has stopped observing. The run may still exist.
                turn.status = "observation stopped"
                turn.observation_paused = True
        if not turn.observing:
            st.rerun()
    left, right = st.columns([2, 3], gap="large")
    with left:
        show_chat(chat)
    with right:
        show_flow(chat)


workspace()

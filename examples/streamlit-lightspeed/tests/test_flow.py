from demo import Rpc, Turn, workspace_files
from flow import INFERRED, OBSERVED, diagram, steps_for


def event(seq, kind, **fields):
    return {"cursor": {"seq": seq}, "observedAtMs": seq * 10, "kind": {"type": kind, **fields}}


def test_map_distinguishes_rpc_observation_from_inferred_hops():
    turn = Turn("Show the change log")
    turn.calls.append(Rpc("session/events/read", "req", 3.0, "HTTP 200", 31))
    turn.observe(event(1, "turnGenerationRequested"))
    turn.observe(event(2, "toolBatchStarted", calls=[{"callId": "call", "toolName": "vfs_read_file"}]))
    turn.observe(event(3, "toolCallCompleted", callId="call"))
    turn.observe(event(3, "toolCallCompleted", callId="call"))
    steps = steps_for(turn)
    assert len(steps) == 4
    assert steps[0].edges["generate"] == "inferred"
    assert steps[2].edges["files"] == "inferred"
    assert steps[2].edges["blobs"] == "inferred"
    assert steps[3].edges == {"rpc": "observed", "reply": "observed", "read": "inferred"}
    figure = diagram(steps, "db")
    assert {"db", "vfs", "daemon"} <= set(figure.data[0].customdata)
    assert {OBSERVED, INFERRED} <= {shape.line.color for shape in figure.layout.shapes}
    assert all("execute" not in step.edges for step in steps)


def test_vfs_listing_uses_manifest_paths_and_blob_metadata():
    manifest = {
        "schema_version": "lightspeed.vfs.snapshot.v1",
        "root": {"entries": {"docs": {"kind": "directory", "entries": {
            "notes.md": {"kind": "file", "size_bytes": 12, "blob_ref": "sha256:notes", "media_type": "text/markdown"},
        }}}},
    }
    assert workspace_files(manifest) == [
        {"path": "docs/notes.md", "bytes": 12, "ref": "sha256:notes", "media": "text/markdown"},
    ]


def test_direct_environment_route_uses_the_tool_name_even_with_a_registry_id():
    turn = Turn("Run a command")
    turn.observe(event(1, "toolBatchStarted", calls=[
        {"callId": "call", "toolName": "exec_command", "toolId": "registry.exec"},
    ]))
    turn.observe(event(2, "toolCallCompleted", callId="call"))
    steps = steps_for(turn)
    figure = diagram(steps, "daemon", "external")
    assert steps[-1].edges["execute"] == "inferred"
    assert figure.layout.shapes[-1].line.color == INFERRED
    assert figure.layout.shapes[-1].line.width == 3.0

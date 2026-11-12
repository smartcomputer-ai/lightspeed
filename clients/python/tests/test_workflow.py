import pytest

from lightspeed_ai.workflow import (
    CHANNEL_CONNECTOR_ACTIVITIES,
    DELIVER_EMISSION_SIGNAL,
    REPLY_COMPLETION_KEY,
    WORKFLOW_CONTRACT_VECTORS,
    WORKFLOW_TOOL_RECOVERY_QUERY,
    connector_task_queue,
    environment_job_workflow_id,
    invocation_cancellation_emission_id,
    parse_emission_envelope,
    recipe_fingerprint,
    reply_promise_id,
    run_terminal_emission_id,
    session_workflow_id,
    source_resolution_emission_id,
    source_resolution_envelope,
    split_workflow_id,
    tool_invocation_emission_id,
)

VECTORS = WORKFLOW_CONTRACT_VECTORS
INPUTS = VECTORS["inputs"]
RUN_TERMINAL, SOURCE_RESOLUTION = VECTORS["envelopes"][0], VECTORS["envelopes"][1]


def resolution_envelope(**overrides):
    return source_resolution_envelope(**{
        "universe_id": INPUTS["universeId"], "producer_workflow_id": INPUTS["producerWorkflowId"],
        "holder_workflow_id": INPUTS["holderWorkflowId"], "promise_id": INPUTS["promiseId"],
        "resolution": {"kind": "resolved", "payload_ref": None}, **overrides,
    })


def test_matches_every_rust_derivation_vector():
    ids = VECTORS["emissionIds"]
    assert run_terminal_emission_id(
        INPUTS["universeId"], INPUTS["sessionId"], INPUTS["runId"], INPUTS["token"],
    ) == ids["runTerminal"]
    assert source_resolution_emission_id(
        INPUTS["universeId"], INPUTS["producerWorkflowId"], INPUTS["holderWorkflowId"], INPUTS["promiseId"],
    ) == ids["sourceResolution"]
    assert tool_invocation_emission_id(INPUTS["invocationId"]) == ids["toolInvocation"]
    assert invocation_cancellation_emission_id(
        INPUTS["invocationId"], INPUTS["completionKey"],
    ) == ids["invocationCancellation"]
    assert recipe_fingerprint(INPUTS["recipeJson"]) == VECTORS["recipeFingerprint"]
    assert recipe_fingerprint(INPUTS["recipeJson"].encode()) == VECTORS["recipeFingerprint"]
    assert session_workflow_id(INPUTS["universeId"], INPUTS["sessionId"]) == VECTORS["workflowIds"]["session"]
    assert environment_job_workflow_id(
        INPUTS["universeId"], INPUTS["environmentId"], INPUTS["jobGroupId"],
    ) == VECTORS["workflowIds"]["environmentJob"]


def test_splits_only_session_workflow_ids():
    expected = VECTORS["workflowIds"]["split"]
    split = split_workflow_id(VECTORS["workflowIds"]["session"])
    assert split == (expected["universeId"], expected["sessionId"])
    assert (split.universe_id, split.session_id) == (expected["universeId"], expected["sessionId"])
    for other in ("no-separator", f"{INPUTS['universeId']}/a/b", "not-a-uuid/session", f"{INPUTS['universeId']}/"):
        assert split_workflow_id(other) is None


def test_derives_the_connector_task_queue_of_a_channel_account():
    channel = VECTORS["channels"]["inputs"]
    expected = VECTORS["channels"]["connectorTaskQueue"]
    assert connector_task_queue(channel["universeId"], channel["provider"], channel["accountId"]) == expected
    assert connector_task_queue(
        channel["universeId"].upper(), channel["provider"], channel["accountId"],
    ) == expected
    assert connector_task_queue(channel["universeId"], "whatsapp", channel["accountId"]) != expected
    with pytest.raises(ValueError, match="UUID"):
        connector_task_queue("not-a-uuid", "telegram", "tg-main")
    assert CHANNEL_CONNECTOR_ACTIVITIES == {
        "deliverChannelMessage": "deliverChannelMessage",
        "prepareChannelMedia": "prepareChannelMedia",
        "maintainChannelTyping": "maintainChannelTyping",
    }


def test_exports_manifest_owned_names_and_the_reply_convention():
    assert DELIVER_EMISSION_SIGNAL == "deliver_emission"
    assert WORKFLOW_TOOL_RECOVERY_QUERY == "workflow_tool_recovery"
    assert REPLY_COMPLETION_KEY == "reply"
    invocation = VECTORS["startArgs"]["invocation"]
    assert reply_promise_id(invocation) == INPUTS["promiseId"]
    for promises in (None, {}, {"other": "promise_1"}, {"reply": "promise_1", "other": "promise_2"}, {"reply": ""}):
        with pytest.raises(ValueError, match="reply"):
            reply_promise_id({**invocation, "completion_promises": promises})


def test_parses_every_rust_envelope_vector():
    for envelope in VECTORS["envelopes"]:
        assert parse_emission_envelope(envelope) is envelope


def test_validates_the_encoding_descriptor_on_terminal_output():
    described = {"media_type": "application/json", "provider_kind": "anthropic.messages.text_blocks"}
    with pytest.raises(ValueError, match="canonical blob ref"):
        parse_emission_envelope({
            **RUN_TERMINAL, "body": {**RUN_TERMINAL["body"], "output": {"content_ref": "not-a-blob", **described}},
        })
    with pytest.raises(ValueError, match="content_ref"):
        parse_emission_envelope({
            **RUN_TERMINAL, "body": {**RUN_TERMINAL["body"], "output": {"media_type": "application/json"}},
        })


def test_constructs_canonical_source_resolutions():
    assert resolution_envelope() == SOURCE_RESOLUTION
    assert resolution_envelope(universe_id=INPUTS["universeId"].upper()) == SOURCE_RESOLUTION


@pytest.mark.parametrize("promise_id", ["reply", "promise_", "promise_01", "promise_x", "promise_1\n"])
def test_refuses_a_promise_id_the_holder_could_not_decode(promise_id):
    with pytest.raises(ValueError, match="promise_id has an invalid format"):
        resolution_envelope(promise_id=promise_id)
    with pytest.raises(ValueError, match="promise_id has an invalid format"):
        parse_emission_envelope({
            **SOURCE_RESOLUTION, "body": {**SOURCE_RESOLUTION["body"], "promise_id": promise_id},
        })


def test_accepts_the_contracts_run_statuses_and_nothing_else():
    def with_status(status):
        return parse_emission_envelope({**RUN_TERMINAL, "body": {**RUN_TERMINAL["body"], "status": status}})

    for status in ("running", "parked", "cancelling", "completed", "failed", "cancelled"):
        with_status(status)
    with pytest.raises(ValueError, match="invalid run status"):
        with_status("active")


def test_rejects_unknown_bodies_non_canonical_ids_and_non_integers():
    with pytest.raises(ValueError, match="unknown emission body kind"):
        parse_emission_envelope({**RUN_TERMINAL, "body": {"kind": "future_protocol_shape"}})
    with pytest.raises(ValueError, match="invalid format"):
        parse_emission_envelope({**RUN_TERMINAL, "emission_id": "emission:sha256:ABC"})
    with pytest.raises(ValueError, match="must be an object"):
        parse_emission_envelope([RUN_TERMINAL])
    for run_id in (True, -1, 1.0, 2**53, "7"):
        with pytest.raises(ValueError, match="run_id must be a non-negative safe integer"):
            parse_emission_envelope({**RUN_TERMINAL, "body": {**RUN_TERMINAL["body"], "run_id": run_id}})
        with pytest.raises(ValueError, match="run_id must be a non-negative safe integer"):
            run_terminal_emission_id(INPUTS["universeId"], INPUTS["sessionId"], run_id, INPUTS["token"])


def test_temporals_data_converter_decodes_the_generated_types():
    # A workflow or activity that names these types in its signature has its
    # arguments decoded from the type hints, before any of its code runs.
    from temporalio.converter import value_to_type

    from lightspeed_ai import AwaitRunResult, RunTimelineItem, types, workflow_types

    assert value_to_type(workflow_types.WorkflowToolStartArgs, VECTORS["startArgs"]) == VECTORS["startArgs"]
    assert value_to_type(
        workflow_types.WorkflowToolRecoveryResult, VECTORS["recoveryResult"],
    ) == VECTORS["recoveryResult"]
    for envelope in VECTORS["envelopes"]:
        assert value_to_type(workflow_types.EmissionEnvelope, envelope) == envelope

    event = {
        "cursor": {"seq": 1}, "sessionId": "s", "observedAtMs": 1, "joins": {"runId": "1"},
        "kind": {"type": "runFailed", "runId": "1", "kind": "modelFailure", "message": "no"},
    }
    page = {"events": [event], "complete": True, "nextCursor": {"seq": 1}}
    assert value_to_type(types.SessionEventsReadResponse, page) == page
    result = {"run": {"id": "1", "status": "completed"}, "cursor": {"seq": 1}}
    assert value_to_type(AwaitRunResult, result) == result
    item = {"type": "message", "role": "assistant", "entry": {"kind": {"type": "message", "role": "assistant"}}}
    assert value_to_type(list[RunTimelineItem], [item]) == [item]

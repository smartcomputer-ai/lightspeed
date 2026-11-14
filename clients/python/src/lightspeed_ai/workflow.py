"""Helpers for Temporal workflow code that takes part in a Lightspeed session.

A workflow that receives tool calls from an integrated session, or answers
them, uses this module for the contract's names, IDs and envelopes. It depends
on no Temporal package and does nothing but hash, compare and validate, so it
is deterministic. `lightspeed_ai.workflow_types` holds the contract's types.

Every function raises `ValueError` for a value the contract does not allow.
"""

from __future__ import annotations

import hashlib
import re
from collections.abc import Mapping
from typing import Any, NamedTuple, cast

from ._workflow_manifest import WORKFLOW_CONTRACT_MANIFEST
from .workflow_types import EmissionEnvelope, PromiseResolution, WorkflowToolInvocation

__all__ = [
    "CHANNEL_CONNECTOR_ACTIVITIES",
    "CHANNEL_CONVERSATION_WORKFLOW_KIND",
    "CHANNEL_DELIVERY_RECEIPT_SIGNAL",
    "CHANNEL_INBOUND_SIGNAL",
    "CHANNEL_STATE_QUERY",
    "DELIVER_EMISSION_SIGNAL",
    "REPLY_COMPLETION_KEY",
    "WORKFLOW_CONTRACT_MANIFEST",
    "WORKFLOW_CONTRACT_VECTORS",
    "WORKFLOW_TOOL_EXECUTION_KIND",
    "WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX",
    "WORKFLOW_TOOL_RECIPE_FORMAT_V1",
    "WORKFLOW_TOOL_RECOVERY_QUERY",
    "SessionWorkflowId",
    "connector_task_queue",
    "environment_job_workflow_id",
    "invocation_cancellation_emission_id",
    "parse_emission_envelope",
    "recipe_fingerprint",
    "reply_promise_id",
    "run_terminal_emission_id",
    "session_workflow_id",
    "source_resolution_emission_id",
    "source_resolution_envelope",
    "split_workflow_id",
    "tool_invocation_emission_id",
]

DELIVER_EMISSION_SIGNAL: str = WORKFLOW_CONTRACT_MANIFEST["signals"]["deliverEmission"]
WORKFLOW_TOOL_RECOVERY_QUERY: str = WORKFLOW_CONTRACT_MANIFEST["queries"]["workflowToolRecovery"]
WORKFLOW_TOOL_EXECUTION_KIND: str = WORKFLOW_CONTRACT_MANIFEST["workflowTools"]["executionKind"]
WORKFLOW_TOOL_RECIPE_FORMAT_V1: str = WORKFLOW_CONTRACT_MANIFEST["workflowTools"]["recipeFormatV1"]
WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX: str = (
    WORKFLOW_CONTRACT_MANIFEST["workflowTools"]["recipeFingerprintPrefix"]
)
REPLY_COMPLETION_KEY: str = WORKFLOW_CONTRACT_MANIFEST["workflowTools"]["replyCompletionKey"]

CHANNEL_CONNECTOR_ACTIVITIES: dict[str, str] = WORKFLOW_CONTRACT_MANIFEST["channels"]["connectorActivities"]
"""Activity names a connector host registers on its account task queues."""
CHANNEL_CONVERSATION_WORKFLOW_KIND: str = WORKFLOW_CONTRACT_MANIFEST["channels"]["workflowKind"]
CHANNEL_INBOUND_SIGNAL: str = WORKFLOW_CONTRACT_MANIFEST["channels"]["inboundSignal"]
CHANNEL_STATE_QUERY: str = WORKFLOW_CONTRACT_MANIFEST["channels"]["stateQuery"]
CHANNEL_DELIVERY_RECEIPT_SIGNAL: str = WORKFLOW_CONTRACT_MANIFEST["channels"]["deliveryReceiptSignal"]

WORKFLOW_CONTRACT_VECTORS: dict[str, Any] = WORKFLOW_CONTRACT_MANIFEST["vectors"]
"""Known-answer vectors emitted by Rust and shared by every generated consumer."""

_EMISSION_ID = re.compile(r"(?:emission|wti):sha256:[0-9a-f]{64}")
_INVOCATION_ID = re.compile(r"wti:sha256:[0-9a-f]{64}")
_BLOB_REF = re.compile(r"sha256:[0-9a-f]{64}")
_PROMISE_ID = re.compile(r"promise_(?:0|[1-9][0-9]{0,19})")
_UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", re.IGNORECASE)
_SESSION_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:-]*")
_RUN_STATUSES = frozenset(("running", "parked", "cancelling", "completed", "failed", "cancelled"))
# Integers travel as JSON numbers, which other consumers hold as 64-bit floats.
_MAX_SAFE_INTEGER = 2**53 - 1
_REPLY_REQUIRED = "pushed invocation must carry exactly the reserved `reply` completion promise"


class SessionWorkflowId(NamedTuple):
    """The parts of a session's Temporal workflow ID."""

    universe_id: str
    session_id: str


def run_terminal_emission_id(universe_id: str, session_id: str, run_id: int, token: str) -> str:
    """The emission ID of a run's terminal notification."""
    _require_session_id(session_id)
    _require_safe_integer(run_id, "run_id")
    _require_non_empty(token, "token")
    return _derived_emission_id("run_terminal", [
        _normalize_universe_id(universe_id).encode(), session_id.encode(), _u64be(run_id), token.encode(),
    ])


def source_resolution_emission_id(
    universe_id: str, producer_workflow_id: str, holder_workflow_id: str, promise_id: str,
) -> str:
    """The emission ID of one producer resolving one holder's promise.

    Promise IDs are session-scoped counters (`promise_7`), so the holder
    workflow ID is part of the identity: one producer resolving `promise_7`
    for two holders sends two distinct emissions.
    """
    _require_non_empty(producer_workflow_id, "producer_workflow_id")
    _require_non_empty(holder_workflow_id, "holder_workflow_id")
    _require_non_empty(promise_id, "promise_id")
    return _derived_emission_id("source_resolution", [
        _normalize_universe_id(universe_id).encode(), producer_workflow_id.encode(),
        holder_workflow_id.encode(), promise_id.encode(),
    ])


def tool_invocation_emission_id(invocation_id: str) -> str:
    """The emission ID of a tool invocation, which is the invocation ID itself."""
    _require_invocation_id(invocation_id)
    return invocation_id


def invocation_cancellation_emission_id(invocation_id: str, completion_key: str) -> str:
    """The emission ID of cancelling one completion of an invocation."""
    _require_invocation_id(invocation_id)
    _require_non_empty(completion_key, "completion_key")
    return _derived_emission_id("invocation_cancellation", [invocation_id.encode(), completion_key.encode()])


def source_resolution_envelope(
    *,
    universe_id: str,
    producer_workflow_id: str,
    holder_workflow_id: str,
    promise_id: str,
    resolution: PromiseResolution,
) -> EmissionEnvelope:
    """Build the exact envelope a workflow sends back to a Lightspeed holder.

    Args:
        universe_id: The universe both workflows belong to.
        producer_workflow_id: The workflow sending the resolution.
        holder_workflow_id: The Lightspeed session workflow that holds the promise.
        promise_id: The promise being resolved, such as `promise_7`.
        resolution: How it resolved.
    """
    universe = _normalize_universe_id(universe_id)
    _require_non_empty(producer_workflow_id, "producer_workflow_id")
    _require_non_empty(holder_workflow_id, "holder_workflow_id")
    # The holder logs and drops an envelope it cannot decode, so this is the
    # only place the sender can hear about a bad ID.
    if not isinstance(promise_id, str) or not _PROMISE_ID.fullmatch(promise_id):
        raise ValueError("promise_id has an invalid format")
    _parse_resolution(resolution)
    return {
        "emission_id": source_resolution_emission_id(
            universe, producer_workflow_id, holder_workflow_id, promise_id,
        ),
        "producer": {"kind": "workflow", "universe_id": universe, "workflow_id": producer_workflow_id},
        "body": {"kind": "source_resolution", "promise_id": promise_id, "resolution": resolution},
    }


def parse_emission_envelope(value: object) -> EmissionEnvelope:
    """Validate the fixed signal payload at a workflow boundary and return it."""
    envelope = _record(value, "emission envelope")
    _require_pattern(envelope, "emission_id", _EMISSION_ID)
    _parse_producer(envelope.get("producer"))
    _parse_body(envelope.get("body"))
    return cast(EmissionEnvelope, value)


def reply_promise_id(invocation: WorkflowToolInvocation) -> str:
    """The single receiver-visible promise under the reserved `reply` key."""
    promises = invocation.get("completion_promises")
    if not isinstance(promises, Mapping) or list(promises) != [REPLY_COMPLETION_KEY]:
        raise ValueError(_REPLY_REQUIRED)
    return _require_non_empty(promises[REPLY_COMPLETION_KEY], "reply promise")


def session_workflow_id(universe_id: str, session_id: str) -> str:
    """The stable Temporal workflow ID of a Lightspeed session."""
    _require_session_id(session_id)
    return f"{_normalize_universe_id(universe_id)}/{session_id}"


def environment_job_workflow_id(universe_id: str, environment_id: str, job_group_id: str) -> str:
    """The stable Temporal workflow ID of an environment job group."""
    _require_non_empty(environment_id, "environment_id")
    _require_non_empty(job_group_id, "job_group_id")
    return f"{_normalize_universe_id(universe_id)}/envjob-{environment_id}-{job_group_id}"


def connector_task_queue(universe_id: str, provider: str, account_id: str) -> str:
    """The task queue of the connector host serving one channel account.

    It is `lightspeed-connector-{provider}-{24 hex}`, the hex being SHA-256
    over the length-prefixed domain, hyphenated lowercase universe ID,
    provider, and account ID. The core routes connector activities here, and
    the host polls it.
    """
    _require_non_empty(provider, "provider")
    _require_non_empty(account_id, "account_id")
    digest = _framed_sha256([
        WORKFLOW_CONTRACT_MANIFEST["channels"]["domains"]["connectorTaskQueue"].encode(),
        _normalize_universe_id(universe_id).encode(), provider.encode(), account_id.encode(),
    ])
    return f"lightspeed-connector-{provider}-{digest[:24]}"


def split_workflow_id(workflow_id: str) -> SessionWorkflowId | None:
    """Split a canonical session workflow ID, returning None for any other ID."""
    universe_id, separator, session_id = workflow_id.partition("/")
    if not separator or "/" in session_id:
        return None
    try:
        _require_session_id(session_id)
        return SessionWorkflowId(_normalize_universe_id(universe_id), session_id)
    except ValueError:
        return None


def recipe_fingerprint(recipe: str | bytes) -> str:
    """Fingerprint the exact raw recipe bytes with the canonical prefix."""
    data = recipe.encode() if isinstance(recipe, str) else recipe
    return WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX + hashlib.sha256(data).hexdigest()


def _framed_sha256(parts: list[bytes]) -> str:
    digest = hashlib.sha256()
    for part in parts:
        digest.update(_u64be(len(part)))
        digest.update(part)
    return digest.hexdigest()


def _derived_emission_id(kind: str, parts: list[bytes]) -> str:
    emission_ids: dict[str, str] = WORKFLOW_CONTRACT_MANIFEST["emissionIds"]
    return emission_ids["prefix"] + _framed_sha256([emission_ids["hashDomain"].encode(), kind.encode(), *parts])


def _parse_producer(value: object) -> None:
    producer = _record(value, "emission producer")
    kind = _require_string(producer, "kind")
    _normalize_universe_id(_require_string(producer, "universe_id"))
    if kind == "session":
        _require_session_id(_require_string(producer, "session_id"))
        _require_safe_integer(producer.get("log_seq"), "log_seq")
    elif kind == "workflow":
        _require_non_empty(_require_string(producer, "workflow_id"), "producer.workflow_id")
    else:
        raise ValueError(f"unknown emission producer kind: {kind}")


def _parse_body(value: object) -> None:
    body = _record(value, "emission body")
    kind = _require_string(body, "kind")
    match kind:
        case "run_terminal":
            _require_non_empty(_require_string(body, "token"), "run_terminal.token")
            _require_safe_integer(body.get("run_id"), "run_id")
            if body.get("status") not in _RUN_STATUSES:
                raise ValueError(f"invalid run status: {body.get('status')}")
            if body.get("output") is not None:
                output = _record(body["output"], "run_terminal.output")
                _require_string(output, "content_ref")
                _require_optional_blob_ref(output, "content_ref")
                for key in ("media_type", "provider_kind"):
                    if output.get(key) is not None:
                        _require_string(output, key)
            _require_optional_blob_ref(body, "failure_message_ref")
        case "source_resolution":
            _require_pattern(body, "promise_id", _PROMISE_ID)
            _parse_resolution(body.get("resolution"))
        case "tool_invocation":
            _parse_invocation(body.get("invocation"))
            _require_non_empty(
                _require_string(body, "holder_workflow_id"), "tool_invocation.holder_workflow_id",
            )
        case "invocation_cancellation":
            _require_pattern(body, "invocation_id", _INVOCATION_ID)
            _require_non_empty(_require_string(body, "completion_key"), "cancellation.completion_key")
            _require_pattern(body, "promise_id", _PROMISE_ID)
        case _:
            raise ValueError(f"unknown emission body kind: {kind}")


def _parse_invocation(value: object) -> None:
    invocation = _record(value, "workflow tool invocation")
    _require_pattern(invocation, "invocation_id", _INVOCATION_ID)
    for key in ("workflow_tool_id", "semantic_type", "binding_fingerprint", "session_id", "tool_call_id"):
        _require_non_empty(_require_string(invocation, key), f"invocation.{key}")
    _normalize_universe_id(_require_string(invocation, "session_universe_id"))
    _require_pattern(invocation, "arguments_ref", _BLOB_REF)
    if invocation.get("execution_context_ref") is not None:
        _require_pattern(invocation, "execution_context_ref", _BLOB_REF)
    for key in ("schema_revision", "run_id", "turn_id", "tool_batch_id"):
        _require_safe_integer(invocation.get(key), key)
    if invocation.get("completion_promises") is not None:
        promises = _record(invocation["completion_promises"], "completion promises")
        for key, promise_id in promises.items():
            _require_non_empty(key, "completion promise key")
            _require_non_empty(promise_id, f"completion promise {key}")


def _parse_resolution(value: object) -> None:
    resolution = _record(value, "promise resolution")
    kind = _require_string(resolution, "kind")
    if kind == "resolved":
        _require_optional_blob_ref(resolution, "payload_ref")
    elif kind == "failed":
        _require_optional_blob_ref(resolution, "error_ref")
    elif kind != "cancelled":
        raise ValueError(f"unknown promise resolution kind: {kind}")


def _record(value: object, name: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise ValueError(f"{name} must be an object")
    return value


def _require_string(value: Mapping[str, Any], key: str) -> str:
    field = value.get(key)
    if not isinstance(field, str):
        raise ValueError(f"{key} must be a string")
    return field


def _require_pattern(value: Mapping[str, Any], key: str, pattern: re.Pattern[str]) -> None:
    if not pattern.fullmatch(_require_string(value, key)):
        raise ValueError(f"{key} has an invalid format")


def _require_optional_blob_ref(value: Mapping[str, Any], key: str) -> None:
    field = value.get(key)
    if field is not None and not _BLOB_REF.fullmatch(_require_non_empty(field, key)):
        raise ValueError(f"{key} is not a canonical blob ref")


def _require_safe_integer(value: object, name: str) -> None:
    # A bool is an int in Python, but not a number on the wire.
    if type(value) is not int or not 0 <= value <= _MAX_SAFE_INTEGER:
        raise ValueError(f"{name} must be a non-negative safe integer")


def _require_session_id(value: object) -> None:
    if not isinstance(value, str) or not _SESSION_ID.fullmatch(value) or len(value.encode()) > 128:
        raise ValueError("invalid session id")


def _normalize_universe_id(value: object) -> str:
    if not isinstance(value, str) or not _UUID.fullmatch(value):
        raise ValueError("universe_id must be a UUID")
    return value.lower()


def _require_invocation_id(value: object) -> None:
    if not isinstance(value, str) or not _INVOCATION_ID.fullmatch(value):
        raise ValueError("invalid workflow tool invocation id")


def _require_non_empty(value: object, name: str) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{name} must be a non-empty string")
    return value


def _u64be(value: int) -> bytes:
    return value.to_bytes(8, "big")

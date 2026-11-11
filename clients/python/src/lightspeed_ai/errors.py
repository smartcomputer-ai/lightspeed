from typing import Any

_RPC_KINDS = {
    -32602: "invalidRequest",
    -32601: "methodNotFound",
    -32001: "unauthenticated",
    -32003: "forbidden",
    -32004: "notFound",
    -32009: "conflict",
    -32010: "rejected",
    -32011: "sessionBootstrapFailed",
    -32012: "environmentNotReady",
    -32013: "responseTooLarge",
    -32603: "internal",
    -32014: "modelDefaultUnset",
    -32015: "operationPending",
    -32016: "sessionExecutionStopped",
}


class LightspeedRpcError(Exception):
    """The gateway rejected the operation."""

    def __init__(self, payload: dict[str, Any]) -> None:
        super().__init__(payload["message"])
        self.payload = payload
        self.code: int = payload["code"]
        self.message: str = payload["message"]
        self.data: Any = payload.get("data")
        self.kind: str = (
            (self.data.get("kind") if isinstance(self.data, dict) else None)
            or _RPC_KINDS.get(self.code, "unknown")
        )


class LightspeedTransportError(Exception):
    """A request failed in transit or the response was unusable."""

    def __init__(
        self,
        message: str,
        *,
        status: int | None = None,
        body: Any = None,
        transient: bool = False,
    ) -> None:
        super().__init__(message)
        self.status = status
        self.body = body
        self.transient = transient or (status is not None and (status >= 500 or status in (408, 429)))

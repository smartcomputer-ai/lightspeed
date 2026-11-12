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


def rpc_error_kind(code: int) -> str:
    """The error kind a JSON-RPC code stands for, or `"unknown"`.

    Several kinds can share a code, so prefer `LightspeedRpcError.kind`, which
    takes the server's own kind first.
    """
    return _RPC_KINDS.get(code, "unknown")


class LightspeedRpcError(Exception):
    """The server answered with an error.

    Attributes:
        kind: What went wrong, such as `notFound`, `conflict` or
            `sessionExecutionStopped`. Branch on this rather than on `code`.
        code: The JSON-RPC error code.
        message: Text for display.
        data: The server's structured error, when it sent one.
        payload: The error object exactly as received.
    """

    def __init__(self, payload: dict[str, Any]) -> None:
        super().__init__(payload["message"])
        self.payload = payload
        self.code: int = payload["code"]
        self.message: str = payload["message"]
        self.data: Any = payload.get("data")
        self.kind: str = (
            (self.data.get("kind") if isinstance(self.data, dict) else None)
            or rpc_error_kind(self.code)
        )


class LightspeedTransportError(Exception):
    """The request failed in transit, or the response was unusable.

    The underlying failure, when there is one, is the exception's `__cause__`.

    Attributes:
        status: The HTTP status, when there was a response.
        body: The response body, parsed as JSON when it could be.
        transient: The same request may succeed if repeated: the network
            failed, or the server or a proxy is struggling. After a transient
            failure of a mutation the request may still have landed, so repeat
            it with the same IDs.
    """

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

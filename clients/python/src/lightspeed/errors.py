from typing import Any


class LightspeedApiError(Exception):
    """A structured HTTP problem; branch on kind, inspect status and payload."""

    def __init__(self, payload: dict[str, Any]) -> None:
        super().__init__(payload["detail"])
        self.payload = payload
        self.status: int = payload["status"]
        self.message: str = payload["detail"]
        self.data = payload
        self.kind: str = payload["kind"]


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

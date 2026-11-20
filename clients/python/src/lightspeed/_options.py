from enum import Enum
from typing import Awaitable, Callable, Mapping

from httpx import Headers

HeadersInput = Mapping[str, str] | Headers
HeaderProvider = Callable[[], HeadersInput | Awaitable[HeadersInput]]


class Unset(Enum):
    TOKEN = "unset"


UNSET = Unset.TOKEN

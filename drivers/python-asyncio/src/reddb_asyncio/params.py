"""Query parameter conversion helpers shared by HTTP and RedWire."""

from __future__ import annotations

import base64
import datetime as _datetime
import math
from typing import Any


def normalize_params(params: Any) -> list[Any]:
    if params is None:
        return []
    if not isinstance(params, (list, tuple)):
        raise TypeError("params must be a list or tuple")
    return [to_json_param(value) for value in params]


def to_json_param(value: Any) -> Any:
    if value is None or isinstance(value, (bool, int, str)):
        return value
    if isinstance(value, float):
        if math.isnan(value):
            return {"$float": "NaN"}
        if math.isinf(value):
            return {"$float": "Infinity" if value > 0 else "-Infinity"}
        return value
    if isinstance(value, (bytes, bytearray, memoryview)):
        raw = bytes(value)
        return {"$bytes": base64.b64encode(raw).decode("ascii")}
    if isinstance(value, _datetime.datetime):
        return {"$ts": int(value.timestamp())}
    if isinstance(value, (list, tuple)):
        if all(
            isinstance(item, (int, float)) and not isinstance(item, bool)
            for item in value
        ):
            return [float(item) for item in value]
        raise TypeError("list params must contain only numbers")
    if isinstance(value, dict):
        return {str(k): to_json_param(v) for k, v in value.items()}
    raise TypeError(f"unsupported query parameter type: {type(value).__name__}")

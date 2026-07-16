"""Query parameter conversion helpers shared by HTTP and RedWire."""

from __future__ import annotations

import base64
import datetime as _datetime
import decimal as _decimal
import math
from typing import Any

_MIN_I64 = -(2**63)
_MAX_I64 = 2**63 - 1
_MAX_U64 = 2**64 - 1


def normalize_params(params: Any) -> list[Any]:
    if params is None:
        return []
    if not isinstance(params, (list, tuple)):
        raise TypeError("params must be a list or tuple")
    return [to_json_param(value) for value in params]


def to_json_param(value: Any) -> Any:
    if value is None or isinstance(value, (bool, str)):
        return value
    if isinstance(value, int):
        if _MIN_I64 <= value <= _MAX_I64:
            return value
        if 0 <= value <= _MAX_U64:
            return {"$uint": str(value)}
        raise TypeError("integer query parameters must fit i64 or u64")
    if isinstance(value, _decimal.Decimal):
        if not value.is_finite():
            raise TypeError("Decimal query parameters must be finite")
        return {"$decimal": format(value, "f")}
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


def normalize_json_value(value: Any) -> Any:
    if isinstance(value, list):
        return [normalize_json_value(item) for item in value]
    if isinstance(value, dict):
        if len(value) == 1:
            if isinstance(value.get("$int"), str):
                return int(value["$int"])
            if isinstance(value.get("$uint"), str):
                return int(value["$uint"])
            if isinstance(value.get("$decimal"), str):
                return _decimal.Decimal(value["$decimal"])
            if "$number" in value or "$decimalText" in value:
                raise ValueError("superseded exact-number envelope")
        return {key: normalize_json_value(item) for key, item in value.items()}
    return value

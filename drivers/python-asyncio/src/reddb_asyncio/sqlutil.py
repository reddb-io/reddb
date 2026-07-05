"""SQL identifier and literal helpers shared by the helper namespaces.

Keep these pure-Python and dependency-free so they can be unit-tested
without spinning up a server.
"""

from __future__ import annotations

import json as _json
import re
from typing import Any

from .errors import RedDBError


_IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def sql_identifier(value: Any) -> str:
    ident = str(value)
    if not _IDENT_RE.match(ident):
        raise RedDBError(f'invalid SQL identifier "{ident}"', code="INVALID_ARGUMENT")
    return ident


def sql_identifier_path(value: Any) -> str:
    return ".".join(sql_identifier(part) for part in str(value).split("."))


def sql_string(value: Any) -> str:
    return "'" + str(value).replace("'", "''") + "'"


def sql_json_literal(value: Any) -> str:
    return sql_string(_json.dumps(value, separators=(",", ":")))


def sql_json_inline_literal(value: Any) -> str:
    # ADR 0067 (#1709): a document body is written as an inline strict-JSON
    # literal (no surrounding quotes) — the quoted-string coercion is removed.
    return _json.dumps(value, separators=(",", ":"))


def sql_value_literal(value: Any) -> str:
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, (dict, list, tuple)):
        return sql_json_literal(value)
    return sql_string(value)


__all__ = [
    "sql_identifier",
    "sql_identifier_path",
    "sql_string",
    "sql_json_literal",
    "sql_json_inline_literal",
    "sql_value_literal",
]

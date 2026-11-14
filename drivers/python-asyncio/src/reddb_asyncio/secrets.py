"""Explicitly scoped Vault helpers for application secrets."""

from __future__ import annotations

import re
from typing import Any

from .errors import RedDBError
from .kv import kv_value_literal

__all__ = ["SecretsClient"]

_IDENTIFIER = re.compile(r"^[A-Za-z0-9_.]+$")


class SecretsClient:
    """Vault collection with metadata reads separated from plaintext reveal."""

    def __init__(self, transport: Any, collection: str) -> None:
        if _IDENTIFIER.fullmatch(collection) is None:
            raise RedDBError("invalid vault collection", code="INVALID_ARGUMENT")
        self._transport = transport
        self.collection = collection

    async def put(self, key: str, value: Any) -> dict[str, Any]:
        return await self._transport.query(
            f"VAULT PUT {self._path(key)} = {kv_value_literal(value)}"
        )

    async def get(self, key: str) -> dict[str, Any]:
        return await self._transport.query(f"VAULT GET {self._path(key)}")

    async def reveal(self, key: str) -> dict[str, Any]:
        return await self._transport.query(f"VAULT REVEAL {self._path(key)}")

    def _path(self, key: str) -> str:
        if _IDENTIFIER.fullmatch(key) is None:
            raise RedDBError("invalid vault key", code="INVALID_ARGUMENT")
        return f"{self.collection}.{key}"

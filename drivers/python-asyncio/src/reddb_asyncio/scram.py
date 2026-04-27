"""SCRAM-SHA-256 client primitives (RFC 5802 / RFC 7677).

Pure functions, no I/O. Mirrors ``drivers/rust/src/redwire/scram.rs``
so the two clients are bit-for-bit interoperable with the engine's
``src/auth/scram.rs``.

The wire encoding for salt / proof / signature is **standard base64**
(RFC 4648 with ``+/`` alphabet, ``=`` padding) — see
``src/wire/redwire/auth.rs::base64_std``.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import os
import secrets
from typing import Final

DEFAULT_ITER: Final[int] = 16_384
MIN_ITER: Final[int] = 4_096


def hmac_sha256(key: bytes, data: bytes) -> bytes:
    return hmac.new(key, data, hashlib.sha256).digest()


def sha256(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def pbkdf2_sha256(password: bytes, salt: bytes, iterations: int) -> bytes:
    """PBKDF2-HMAC-SHA256 with a fixed 32-byte derived key."""
    return hashlib.pbkdf2_hmac("sha256", password, salt, iterations, dklen=32)


def xor(a: bytes, b: bytes) -> bytes:
    if len(a) != len(b):
        raise ValueError("xor inputs must be equal length")
    return bytes(x ^ y for x, y in zip(a, b))


def b64encode(data: bytes) -> str:
    """Standard-alphabet base64 with padding — matches the server."""
    return base64.b64encode(data).decode("ascii")


def b64decode(text: str) -> bytes:
    """Standard-alphabet base64 decode tolerant of missing padding."""
    pad = (-len(text)) % 4
    return base64.b64decode(text + "=" * pad)


def make_client_nonce(num_bytes: int = 18) -> str:
    """24-character base64 nonce (matches the server's ``base64_std(random(18))``)."""
    return b64encode(secrets.token_bytes(num_bytes))


# ---------------------------------------------------------------------------
# High-level helpers used by the handshake state machine.
# ---------------------------------------------------------------------------


def build_client_first(username: str, client_nonce: str) -> tuple[str, str]:
    """Return ``(client_first_message, client_first_bare)``.

    Both pieces are needed: the full message is sent on the wire
    (``n,,n=<user>,r=<nonce>``) and ``client_first_bare`` feeds the
    ``auth_message`` later.
    """
    bare = f"n={username},r={client_nonce}"
    return f"n,,{bare}", bare


def parse_server_first(server_first: str, client_nonce: str) -> tuple[str, bytes, int]:
    """Parse the server-first-message ``r=<combined>,s=<salt_b64>,i=<iter>``.

    Returns ``(combined_nonce, salt_bytes, iter)``.
    Raises :class:`ValueError` when the nonce does not start with ours
    or required fields are missing.
    """
    combined = salt_b64 = ""
    iters: int | None = None
    for part in server_first.split(","):
        if part.startswith("r="):
            combined = part[2:]
        elif part.startswith("s="):
            salt_b64 = part[2:]
        elif part.startswith("i="):
            try:
                iters = int(part[2:])
            except ValueError as exc:
                raise ValueError(f"invalid iter in server-first: {part!r}") from exc
    if not combined or not salt_b64 or iters is None:
        raise ValueError(f"server-first missing fields: {server_first!r}")
    if not combined.startswith(client_nonce):
        raise ValueError("server nonce does not start with client nonce (replay protection)")
    if iters < MIN_ITER:
        raise ValueError(f"server-supplied iter ({iters}) is below MIN_ITER={MIN_ITER}")
    return combined, b64decode(salt_b64), iters


def auth_message(client_first_bare: str, server_first: str, client_final_no_proof: str) -> bytes:
    return f"{client_first_bare},{server_first},{client_final_no_proof}".encode("utf-8")


def client_proof(password: bytes, salt: bytes, iters: int, am: bytes) -> bytes:
    """Compute ``ClientKey XOR HMAC(StoredKey, AuthMessage)`` (32 bytes)."""
    salted = pbkdf2_sha256(password, salt, iters)
    client_key = hmac_sha256(salted, b"Client Key")
    stored_key = sha256(client_key)
    sig = hmac_sha256(stored_key, am)
    return xor(client_key, sig)


def build_client_final(combined_nonce: str, proof: bytes) -> tuple[str, str]:
    """Return ``(client_final_message, client_final_no_proof)``."""
    no_proof = f"c=biws,r={combined_nonce}"
    return f"{no_proof},p={b64encode(proof)}", no_proof


def server_signature(password: bytes, salt: bytes, iters: int, am: bytes) -> bytes:
    salted = pbkdf2_sha256(password, salt, iters)
    server_key = hmac_sha256(salted, b"Server Key")
    return hmac_sha256(server_key, am)


def constant_time_eq(a: bytes, b: bytes) -> bool:
    return hmac.compare_digest(a, b)


def verify_server_signature(password: bytes, salt: bytes, iters: int, am: bytes, presented: bytes) -> bool:
    if len(presented) != 32:
        return False
    expected = server_signature(password, salt, iters, am)
    return constant_time_eq(expected, presented)


__all__ = [
    "DEFAULT_ITER",
    "MIN_ITER",
    "hmac_sha256",
    "sha256",
    "pbkdf2_sha256",
    "xor",
    "b64encode",
    "b64decode",
    "make_client_nonce",
    "build_client_first",
    "parse_server_first",
    "auth_message",
    "client_proof",
    "build_client_final",
    "server_signature",
    "verify_server_signature",
    "constant_time_eq",
]

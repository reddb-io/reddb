"""Frame codec round-trip + invariant tests.

Mirrors src/wire/redwire/codec.rs unit tests so any future drift on
the wire shows up here.
"""

from __future__ import annotations

import pytest

from reddb_asyncio import (
    FrameTooLarge,
    UnknownFlags,
)
from reddb_asyncio.errors import ProtocolError
from reddb_asyncio.redwire import (
    FRAME_HEADER_SIZE,
    Flags,
    Frame,
    Kind,
    decode_frame,
    encode_frame,
)


def test_round_trip_empty_payload():
    f = Frame(kind=Kind.Ping, correlation_id=1)
    bytes_ = encode_frame(f)
    assert len(bytes_) == FRAME_HEADER_SIZE
    decoded = decode_frame(bytes_)
    assert decoded.kind == Kind.Ping
    assert decoded.correlation_id == 1
    assert decoded.payload == b""


def test_round_trip_with_payload():
    f = Frame(kind=Kind.Query, correlation_id=42, payload=b"SELECT 1")
    decoded = decode_frame(encode_frame(f))
    assert decoded.kind == Kind.Query
    assert decoded.correlation_id == 42
    assert decoded.payload == b"SELECT 1"


def test_unknown_flags_rejected():
    # Build a header with an unknown high bit.
    header = bytearray(16)
    header[0:4] = (FRAME_HEADER_SIZE).to_bytes(4, "little")
    header[4] = Kind.Ping
    header[5] = 0b1000_0000
    with pytest.raises(UnknownFlags):
        decode_frame(bytes(header))


def test_invalid_length_rejected():
    header = bytearray(16)
    header[0:4] = (15).to_bytes(4, "little")
    header[4] = Kind.Ping
    with pytest.raises(ProtocolError):
        decode_frame(bytes(header))


def test_frame_too_large_at_encode():
    big = b"x" * (16 * 1024 * 1024 - FRAME_HEADER_SIZE + 1)
    with pytest.raises(FrameTooLarge):
        encode_frame(Frame(kind=Kind.Query, correlation_id=1, payload=big))


def test_streaming_decode_two_frames_back_to_back():
    a = encode_frame(Frame(kind=Kind.Query, correlation_id=1, payload=b"a"))
    b = encode_frame(Frame(kind=Kind.Query, correlation_id=2, payload=b"b"))
    buf = a + b
    fa = decode_frame(buf[: len(a)])
    fb = decode_frame(buf[len(a) :])
    assert fa.payload == b"a"
    assert fb.payload == b"b"


def test_compressed_round_trip_when_zstd_present():
    pytest.importorskip("zstandard")
    payload = b"abcabcabcabc" * 100
    f = Frame(kind=Kind.Result, correlation_id=7, payload=payload, flags=Flags.COMPRESSED)
    on_wire = encode_frame(f)
    assert len(on_wire) < FRAME_HEADER_SIZE + len(payload)
    decoded = decode_frame(on_wire)
    assert decoded.payload == payload
    assert decoded.flags & Flags.COMPRESSED

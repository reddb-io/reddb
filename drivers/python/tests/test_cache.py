"""
Cache API tests for the reddb Python driver.

Run after `maturin develop`:

    python -m pytest tests/test_cache.py
"""

import sys

import reddb


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _db():
    return reddb.connect("memory://")


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

def test_cache_attribute_exists():
    with _db() as db:
        c = db.cache
        assert c is not None


def test_put_and_get_round_trip():
    with _db() as db:
        db.cache.put("ns", "k1", b"hello")
        result = db.cache.get("ns", "k1")
        assert result == b"hello"


def test_get_miss_returns_none():
    with _db() as db:
        result = db.cache.get("ns", "no-such-key")
        assert result is None


def test_put_string_value_is_rejected():
    """put() requires bytes, not str — type error expected."""
    with _db() as db:
        try:
            db.cache.put("ns", "k", "not-bytes")
        except (TypeError, ValueError):
            pass
        else:
            raise AssertionError("expected TypeError or ValueError for str value")


def test_put_with_ttl_ms():
    with _db() as db:
        db.cache.put("ns", "k-ttl", b"data", ttl_ms=60_000)
        result = db.cache.get("ns", "k-ttl")
        assert result == b"data"


def test_put_with_tags():
    with _db() as db:
        db.cache.put("ns", "tagged", b"payload", tags=["tag1", "tag2"])
        result = db.cache.get("ns", "tagged")
        assert result == b"payload"


def test_exists_present():
    with _db() as db:
        db.cache.put("ns", "e1", b"v")
        status = db.cache.exists("ns", "e1")
        assert status == "present"


def test_exists_absent():
    with _db() as db:
        status = db.cache.exists("ns", "ghost")
        assert status == "absent"


def test_exists_returns_valid_literal():
    with _db() as db:
        db.cache.put("ns", "x", b"y")
        status = db.cache.exists("ns", "x")
        assert status in ("present", "absent", "maybe")


def test_invalidate_removes_entry():
    with _db() as db:
        db.cache.put("ns", "del", b"gone")
        count = db.cache.invalidate("ns", "del")
        assert count >= 0
        assert db.cache.get("ns", "del") is None


def test_invalidate_prefix():
    with _db() as db:
        db.cache.put("ns", "prefix/a", b"1")
        db.cache.put("ns", "prefix/b", b"2")
        db.cache.put("ns", "other/c", b"3")
        removed = db.cache.invalidate_prefix("ns", "prefix/")
        assert removed >= 0
        assert db.cache.get("ns", "prefix/a") is None
        assert db.cache.get("ns", "prefix/b") is None
        assert db.cache.get("ns", "other/c") == b"3"


def test_invalidate_tags():
    with _db() as db:
        db.cache.put("ns", "t1", b"a", tags=["shared"])
        db.cache.put("ns", "t2", b"b", tags=["shared"])
        db.cache.put("ns", "t3", b"c", tags=["other"])
        removed = db.cache.invalidate_tags("ns", ["shared"])
        assert removed >= 0
        assert db.cache.get("ns", "t1") is None
        assert db.cache.get("ns", "t2") is None
        assert db.cache.get("ns", "t3") == b"c"


def test_flush_namespace():
    with _db() as db:
        db.cache.put("ns", "a", b"1")
        db.cache.put("ns", "b", b"2")
        db.cache.flush_namespace("ns")
        assert db.cache.get("ns", "a") is None
        assert db.cache.get("ns", "b") is None


def test_different_namespaces_are_isolated():
    with _db() as db:
        db.cache.put("ns1", "k", b"from-ns1")
        db.cache.put("ns2", "k", b"from-ns2")
        assert db.cache.get("ns1", "k") == b"from-ns1"
        assert db.cache.get("ns2", "k") == b"from-ns2"
        db.cache.flush_namespace("ns1")
        assert db.cache.get("ns1", "k") is None
        assert db.cache.get("ns2", "k") == b"from-ns2"


def test_overwrite_key():
    with _db() as db:
        db.cache.put("ns", "rw", b"v1")
        db.cache.put("ns", "rw", b"v2")
        assert db.cache.get("ns", "rw") == b"v2"


def test_cache_client_is_exported():
    assert hasattr(reddb, "CacheClient")


# ---------------------------------------------------------------------------
# Plain-stdlib runner
# ---------------------------------------------------------------------------
if __name__ == "__main__":
    tests = [
        ("cache_attribute_exists", test_cache_attribute_exists),
        ("put_and_get_round_trip", test_put_and_get_round_trip),
        ("get_miss_returns_none", test_get_miss_returns_none),
        ("put_string_value_is_rejected", test_put_string_value_is_rejected),
        ("put_with_ttl_ms", test_put_with_ttl_ms),
        ("put_with_tags", test_put_with_tags),
        ("exists_present", test_exists_present),
        ("exists_absent", test_exists_absent),
        ("exists_returns_valid_literal", test_exists_returns_valid_literal),
        ("invalidate_removes_entry", test_invalidate_removes_entry),
        ("invalidate_prefix", test_invalidate_prefix),
        ("invalidate_tags", test_invalidate_tags),
        ("flush_namespace", test_flush_namespace),
        ("different_namespaces_are_isolated", test_different_namespaces_are_isolated),
        ("overwrite_key", test_overwrite_key),
        ("cache_client_is_exported", test_cache_client_is_exported),
    ]
    passed = failed = 0
    for name, fn in tests:
        try:
            fn()
            print(f"  ok  {name}")
            passed += 1
        except Exception as exc:
            print(f"  FAIL {name}: {exc}")
            failed += 1
    print(f"\n{passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)

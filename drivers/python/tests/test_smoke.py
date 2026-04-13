"""
Smoke test for the reddb Python driver.

Run after `maturin develop`:

    python -m pytest tests/test_smoke.py
    # or, with no pytest:
    python tests/test_smoke.py
"""

import sys

import reddb


def test_version_reports_module_version():
    db = reddb.connect("memory://")
    try:
        v = db.version()
        assert isinstance(v, dict)
        assert v["protocol"] == "1.0"
        assert isinstance(v["version"], str)
    finally:
        db.close()


def test_health_returns_ok_true():
    with reddb.connect("memory://") as db:
        h = db.health()
        assert h["ok"] is True
        assert "version" in h


def test_insert_then_query_round_trip():
    with reddb.connect("memory://") as db:
        ins = db.insert("users", {"name": "Alice", "age": 30})
        assert ins["affected"] == 1

        ins2 = db.insert("users", {"name": "Bob", "age": 25})
        assert ins2["affected"] == 1

        result = db.query("SELECT * FROM users")
        assert result["statement"] == "select"
        assert isinstance(result["rows"], list)
        assert len(result["rows"]) == 2

        names = sorted(row["name"] for row in result["rows"])
        assert names == ["Alice", "Bob"]


def test_bulk_insert_returns_total_affected():
    with reddb.connect("memory://") as db:
        n = db.bulk_insert(
            "items",
            [{"name": "a"}, {"name": "b"}, {"name": "c"}],
        )
        assert n["affected"] == 3
        rows = db.query("SELECT * FROM items")["rows"]
        assert len(rows) == 3


def test_query_error_raises_value_error():
    with reddb.connect("memory://") as db:
        try:
            db.query("NOT A VALID STATEMENT $$$")
        except ValueError as e:
            assert "QUERY_ERROR" in str(e)
        else:
            raise AssertionError("expected ValueError")


def test_unsupported_scheme_raises():
    try:
        reddb.connect("mongodb://localhost")
    except ValueError as e:
        assert "UNSUPPORTED_SCHEME" in str(e)
    else:
        raise AssertionError("expected ValueError")


def test_grpc_uri_returns_feature_disabled_for_now():
    try:
        reddb.connect("grpc://localhost:50051")
    except ValueError as e:
        assert "FEATURE_DISABLED" in str(e)
    else:
        raise AssertionError("expected ValueError")


def test_calls_after_close_raise_client_closed():
    db = reddb.connect("memory://")
    db.close()
    try:
        db.version()
    except ValueError as e:
        assert "CLIENT_CLOSED" in str(e)
    else:
        raise AssertionError("expected ValueError")


def test_field_value_types_are_validated():
    with reddb.connect("memory://") as db:
        try:
            db.insert("users", {"weird": [1, 2, 3]})  # list values not supported
        except ValueError as e:
            assert "INVALID_PARAMS" in str(e)
        else:
            raise AssertionError("expected ValueError")


def test_legacy_classes_are_still_exported():
    # We don't try to connect them here (would need a real server) but the
    # symbols should be importable so existing code keeps working.
    assert hasattr(reddb, "legacy_grpc_connect")
    assert hasattr(reddb, "wire_connect")
    assert hasattr(reddb, "Connection")
    assert hasattr(reddb, "WireConnection")


# -----------------------------------------------------------------------
# Plain-stdlib runner so the file can be executed without pytest.
# -----------------------------------------------------------------------
if __name__ == "__main__":
    tests = [
        ("version_reports_module_version", test_version_reports_module_version),
        ("health_returns_ok_true", test_health_returns_ok_true),
        ("insert_then_query_round_trip", test_insert_then_query_round_trip),
        ("bulk_insert_returns_total_affected", test_bulk_insert_returns_total_affected),
        ("query_error_raises_value_error", test_query_error_raises_value_error),
        ("unsupported_scheme_raises", test_unsupported_scheme_raises),
        ("grpc_uri_returns_feature_disabled_for_now", test_grpc_uri_returns_feature_disabled_for_now),
        ("calls_after_close_raise_client_closed", test_calls_after_close_raise_client_closed),
        ("field_value_types_are_validated", test_field_value_types_are_validated),
        ("legacy_classes_are_still_exported", test_legacy_classes_are_still_exported),
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

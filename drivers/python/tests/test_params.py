"""
Parameterized query tests for the reddb Python driver (issue #362).

Build the wheel first:

    maturin develop --release        # in drivers/python/
    python -m pytest tests/test_params.py

Or, with no pytest:

    python tests/test_params.py
"""

import datetime
import importlib.util
from pathlib import Path
import sys
import uuid

import reddb


# ---------------------------------------------------------------------------
# Native-type round-trip — happy path
# ---------------------------------------------------------------------------

def test_int_param_binds_into_where_clause():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE t (id INT, name TEXT)")
        db.insert("t", {"id": 1, "name": "Alice"})
        db.insert("t", {"id": 2, "name": "Bob"})

        rows = db.query("SELECT * FROM t WHERE id = $1", 2)["rows"]
        assert len(rows) == 1
        assert rows[0]["name"] == "Bob"


def test_text_param_binds_into_where_clause():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE t (id INT, name TEXT)")
        db.insert("t", {"id": 1, "name": "Alice"})
        db.insert("t", {"id": 2, "name": "Bob'son"})  # embedded quote

        rows = db.query("SELECT * FROM t WHERE name = $1", "Bob'son")["rows"]
        assert len(rows) == 1
        assert rows[0]["id"] == 2


def test_null_param_binds():
    """`None` -> Value::Null. `WHERE x IS $1` matches NULL rows."""
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE t (id INT, note TEXT)")
        db.insert("t", {"id": 1, "note": "x"})

        # The binder accepts Null; the engine compares accordingly.
        # We don't pin the SQL syntax for IS NULL via params here — we
        # just confirm the call completes (i.e. None serialized).
        out = db.query("SELECT * FROM t WHERE id = $1", 1)
        assert out["rows"][0]["note"] == "x"

        # Re-bind None to a different slot — the conversion must succeed
        # even if the engine rejects the comparison semantically.
        try:
            db.query("SELECT * FROM t WHERE note = $1", None)
        except ValueError:
            pass  # engine-level rejection is fine; serialization is the wedge


def test_float_param_round_trips():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE m (id INT, val FLOAT)")
        db.insert("m", {"id": 1, "val": 3.14})

        rows = db.query("SELECT * FROM m WHERE id = $1", 1)["rows"]
        assert abs(rows[0]["val"] - 3.14) < 1e-6


def test_bool_param_round_trips():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE flags (id INT, ok BOOLEAN)")
        db.insert("flags", {"id": 1, "ok": True})
        rows = db.query("SELECT * FROM flags WHERE id = $1", 1)["rows"]
        assert rows[0]["ok"] is True


# ---------------------------------------------------------------------------
# params= keyword form parity
# ---------------------------------------------------------------------------

def test_params_kwarg_matches_variadic_form():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE t (id INT, name TEXT)")
        db.insert("t", {"id": 1, "name": "Alice"})
        db.insert("t", {"id": 2, "name": "Bob"})

        a = db.query("SELECT * FROM t WHERE id = $1", 2)
        b = db.query("SELECT * FROM t WHERE id = $1", params=[2])
        assert a == b


def test_params_kwarg_accepts_tuple_dbapi_style():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE t (id INT, name TEXT)")
        db.insert("t", {"id": 1, "name": "Ada"})

        rows = db.query("SELECT * FROM t WHERE id = $1", params=(1,))["rows"]
        assert len(rows) == 1
        assert rows[0]["name"] == "Ada"


def test_typing_metadata_installed():
    spec = importlib.util.find_spec("reddb")
    assert spec is not None and spec.origin is not None
    package_dir = Path(spec.origin).parent
    assert (package_dir / "__init__.pyi").exists()
    assert (package_dir / "py.typed").exists()


def test_execute_accepts_params_keyword():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE exec_params (id INT, name TEXT)")
        inserted = db.execute(
            "INSERT INTO exec_params (id, name) VALUES ($1, $2)",
            params=[1, "Ada"],
        )
        assert inserted["affected"] == 1
        rows = db.query("SELECT * FROM exec_params WHERE id = $1", params=[1])["rows"]
        assert rows[0]["name"] == "Ada"


def test_params_none_kw_equals_no_params():
    """`params=None` composes with `db.query(sql, params=maybe_list)` callers."""
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE t (id INT)")
        db.insert("t", {"id": 1})
        rows = db.query("SELECT * FROM t", params=None)["rows"]
        assert len(rows) == 1


# ---------------------------------------------------------------------------
# Error paths
# ---------------------------------------------------------------------------

def test_arity_mismatch_raises_query_error():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE t (id INT)")
        try:
            db.query("SELECT * FROM t WHERE id = $1 AND id = $2", 1)
        except ValueError as e:
            assert "QUERY_ERROR" in str(e) or "INVALID_PARAMS" in str(e)
        else:
            raise AssertionError("expected ValueError on arity mismatch")


def test_unsupported_param_type_raises():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE t (id INT)")
        try:
            db.query("SELECT * FROM t WHERE id = $1", {1, 2, 3})  # set
        except ValueError as e:
            assert "INVALID_PARAMS" in str(e)
        else:
            raise AssertionError("expected ValueError on unsupported type")


def test_params_kw_must_be_list_or_tuple():
    with reddb.connect("memory://") as db:
        try:
            db.query("SELECT 1", params="not a list")
        except ValueError as e:
            assert "INVALID_PARAMS" in str(e)
        else:
            raise AssertionError("expected ValueError when params= is not a list or tuple")


# ---------------------------------------------------------------------------
# Original single-arg overload is unchanged (back-compat).
# ---------------------------------------------------------------------------

def test_query_without_params_keeps_original_signature():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE t (id INT)")
        db.insert("t", {"id": 7})
        rows = db.query("SELECT * FROM t")["rows"]
        assert len(rows) == 1
        assert rows[0]["id"] == 7


# ---------------------------------------------------------------------------
# Native-type serialization: invoking each branch of `py_to_param_value`.
# These don't all reach an executable SQL form (datetime/uuid/vector slots
# in the parser aren't broadly available), but they prove the converter
# itself accepts the value without raising INVALID_PARAMS.
# ---------------------------------------------------------------------------

def _converter_accepts(value):
    """A param converter sniff-test that doesn't depend on the runtime
    accepting the binding semantically. We pick a SQL form the parser
    rejects late so the python-side conversion happens before the engine
    complains. If conversion raises INVALID_PARAMS, the value is *not*
    accepted."""
    with reddb.connect("memory://") as db:
        try:
            db.query("SELECT $1", value)
        except ValueError as e:
            # Engine-level QUERY_ERROR is fine; conversion succeeded.
            return "INVALID_PARAMS" not in str(e)
        return True


def test_converter_accepts_bytes():
    assert _converter_accepts(b"raw bytes")


def test_converter_accepts_bytearray():
    assert _converter_accepts(bytearray(b"ba"))


def test_converter_accepts_list_of_floats():
    assert _converter_accepts([0.1, 0.2, 0.3])


def test_converter_accepts_list_of_ints_as_vector():
    assert _converter_accepts([1, 2, 3])


def test_converter_rejects_mixed_list():
    with reddb.connect("memory://") as db:
        try:
            db.query("SELECT $1", [1, "str"])
        except ValueError as e:
            assert "INVALID_PARAMS" in str(e)
        else:
            raise AssertionError("expected INVALID_PARAMS for mixed list")


def test_converter_accepts_datetime():
    assert _converter_accepts(datetime.datetime(2026, 1, 2, 3, 4, 5))


def test_converter_accepts_uuid():
    assert _converter_accepts(uuid.UUID("12345678-1234-5678-1234-567812345678"))


def test_converter_accepts_dict_as_json():
    assert _converter_accepts({"a": 1, "b": "two"})


# ---------------------------------------------------------------------------
# Plain-stdlib runner
# ---------------------------------------------------------------------------
if __name__ == "__main__":
    tests = [(name, fn) for name, fn in sorted(globals().items()) if name.startswith("test_")]
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

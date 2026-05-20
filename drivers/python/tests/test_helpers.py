"""
Conformance smoke tests for high-level Python helpers.

Run after `maturin develop`:

    python -m pytest tests/test_helpers.py
"""

import sys

import reddb


def test_insert_and_bulk_insert_expose_rid_aliases():
    with reddb.connect("memory://") as db:
        one = db.insert("users", {"name": "Ada"})
        assert one["affected"] == 1
        assert int(one["rid"]) > 0
        assert one["id"] == one["rid"]

        many = db.bulk_insert("users", [{"name": "Bob"}, {"name": "Cy"}])
        assert many["affected"] == 2
        assert len(many["rids"]) == 2
        assert many["ids"] == many["rids"]

        assert db.exists("users", str(one["rid"])) == {"exists": True}
        listed = db.list("users", order_by="rid ASC", limit=3)
        assert [row["name"] for row in listed["items"]] == ["Ada", "Bob", "Cy"]


def test_documents_crud_nested_patch_delete():
    with reddb.connect("memory://") as db:
        inserted = db.documents.insert(
            "profiles",
            {
                "name": "Hansel",
                "status": "lost",
                "details": {"trail": "crumbs", "count": 3},
            },
        )
        assert inserted["affected"] == 1
        rid = str(inserted["rid"])
        assert inserted["item"]["kind"] == "document"
        assert inserted["item"]["details"]["trail"] == "crumbs"

        fetched = db.documents.get("profiles", rid)
        assert fetched["rid"] == inserted["rid"]
        assert fetched["details"]["count"] == 3

        listed = db.documents.list("profiles", filter="name = 'Hansel'")
        assert len(listed["items"]) == 1

        patched = db.documents.patch("profiles", rid, {"status": "found"})
        assert patched["status"] == "found"
        assert patched["details"]["trail"] == "crumbs"

        deleted = db.documents.delete("profiles", rid)
        assert deleted["affected"] == 1
        try:
            db.documents.get("profiles", rid)
        except ValueError as exc:
            assert "NOT_FOUND" in str(exc)
        else:
            raise AssertionError("expected NOT_FOUND")


def test_kv_namespaced_exact_key_round_trip():
    with reddb.connect("memory://") as db:
        key = "characters:hansel"
        db.kv.set("settings", key, {"role": "finder", "steps": 7})

        assert db.kv.exists("settings", key) == {"exists": True}
        assert db.kv.get("settings", key) == {"role": "finder", "steps": 7}

        listed = db.kv.list("settings", prefix="characters:")
        assert listed["items"] == [
            {"key": "characters:hansel", "value": {"role": "finder", "steps": 7}}
        ]

        deleted = db.kv.delete("settings", key)
        assert deleted["affected"] == 1
        assert db.kv.exists("settings", key) == {"exists": False}


def test_kv_get_missing_returns_none_and_delete_envelope():
    with reddb.connect("memory://") as db:
        db.kv.set("kvenv", "present", {"v": 1})
        assert db.kv.get("kvenv", "absent") is None
        deleted = db.kv.delete("kvenv", "present")
        assert deleted == {"affected": 1, "deleted": True}
        missing = db.kv.delete("kvenv", "absent")
        assert missing["deleted"] is False


def test_queues_fifo_round_trip():
    with reddb.connect("memory://") as db:
        db.queues.create("jobs")
        db.queues.push("jobs", {"n": 1})
        db.queues.push("jobs", {"n": 2})
        assert db.queues.len("jobs") == 2
        # peek does not consume
        assert len(db.queues.peek("jobs", 1)["items"]) == 1
        assert db.queues.len("jobs") == 2
        assert len(db.queues.pop("jobs")["items"]) == 1
        assert db.queues.len("jobs") == 1
        # empty pop is not an error
        db.queues.purge("jobs")
        assert db.queues.pop("jobs")["items"] == []
        assert db.queues.len("jobs") == 0
        # singular alias points at the same client
        assert db.queue.len("jobs") == 0


def test_tx_run_commit_and_rollback():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE tx_users (name TEXT)")

        db.tx.run(lambda tx: db.query("INSERT INTO tx_users (name) VALUES ('kept')"))
        assert len(db.query("SELECT name FROM tx_users WHERE name = 'kept'")["rows"]) == 1

        def boom(tx):
            db.query("INSERT INTO tx_users (name) VALUES ('dropped')")
            raise RuntimeError("explode")

        try:
            db.tx.run(boom)
        except RuntimeError:
            pass
        else:
            raise AssertionError("expected the callback exception to propagate")
        assert db.query("SELECT name FROM tx_users WHERE name = 'dropped'")["rows"] == []


def test_tx_run_rejects_nesting():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE tx_nest (name TEXT)")
        tx = db.tx

        def outer(inner):
            inner.run(lambda _t: None)

        try:
            tx.run(outer)
        except ValueError as exc:
            assert "INVALID_ARGUMENT" in str(exc)
        else:
            raise AssertionError("expected nested tx.run to reject")


def test_helper_spec_version_constant():
    assert reddb.helper_spec_version == "1.0"
    with reddb.connect("memory://") as db:
        assert db.helper_spec_version == "1.0"


def test_grpc_helper_limitations_are_explicit_without_server():
    try:
        db = reddb.connect("grpc://127.0.0.1:1")
    except ValueError as exc:
        assert "IO_ERROR" in str(exc) or "grpc connect failed" in str(exc)
    else:
        try:
            try:
                db.documents.insert("docs", {"name": "x"})
            except ValueError as exc:
                assert "NOT_SUPPORTED" in str(exc)
            else:
                raise AssertionError("expected NOT_SUPPORTED")
        finally:
            db.close()


if __name__ == "__main__":
    tests = [
        ("insert_and_bulk_insert_expose_rid_aliases", test_insert_and_bulk_insert_expose_rid_aliases),
        ("documents_crud_nested_patch_delete", test_documents_crud_nested_patch_delete),
        ("kv_namespaced_exact_key_round_trip", test_kv_namespaced_exact_key_round_trip),
        ("kv_get_missing_returns_none_and_delete_envelope", test_kv_get_missing_returns_none_and_delete_envelope),
        ("queues_fifo_round_trip", test_queues_fifo_round_trip),
        ("tx_run_commit_and_rollback", test_tx_run_commit_and_rollback),
        ("tx_run_rejects_nesting", test_tx_run_rejects_nesting),
        ("helper_spec_version_constant", test_helper_spec_version_constant),
        ("grpc_helper_limitations_are_explicit_without_server", test_grpc_helper_limitations_are_explicit_without_server),
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

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

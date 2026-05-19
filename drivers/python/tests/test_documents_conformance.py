"""
SDK Helper Spec — documents.* conformance harness (Python driver port).

Mirrors the Rust harness at ``crates/reddb-client/tests/conformance.rs``
for the documents.* case IDs defined in §12 of
``docs/spec/sdk-helpers.md``.

Cases covered (per spec §12):

- ``documents.crud_nested_patch``
- ``documents.delete_missing_no_error``
- ``documents.patch_empty_rejects``
- ``errors.not_found.document_get``

Run after ``maturin develop``::

    python -m pytest drivers/python/tests/test_documents_conformance.py
"""

import sys

import reddb


# ----------------------------------------------------- documents.crud_nested_patch
def test_documents_crud_nested_patch():
    with reddb.connect("memory://") as db:
        inserted = db.documents.insert(
            "conf_events",
            {"event_type": "login", "attempts": 2, "success": True},
        )
        assert inserted["affected"] == 1
        rid = str(inserted["rid"])

        fetched = db.documents.get("conf_events", rid)
        assert str(fetched["rid"]) == rid
        assert fetched["event_type"] == "login"

        listed = db.documents.list("conf_events", limit=10)
        assert len(listed["items"]) >= 1

        patched = db.documents.patch("conf_events", rid, {"attempts": 3})
        # Top-level merge patch: unrelated fields MUST survive.
        assert patched["event_type"] == "login"

        deleted = db.documents.delete("conf_events", rid)
        assert deleted["affected"] == 1
        assert deleted["deleted"] is True


# ----------------------------------------------- documents.delete_missing_no_error
def test_documents_delete_missing_no_error():
    with reddb.connect("memory://") as db:
        # Seed the collection so the table exists.
        ins = db.documents.insert("conf_events_missing", {"k": "v"})
        db.documents.delete("conf_events_missing", str(ins["rid"]))
        # Deleting an absent rid MUST NOT raise; affected = 0.
        r = db.documents.delete("conf_events_missing", "rid_that_does_not_exist")
        assert r["affected"] == 0
        assert r["deleted"] is False


# --------------------------------------------------- documents.patch_empty_rejects
def test_documents_patch_empty_rejects():
    with reddb.connect("memory://") as db:
        ins = db.documents.insert("conf_events_patch", {"k": "v"})
        rid = str(ins["rid"])
        try:
            db.documents.patch("conf_events_patch", rid, {})
        except ValueError as exc:
            assert "INVALID_ARGUMENT" in str(exc), f"expected INVALID_ARGUMENT, got {exc}"
        else:
            raise AssertionError("expected INVALID_ARGUMENT on empty patch")


# ----------------------------------------------- errors.not_found.document_get
def test_errors_not_found_document_get():
    with reddb.connect("memory://") as db:
        # Seed + delete so the table exists but the rid is absent.
        ins = db.documents.insert("conf_errors_nf", {"k": "v"})
        db.documents.delete("conf_errors_nf", str(ins["rid"]))
        try:
            db.documents.get("conf_errors_nf", "rid_definitely_missing")
        except ValueError as exc:
            assert "NOT_FOUND" in str(exc), f"expected NOT_FOUND, got {exc}"
        else:
            raise AssertionError("expected NOT_FOUND on missing rid")


if __name__ == "__main__":
    cases = [
        ("documents.crud_nested_patch", test_documents_crud_nested_patch),
        ("documents.delete_missing_no_error", test_documents_delete_missing_no_error),
        ("documents.patch_empty_rejects", test_documents_patch_empty_rejects),
        ("errors.not_found.document_get", test_errors_not_found_document_get),
    ]
    passed = failed = 0
    for name, fn in cases:
        try:
            fn()
            print(f"  ok  {name}")
            passed += 1
        except Exception as exc:
            print(f"  FAIL {name}: {exc}")
            failed += 1
    print(f"\n{passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)

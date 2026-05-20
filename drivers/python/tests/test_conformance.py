"""
SDK Helper Spec conformance harness (Python driver port).

One test per case ID in §12 of ``docs/spec/sdk-helpers.md``. Case IDs are
ported verbatim (dots → underscores in the test function name) so cross-driver
CI dashboards line up with the reference Rust harness at
``crates/reddb-client/tests/conformance.rs``.

The Python driver embeds the engine, so every case runs against ``memory://``
in-process — no server to spawn. The four provisional namespaces
(``vectors``, ``graph``, ``timeseries``, ``probabilistic``) have no first-class
helpers in v1.0; their cases pin the wire-level SQL surface reachable via
``db.query`` (spec §8–§11).

Run after ``maturin develop``::

    python -m pytest drivers/python/tests/test_conformance.py
"""

import sys

import reddb


# ----------------------------------------------------------------- generic.*
def test_generic_query_no_params():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE conf_q1 (name TEXT)")
        db.query("INSERT INTO conf_q1 (name) VALUES ('alice')")
        r = db.query("SELECT name FROM conf_q1")
        assert "columns" in r and "rows" in r and "affected" in r
        assert any(row.get("name") == "alice" for row in r["rows"])


def test_generic_query_with_params():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE conf_q2 (id INT, name TEXT)")
        db.insert("conf_q2", {"id": 1, "name": "alice"})
        db.insert("conf_q2", {"id": 2, "name": "bob"})
        r = db.query("SELECT name FROM conf_q2 WHERE id = $1", 2)
        assert [row["name"] for row in r["rows"]] == ["bob"]


def test_generic_insert_rid():
    with reddb.connect("memory://") as db:
        r = db.insert("conf_ins", {"name": "alice"})
        assert r["affected"] == 1
        # rid is lossless and round-trips as a string.
        assert int(r["rid"]) > 0
        assert r["id"] == r["rid"]


def test_generic_bulk_insert_rids():
    with reddb.connect("memory://") as db:
        # Empty input is a no-op returning {affected: 0, rids: []}.
        empty = db.bulk_insert("conf_bulk", [])
        assert empty["affected"] == 0
        assert empty["rids"] == []

        many = db.bulk_insert(
            "conf_bulk",
            [{"name": "a"}, {"name": "b"}, {"name": "c"}],
        )
        assert many["affected"] == 3
        assert len(many["rids"]) == 3
        # rids are unique and preserve per-row identity.
        assert len(set(str(r) for r in many["rids"])) == 3


def test_generic_delete():
    with reddb.connect("memory://") as db:
        ins = db.insert("conf_del", {"name": "alice"})
        r = db.delete("conf_del", str(ins["rid"]))
        assert r["affected"] == 1


# --------------------------------------------------------------- documents.*
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


def test_documents_delete_missing_no_error():
    with reddb.connect("memory://") as db:
        ins = db.documents.insert("conf_events_missing", {"k": "v"})
        db.documents.delete("conf_events_missing", str(ins["rid"]))
        r = db.documents.delete("conf_events_missing", "999999999")
        assert r["affected"] == 0
        assert r["deleted"] is False


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


# ---------------------------------------------------------------------- kv.*
def test_kv_exact_key_round_trip():
    with reddb.connect("memory://") as db:
        key = "characters:hansel"
        db.kv.set("conf_kv", key, {"role": "finder", "steps": 7})
        assert db.kv.exists("conf_kv", key) == {"exists": True}
        assert db.kv.get("conf_kv", key) == {"role": "finder", "steps": 7}
        listed = db.kv.list("conf_kv", prefix="characters:")
        assert listed["items"] == [
            {"key": "characters:hansel", "value": {"role": "finder", "steps": 7}}
        ]


def test_kv_missing_get_returns_none():
    with reddb.connect("memory://") as db:
        db.kv.set("conf_kv_missing", "present", {"v": 1})
        # A missing key returns None — NOT a NOT_FOUND error (spec §5.2).
        assert db.kv.get("conf_kv_missing", "absent") is None


def test_kv_delete_returns_envelope():
    with reddb.connect("memory://") as db:
        key = "to-delete"
        db.kv.set("conf_kv_del", key, {"v": 1})
        r = db.kv.delete("conf_kv_del", key)
        assert r["affected"] == 1
        assert r["deleted"] is True
        assert db.kv.exists("conf_kv_del", key) == {"exists": False}


# ------------------------------------------------------------------- queues.*
def test_queues_fifo_peek_pop_len():
    with reddb.connect("memory://") as db:
        db.queues.create("conf_q")
        db.queues.push("conf_q", {"n": 1})
        db.queues.push("conf_q", {"n": 2})

        assert db.queues.len("conf_q") == 2

        peeked = db.queues.peek("conf_q", 1)
        assert len(peeked["items"]) == 1
        # peek must not decrement length.
        assert db.queues.len("conf_q") == 2

        popped = db.queues.pop("conf_q")
        assert len(popped["items"]) == 1
        assert db.queues.len("conf_q") == 1


def test_queues_empty_pop_returns_empty():
    with reddb.connect("memory://") as db:
        db.queues.create("conf_q_empty")
        r = db.queues.pop("conf_q_empty")
        # Empty queue pop → empty items, NOT an error.
        assert r["items"] == []


def test_queues_purge_resets_len():
    with reddb.connect("memory://") as db:
        db.queues.create("conf_q_purge")
        for i in range(3):
            db.queues.push("conf_q_purge", {"i": i})
        assert db.queues.len("conf_q_purge") == 3
        db.queues.purge("conf_q_purge")
        assert db.queues.len("conf_q_purge") == 0


# ----------------------------------------------------------------------- tx.*
def test_tx_commit_persists():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE conf_tx_commit (name TEXT)")
        db.tx.begin()
        db.query("INSERT INTO conf_tx_commit (name) VALUES ('keep')")
        db.tx.commit()
        r = db.query("SELECT name FROM conf_tx_commit WHERE name = 'keep'")
        assert len(r["rows"]) == 1


def test_tx_rollback_discards():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE conf_tx_rb (name TEXT)")
        db.tx.begin()
        db.query("INSERT INTO conf_tx_rb (name) VALUES ('drop')")
        db.tx.rollback()
        r = db.query("SELECT name FROM conf_tx_rb WHERE name = 'drop'")
        assert r["rows"] == []


# ------------------------------------------------------------------- errors.*
def test_errors_invalid_argument_empty_sql():
    with reddb.connect("memory://") as db:
        try:
            db.query("")
        except ValueError as exc:
            assert "INVALID_ARGUMENT" in str(exc), f"expected INVALID_ARGUMENT, got {exc}"
        else:
            raise AssertionError("expected INVALID_ARGUMENT on empty SQL")


def test_errors_not_found_document_get():
    with reddb.connect("memory://") as db:
        ins = db.documents.insert("conf_errors_nf", {"k": "v"})
        db.documents.delete("conf_errors_nf", str(ins["rid"]))
        try:
            db.documents.get("conf_errors_nf", "999999999")
        except ValueError as exc:
            assert "NOT_FOUND" in str(exc), f"expected NOT_FOUND, got {exc}"
        else:
            raise AssertionError("expected NOT_FOUND on missing rid")


# ------------------------- wire.* (provisional: SQL-only namespaces) --------
def test_wire_vectors_sql_round_trip():
    with reddb.connect("memory://") as db:
        db.query("CREATE VECTOR conf_vec DIM 3 METRIC cosine")
        db.query("INSERT INTO conf_vec VECTOR (dense) VALUES ([0.1, 0.2, 0.3])")
        r = db.query("SEARCH SIMILAR $1 COLLECTION conf_vec LIMIT 5", [0.1, 0.2, 0.3])
        assert "rows" in r and len(r["rows"]) >= 1


def test_wire_graph_sql_round_trip():
    with reddb.connect("memory://") as db:
        # The SQL-side graph surface exposes Cypher MATCH; edge/node mutation
        # is reached through the graph API, not SQL DDL. The conformance bar
        # here is "the query reaches the engine without a parse error".
        r = db.query("MATCH (a)-[r]->(b) RETURN a, b")
        assert "rows" in r


def test_wire_timeseries_sql_round_trip():
    with reddb.connect("memory://") as db:
        db.query("CREATE TIMESERIES conf_cpu RETENTION 90 d")
        db.query(
            'INSERT INTO conf_cpu (metric, value, tags) '
            'VALUES (\'cpu.idle\', 95.2, {"host":"h1"})'
        )
        r = db.query("SELECT * FROM conf_cpu")
        assert "rows" in r and len(r["rows"]) >= 1


def test_wire_probabilistic_hll_round_trip():
    with reddb.connect("memory://") as db:
        db.query("CREATE HLL conf_visitors")
        db.query("HLL ADD conf_visitors 'alice' 'bob' 'alice'")
        r = db.query("HLL COUNT conf_visitors")
        assert r["rows"], "HLL COUNT must return a row"
        row = r["rows"][0]
        # Engine column is `count`; some drivers project `cardinality`. Pin the
        # value contract, not the column name.
        count = row.get("count", row.get("cardinality"))
        assert count is not None, f"HLL COUNT must return count/cardinality, got {row}"
        assert float(count) >= 1


# Helper-surface invariant (spec §14).
def test_helper_spec_version_is_advertised():
    assert reddb.helper_spec_version == "1.0"
    with reddb.connect("memory://") as db:
        assert db.helper_spec_version == "1.0"


if __name__ == "__main__":
    cases = [
        ("generic.query.no_params", test_generic_query_no_params),
        ("generic.query_with.params", test_generic_query_with_params),
        ("generic.insert.rid", test_generic_insert_rid),
        ("generic.bulk_insert.rids", test_generic_bulk_insert_rids),
        ("generic.delete", test_generic_delete),
        ("documents.crud_nested_patch", test_documents_crud_nested_patch),
        ("documents.delete_missing_no_error", test_documents_delete_missing_no_error),
        ("documents.patch_empty_rejects", test_documents_patch_empty_rejects),
        ("kv.exact_key_round_trip", test_kv_exact_key_round_trip),
        ("kv.missing_get_returns_none", test_kv_missing_get_returns_none),
        ("kv.delete_returns_envelope", test_kv_delete_returns_envelope),
        ("queues.fifo_peek_pop_len", test_queues_fifo_peek_pop_len),
        ("queues.empty_pop_returns_empty", test_queues_empty_pop_returns_empty),
        ("queues.purge_resets_len", test_queues_purge_resets_len),
        ("tx.commit_persists", test_tx_commit_persists),
        ("tx.rollback_discards", test_tx_rollback_discards),
        ("errors.invalid_argument.empty_sql", test_errors_invalid_argument_empty_sql),
        ("errors.not_found.document_get", test_errors_not_found_document_get),
        ("wire.vectors.sql_round_trip", test_wire_vectors_sql_round_trip),
        ("wire.graph.sql_round_trip", test_wire_graph_sql_round_trip),
        ("wire.timeseries.sql_round_trip", test_wire_timeseries_sql_round_trip),
        ("wire.probabilistic.hll_round_trip", test_wire_probabilistic_hll_round_trip),
        ("helper_spec_version", test_helper_spec_version_is_advertised),
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

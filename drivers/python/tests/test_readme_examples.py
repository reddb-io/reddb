"""
Executes the runnable code examples published in README.md against a real
embedded engine, so the docs can't drift from the API. One example per data
model (relational, documents, KV, queues, transactions, graph, vector,
time-series) plus isolation levels and the serialization-conflict retry
contract.

Run after `maturin develop`:

    python -m pytest tests/test_readme_examples.py
    # or, with no pytest:
    python tests/test_readme_examples.py
"""

import sys
import time

import reddb


def test_readme_quickstart():
    with reddb.connect("memory://") as db:
        db.insert("users", {"name": "Alice", "age": 30})
        inserted = db.bulk_insert("users", [{"name": "Bob"}, {"name": "Carol"}])
        assert len(inserted["rids"]) == 2

        result = db.query("SELECT * FROM users")
        assert isinstance(result["rows"], list)


def test_readme_documents():
    with reddb.connect("memory://") as db:
        doc = db.documents.insert("profiles", {"name": "Hansel", "details": {"trail": "crumbs"}})
        fetched = db.documents.get("profiles", str(doc["rid"]))
        assert fetched["name"] == "Hansel"
        updated = db.documents.patch("profiles", str(doc["rid"]), {"reviewed": True})
        assert updated["name"] == "Hansel"  # merge preserves unrelated fields
        deleted = db.documents.delete("profiles", str(doc["rid"]))
        assert deleted["deleted"] is True


def test_readme_kv():
    with reddb.connect("memory://") as db:
        db.kv.set("settings", "characters:hansel", {"role": "finder"})
        assert db.kv.get("settings", "characters:hansel") == {"role": "finder"}
        assert db.kv.get("settings", "missing") is None  # not an error
        deleted = db.kv.delete("settings", "characters:hansel")
        assert deleted["deleted"] is True


def test_readme_queues():
    with reddb.connect("memory://") as db:
        db.queues.create("jobs")
        db.queues.push("jobs", {"task": "reindex"})
        assert db.queues.len("jobs") == 1
        assert len(db.queues.pop("jobs")["items"]) == 1  # FIFO
        assert db.queues.len("jobs") == 0


def test_readme_transactions():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE audit (action TEXT)")
        # callback form: a clean return commits, a raise rolls back + re-raises
        db.tx.run(lambda tx: db.insert("users", {"name": "Dave"}))
        rows = db.query("SELECT * FROM users")["rows"]
        assert len(rows) == 1


def test_readme_graph():
    with reddb.connect("memory://") as db:
        # The first user-inserted item gets rid 1024 (1..1023 are reserved).
        db.query("INSERT INTO network NODE (label, node_type) VALUES ('gateway', 'Host')")
        db.query("INSERT INTO network NODE (label, node_type) VALUES ('app', 'Host')")
        db.query("INSERT INTO network NODE (label, node_type) VALUES ('db', 'Host')")
        db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1024, 1025, 1.0)")
        db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1025, 1026, 1.0)")
        db.query("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1024, 1026, 5.0)")

        path = db.query("GRAPH SHORTEST_PATH '1024' TO '1026' ALGORITHM dijkstra")
        # Dijkstra prefers the two 1.0-weight hops over the single 5.0 edge.
        assert path["rows"][0]["total_weight"] == 2


def test_readme_vector():
    with reddb.connect("memory://") as db:
        db.query("INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'gateway runbook')")
        db.query("INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.0, 1.0], 'database manual')")

        # A Python list of floats binds as a single Vector param.
        hits = db.query("SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 1", [1.0, 0.0])
        assert len(hits["rows"]) == 1
        assert hits["rows"][0]["score"] == 1  # the identical vector scores exactly 1


def test_readme_timeseries():
    with reddb.connect("memory://") as db:
        db.query("CREATE TIMESERIES metrics RETENTION 7 d CHUNK_SIZE 64 DOWNSAMPLE 1h:5m:avg")
        db.query("INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 10.0, '{\"host\":\"srv-a\"}', 0)")
        db.query("INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 20.0, '{\"host\":\"srv-a\"}', 60000000000)")
        db.query("INSERT INTO metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 30.0, '{\"host\":\"srv-b\"}', 300000000000)")

        rollup = db.query(
            "SELECT time_bucket(5m) AS bucket, avg(value) AS avg_value, count(*) AS samples "
            "FROM metrics WHERE metric = 'cpu.usage' GROUP BY time_bucket(5m)"
        )
        assert len(rollup["rows"]) == 2  # two five-minute buckets


def test_readme_isolation_level():
    with reddb.connect("memory://") as db:
        db.query("CREATE TABLE audit (action TEXT)")
        db.query("BEGIN ISOLATION LEVEL SERIALIZABLE")
        db.query("INSERT INTO audit (action) VALUES ('serializable write')")
        db.query("COMMIT")
        rows = db.query("SELECT action FROM audit")["rows"]
        assert len(rows) == 1


def _is_serialization_conflict(err: Exception) -> bool:
    """True only for the retryable first-committer-wins conflict."""
    msg = str(err)
    return "QUERY_ERROR" in msg and "serialization conflict" in msg.lower()


def test_readme_serialization_retry():
    with reddb.connect("memory://") as db:
        def with_retry(fn, max_retries: int = 5):
            attempt = 0
            while True:
                try:
                    return db.tx.run(fn)
                except ValueError as err:
                    if _is_serialization_conflict(err) and attempt < max_retries:
                        attempt += 1
                        time.sleep(2 ** attempt * 0.005)  # backoff
                        continue
                    raise  # not retryable, or out of attempts

        db.query("CREATE TABLE ledger (entry TEXT)")
        result = with_retry(lambda tx: db.query("INSERT INTO ledger (entry) VALUES ('committed')"))
        assert result is not None

        # The classifier fires only for real serialization conflicts.
        assert _is_serialization_conflict(
            ValueError("[QUERY_ERROR] serialization conflict: table row accounts/1 "
                       "was modified by concurrent transaction 42")
        )
        assert not _is_serialization_conflict(ValueError("[QUERY_ERROR] syntax error near FROM"))


# -----------------------------------------------------------------------
# Plain-stdlib runner so the file can be executed without pytest.
# -----------------------------------------------------------------------
if __name__ == "__main__":
    tests = [(name, fn) for name, fn in sorted(globals().items())
             if name.startswith("test_") and callable(fn)]
    passed = failed = 0
    for name, fn in tests:
        try:
            fn()
            print(f"  ok  {name}")
            passed += 1
        except Exception as exc:  # noqa: BLE001
            print(f"  FAIL {name}: {exc}")
            failed += 1
    print(f"\n{passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)

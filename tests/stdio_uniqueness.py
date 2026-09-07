"""Real stdio runtime uniqueness regressions; set REDDB_BINARY_PATH to the built binary."""
import json
import os
import select
from pathlib import Path
import subprocess
import tempfile
import unittest


class Uniqueness(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="reddb-unique-")
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "db.rdb"

        self.process = subprocess.Popen(
            [os.environ["REDDB_BINARY_PATH"], "rpc", "--stdio", "--path", str(self.path)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.addCleanup(self.close)
        self.sequence = 0

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.process.stdout.close()

    def query(self, sql, success=True):
        self.sequence += 1
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.sequence,
                                             "method": "query", "params": {"sql": sql}}) + "\n")
        self.process.stdin.flush()
        ready, _, _ = select.select([self.process.stdout], [], [], 30)
        self.assertTrue(ready, "RPC response timed out")
        r = json.loads(self.process.stdout.readline())
        self.assertEqual("error" not in r, success, r)
        return {"data": r["result"]} if success else r["error"]

    def test_primary_key_rejects_duplicates_and_preserves_payload(self):
        self.query("CREATE TABLE events (id TEXT PRIMARY KEY, payload TEXT)")
        self.query("INSERT INTO events (id,payload) VALUES ('a','original')")
        self.query("INSERT INTO events (id,payload) VALUES ('a','replacement')", False)
        rows = self.query("SELECT id,payload FROM events")["data"]["rows"]
        self.assertEqual([{k: row[k] for k in ("id", "payload")} for row in rows],
                         [{"id": "a", "payload": "original"}])
        self.query("INSERT INTO events (id,payload) VALUES (NULL,'missing')", False)

    def test_unique_nulls_remain_distinct(self):
        self.query("CREATE TABLE users (id INT PRIMARY KEY, email TEXT UNIQUE)")
        self.query("INSERT INTO users (id,email) VALUES (1,NULL),(2,NULL),(3,'x')")
        self.query("INSERT INTO users (id,email) VALUES (4,'x')", False)
        self.assertEqual(len(self.query("SELECT id FROM users")["data"]["rows"]), 3)

    def test_conflict_target_updates_the_existing_row(self):
        self.query("CREATE TABLE pairs (id INT PRIMARY KEY, payload TEXT)")
        self.query("INSERT INTO pairs (id,payload) VALUES (1,'old'),(2,'other')")
        self.query("INSERT INTO pairs (id,payload) VALUES (1,'new') "
                   "ON CONFLICT (id) DO UPDATE SET payload=EXCLUDED.payload")
        rows = self.query("SELECT id,payload FROM pairs")["data"]["rows"]
        self.assertEqual({row["id"]: row["payload"] for row in rows}, {1: "new", 2: "other"})

    def test_update_keeps_own_key_and_rejects_other_key(self):
        self.query("CREATE TABLE rows_unique (id INT PRIMARY KEY, payload TEXT)")
        self.query("INSERT INTO rows_unique (id,payload) VALUES (1,'a'),(2,'b')")
        self.query("UPDATE rows_unique SET payload='changed' WHERE id=1")
        self.query("UPDATE rows_unique SET id=2 WHERE id=1", False)
        rows = self.query("SELECT id,payload FROM rows_unique")["data"]["rows"]
        self.assertEqual({row["id"]: row["payload"] for row in rows}, {1: "changed", 2: "b"})


if __name__ == "__main__":
    unittest.main()

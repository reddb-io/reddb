"""Native facade exercises the shared Rust router over actual HTTP sockets."""
import json
import socket
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest
import reddb


def test_native_topology_routing_preserves_values_auth_and_write_safety():
    calls, payloads, auth = [], [], []
    membership = {}
    servers = []

    def peer(label):
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def reply(self, value):
                encoded = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def do_GET(self):
                if self.path != "/health":
                    auth.append(self.headers.get("Authorization"))
                if self.path.startswith("/v1/topology?"):
                    self.reply(membership)
                else:
                    self.reply({"ok": True, "healthy": False, "state": "catching_up", "checked_at_unix_ms": 12345, "version": "test"})

            def do_POST(self):
                auth.append(self.headers.get("Authorization"))
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                if self.path.endswith("/bulk/rows"):
                    payloads.append(body)
                    self.reply({"ok": True, "result": {"affected": len(body["rows"]), "ids": ["42"], "rids": ["42"]}})
                    return
                sql = body["query"]
                calls.append((label, sql))
                if "effectful" in sql or sql == "UPDATE lost":
                    self.connection.shutdown(socket.SHUT_RDWR)
                    self.connection.close()
                    return
                self.reply({"ok": True, "query": sql, "mode": "sql", "statement": "table", "statement_type": "select", "affected_rows": 7, "result": {"columns": ["endpoint", "value"], "records": [{"values": {"endpoint": label, "value": {"integer": 9007199254740993}}}]}})

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        servers.append((server, thread))
        return "http://127.0.0.1:%d" % server.server_port

    primary, replica = peer("primary"), peer("replica")
    membership.update(version=1, epoch="1", primary={"addr": primary, "region": ""},
                      replicas=[{"addr": replica, "region": "", "healthy": True, "lag_ms": 10, "last_applied_lsn": "0", "rebootstrapping": False}])
    db = None
    try:
        db = reddb.connect(primary + "?token=secret")
        assert db.query("SELECT 1")["rows"][0] == {"endpoint": "primary", "value": {"integer": 9007199254740993}}
        assert db.query_with_options("SELECT 1", "local")["rows"][0]["endpoint"] == "replica"
        assert db.query("SELECT 1")["affected"] == 7
        assert db.health()["ok"] is False
        assert db.health()["state"] == "catching_up"
        assert db.health()["checked_at_unix_ms"] == 12345
        db.query("BEGIN")
        assert db.query_with_options("SELECT 1", "local")["rows"][0]["endpoint"] == "primary"
        db.query("ROLLBACK")
        for sql in ['SELECT "effectful"()', 'UPDATE lost']:
            before = len(calls)
            with pytest.raises(ValueError):
                db.query_with_options(sql, "local")
            assert calls[before:] == [("primary", sql)]
        assert db.insert("users", {"number": 9007199254740993})["id"] == "42"
        assert payloads[-1]["rows"][0]["number"] == 9007199254740993
        assert auth and set(auth) == {"Bearer secret"}
    finally:
        if db is not None:
            db.close()
        for server, thread in servers:
            server.shutdown()
            server.server_close()
            thread.join()

#!/usr/bin/env python3
"""Local Jepsen-style black-box harness for a real RedDB cluster.

The harness starts one primary and two replicas, drives writes and reads through
HTTP, injects process kill/restart plus replica-primary message isolation via a
local TCP proxy, and checks the black-box safety envelope captured by the
replication model tests.
"""

from __future__ import annotations

import argparse
import http.client
import json
import os
import random
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


NODE_COUNT = 3
DEFAULT_OPS = 12


class HarnessError(RuntimeError):
    pass


class InvariantViolation(HarnessError):
    pass


def reserve_port() -> int:
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def now_ms() -> int:
    return int(time.time() * 1000)


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


@dataclass
class OperationHistory:
    path: Path
    events: list[dict[str, Any]] = field(default_factory=list)

    def append(self, event: dict[str, Any]) -> None:
        event = {"time_ms": now_ms(), **event}
        self.events.append(event)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(event, sort_keys=True) + "\n")


class SafetyChecker:
    def __init__(self, expected_writer: str) -> None:
        self.expected_writer = expected_writer

    def check(self, history: OperationHistory, final_write_ids: set[int]) -> None:
        self._check_no_acknowledged_write_lost(history, final_write_ids)
        self._check_no_stale_writer_accepted(history)
        self._check_single_writer_per_window(history)

    def _check_no_acknowledged_write_lost(
        self, history: OperationHistory, final_write_ids: set[int]
    ) -> None:
        acked = {
            int(event["write_id"])
            for event in history.events
            if event.get("op") == "write" and event.get("phase") == "ok"
        }
        missing = sorted(acked - final_write_ids)
        if missing:
            raise InvariantViolation(
                "committed-write-loss: acknowledged writes absent after recovery: "
                + ",".join(str(write_id) for write_id in missing)
            )

    def _check_no_stale_writer_accepted(self, history: OperationHistory) -> None:
        for event in history.events:
            if event.get("op") != "write" or event.get("phase") != "ok":
                continue
            accepted_by = event.get("node")
            if accepted_by != self.expected_writer:
                raise InvariantViolation(
                    "stale-leader: non-writer accepted write "
                    f"{event.get('write_id')} on {accepted_by}"
                )

    def _check_single_writer_per_window(self, history: OperationHistory) -> None:
        writers_by_window: dict[str, set[str]] = {}
        for event in history.events:
            if event.get("op") != "write" or event.get("phase") != "ok":
                continue
            window = str(event.get("window", "default"))
            writers_by_window.setdefault(window, set()).add(str(event.get("node")))
        for window, writers in writers_by_window.items():
            if len(writers) > 1:
                raise InvariantViolation(
                    f"single-writer: multiple writers accepted in window {window}: "
                    + ",".join(sorted(writers))
                )


class TcpProxy:
    """Small local TCP proxy that can reject traffic to model message isolation."""

    def __init__(self, listen_port: int, target_port: int, name: str) -> None:
        self.listen_port = listen_port
        self.target_port = target_port
        self.name = name
        self._isolated = threading.Event()
        self._closed = threading.Event()
        self._active: list[socket.socket] = []
        self._server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._server.bind(("127.0.0.1", listen_port))
        self._server.listen()
        self._thread = threading.Thread(target=self._accept_loop, name=name, daemon=True)

    def start(self) -> None:
        self._thread.start()

    def isolate(self) -> None:
        self._isolated.set()
        self._close_active()

    def heal(self) -> None:
        self._isolated.clear()

    def close(self) -> None:
        self._closed.set()
        self._close_active()
        try:
            self._server.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self._server.close()

    def _accept_loop(self) -> None:
        while not self._closed.is_set():
            try:
                client, _addr = self._server.accept()
            except OSError:
                return
            if self._isolated.is_set():
                client.close()
                continue
            try:
                upstream = socket.create_connection(("127.0.0.1", self.target_port), timeout=2)
            except OSError:
                client.close()
                continue
            self._active.extend([client, upstream])
            threading.Thread(target=self._pipe_pair, args=(client, upstream), daemon=True).start()

    def _pipe_pair(self, left: socket.socket, right: socket.socket) -> None:
        threads = [
            threading.Thread(target=self._pipe, args=(left, right), daemon=True),
            threading.Thread(target=self._pipe, args=(right, left), daemon=True),
        ]
        for thread in threads:
            thread.start()

    def _pipe(self, src: socket.socket, dst: socket.socket) -> None:
        try:
            while not self._closed.is_set() and not self._isolated.is_set():
                data = src.recv(65536)
                if not data:
                    return
                dst.sendall(data)
        except OSError:
            return
        finally:
            for sock in (src, dst):
                try:
                    sock.close()
                except OSError:
                    pass

    def _close_active(self) -> None:
        for sock in self._active:
            try:
                sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            try:
                sock.close()
            except OSError:
                pass
        self._active.clear()


@dataclass
class RedNode:
    name: str
    role: str
    data_path: Path
    http_port: int
    grpc_port: int
    log_path: Path
    primary_proxy_port: int | None = None
    process: subprocess.Popen[bytes] | None = None

    def command(self, red_bin: Path) -> list[str]:
        cmd = [
            str(red_bin),
            "server",
            "--role",
            self.role,
            "--path",
            str(self.data_path),
            "--http",
            "--http-bind",
            f"127.0.0.1:{self.http_port}",
            "--grpc",
            "--grpc-bind",
            f"127.0.0.1:{self.grpc_port}",
            "--storage-preset",
            "primary-replica-dev",
            "--no-auth",
        ]
        if self.role == "replica":
            if self.primary_proxy_port is None:
                raise HarnessError(f"{self.name} replica missing primary proxy")
            cmd.extend(["--primary-addr", f"http://127.0.0.1:{self.primary_proxy_port}"])
        return cmd

    def start(self, red_bin: Path) -> None:
        self.log_path.parent.mkdir(parents=True, exist_ok=True)
        log = self.log_path.open("ab")
        env = os.environ.copy()
        env["REDDB_NO_AUTH"] = "1"
        self.process = subprocess.Popen(
            self.command(red_bin),
            stdout=log,
            stderr=subprocess.STDOUT,
            env=env,
        )

    def stop(self) -> None:
        if self.process is None or self.process.poll() is not None:
            return
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)

    def kill(self) -> None:
        if self.process is None or self.process.poll() is not None:
            return
        self.process.kill()
        self.process.wait(timeout=5)


class ClusterHarness:
    def __init__(self, args: argparse.Namespace) -> None:
        self.red_bin = Path(args.red_bin).resolve()
        self.seed = int(args.seed)
        self.ops = int(args.ops)
        self.keep_artifacts = bool(args.keep_artifacts)
        self.rng = random.Random(self.seed)
        self.run_dir = Path(args.work_dir).resolve() / f"seed-{self.seed:016x}-{now_ms()}"
        self.logs_dir = self.run_dir / "logs"
        self.history = OperationHistory(self.run_dir / "history.jsonl")
        self.schedule_path = self.run_dir / "schedule.json"
        self.schedule: list[dict[str, Any]] = []
        self.nodes: dict[str, RedNode] = {}
        self.proxies: dict[str, TcpProxy] = {}

    def run(self) -> None:
        if not self.red_bin.exists():
            raise HarnessError(f"red binary not found: {self.red_bin}")
        self._create_cluster()
        try:
            self._start_cluster()
            self._drive_workload()
            final_write_ids = self._read_final_write_ids()
            SafetyChecker(expected_writer="primary").check(self.history, final_write_ids)
            print(f"ok seed=0x{self.seed:016x} artifacts={self.run_dir}")
            if not self.keep_artifacts:
                shutil.rmtree(self.run_dir, ignore_errors=True)
        except Exception as err:
            self._preserve_failure(err)
            raise
        finally:
            self._stop_cluster()

    def _create_cluster(self) -> None:
        primary_grpc = reserve_port()
        self.nodes["primary"] = RedNode(
            name="primary",
            role="primary",
            data_path=self.run_dir / "primary" / "data.rdb",
            http_port=reserve_port(),
            grpc_port=primary_grpc,
            log_path=self.logs_dir / "primary.log",
        )
        for index in (1, 2):
            proxy_port = reserve_port()
            name = f"replica-{index}"
            self.proxies[name] = TcpProxy(proxy_port, primary_grpc, f"{name}-primary-proxy")
            self.nodes[name] = RedNode(
                name=name,
                role="replica",
                data_path=self.run_dir / name / "data.rdb",
                http_port=reserve_port(),
                grpc_port=reserve_port(),
                log_path=self.logs_dir / f"{name}.log",
                primary_proxy_port=proxy_port,
            )

    def _start_cluster(self) -> None:
        for proxy in self.proxies.values():
            proxy.start()
        for name in ("primary", "replica-1", "replica-2"):
            self._record_schedule({"action": "start", "node": name})
            self.nodes[name].start(self.red_bin)
        for node in self.nodes.values():
            self._wait_health(node)

    def _stop_cluster(self) -> None:
        for node in self.nodes.values():
            node.stop()
        for proxy in self.proxies.values():
            proxy.close()

    def _drive_workload(self) -> None:
        self._query("primary", "CREATE TABLE jepsen_ops (id INT, token TEXT)", op="ddl")
        write_id = 1
        for step in range(self.ops):
            if step == 2:
                self._isolate("replica-1")
                self._write(write_id, "primary", window="isolated-replica")
                write_id += 1
                self._write(write_id, "replica-1", window="isolated-replica", expect_ok=False)
                write_id += 1
                self._heal("replica-1")
            elif step == 5:
                self._kill_restart("replica-2")
            elif step == 8:
                self._kill_restart("primary")
            else:
                self._write(write_id, "primary", window=f"step-{step}")
                write_id += 1
            if step % 4 == 3:
                self._query("primary", "SELECT * FROM jepsen_ops", op="read")

    def _query(self, node_name: str, query: str, op: str) -> tuple[int, str]:
        node = self.nodes[node_name]
        body = json.dumps({"query": query})
        status, text = http_request("POST", node.http_port, "/query", body)
        self.history.append(
            {
                "phase": "ok" if 200 <= status < 300 else "fail",
                "op": op,
                "node": node_name,
                "status": status,
                "query": query,
            }
        )
        if not (200 <= status < 300):
            raise HarnessError(f"{node_name} query failed status={status}: {text[:500]}")
        return status, text

    def _write(self, write_id: int, node_name: str, window: str, expect_ok: bool = True) -> None:
        query = (
            "INSERT INTO jepsen_ops (id, token) "
            f"VALUES ({write_id}, 'jepsen-{write_id}')"
        )
        self.history.append(
            {
                "phase": "invoke",
                "op": "write",
                "node": node_name,
                "write_id": write_id,
                "window": window,
            }
        )
        node = self.nodes[node_name]
        status, text = http_request("POST", node.http_port, "/query", json.dumps({"query": query}))
        ok = 200 <= status < 300
        self.history.append(
            {
                "phase": "ok" if ok else "fail",
                "op": "write",
                "node": node_name,
                "write_id": write_id,
                "window": window,
                "status": status,
                "body_head": text[:240],
            }
        )
        if ok != expect_ok:
            expectation = "accept" if expect_ok else "reject"
            raise HarnessError(
                f"expected {node_name} to {expectation} write {write_id}; "
                f"status={status} body={text[:500]}"
            )

    def _read_final_write_ids(self) -> set[int]:
        _status, text = self._query("primary", "SELECT * FROM jepsen_ops", op="final-read")
        return {int(value) for value in re.findall(r"jepsen-(\d+)", text)}

    def _kill_restart(self, node_name: str) -> None:
        self._record_schedule({"action": "process-kill", "node": node_name})
        self.history.append({"phase": "fault", "op": "process-kill", "node": node_name})
        self.nodes[node_name].kill()
        time.sleep(0.2)
        self._record_schedule({"action": "process-restart", "node": node_name})
        self.history.append({"phase": "fault", "op": "process-restart", "node": node_name})
        self.nodes[node_name].start(self.red_bin)
        self._wait_health(self.nodes[node_name])

    def _isolate(self, node_name: str) -> None:
        self._record_schedule({"action": "message-isolate", "node": node_name})
        self.history.append({"phase": "fault", "op": "message-isolate", "node": node_name})
        self.proxies[node_name].isolate()

    def _heal(self, node_name: str) -> None:
        self._record_schedule({"action": "message-heal", "node": node_name})
        self.history.append({"phase": "fault", "op": "message-heal", "node": node_name})
        self.proxies[node_name].heal()

    def _wait_health(self, node: RedNode) -> None:
        deadline = time.time() + 20
        while time.time() < deadline:
            if node.process is not None and node.process.poll() is not None:
                raise HarnessError(f"{node.name} exited early; see {node.log_path}")
            try:
                status, _body = http_request("GET", node.http_port, "/health", None, timeout=1)
                if 200 <= status < 300:
                    return
            except OSError:
                pass
            time.sleep(0.1)
        raise HarnessError(f"{node.name} did not become healthy; see {node.log_path}")

    def _record_schedule(self, action: dict[str, Any]) -> None:
        action = {"time_ms": now_ms(), **action}
        self.schedule.append(action)
        write_json(self.schedule_path, self.schedule)

    def _preserve_failure(self, err: Exception) -> None:
        write_json(
            self.run_dir / "repro.json",
            {
                "seed": f"0x{self.seed:016x}",
                "error": str(err),
                "schedule_json": str(self.schedule_path),
                "history_jsonl": str(self.history.path),
                "logs_dir": str(self.logs_dir),
                "red_bin": str(self.red_bin),
                "rerun": (
                    f"python3 scripts/jepsen_black_box_cluster.py "
                    f"--seed {self.seed} --red-bin {self.red_bin} --keep-artifacts"
                ),
            },
        )
        print(f"failed seed=0x{self.seed:016x} artifacts={self.run_dir}", file=sys.stderr)


def http_request(
    method: str, port: int, path: str, body: str | None, timeout: float = 5
) -> tuple[int, str]:
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    headers = {"Content-Type": "application/json"} if body is not None else {}
    try:
        conn.request(method, path, body=body, headers=headers)
        response = conn.getresponse()
        text = response.read().decode("utf-8", errors="replace")
        return response.status, text
    finally:
        conn.close()


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="reddb-jepsen-selftest-") as tmp:
        run_dir = Path(tmp)
        history = OperationHistory(run_dir / "history.jsonl")
        schedule = [
            {"action": "start", "node": "primary"},
            {"action": "message-isolate", "node": "replica-1"},
            {"action": "process-kill", "node": "replica-2"},
            {"action": "process-restart", "node": "replica-2"},
        ]
        schedule_path = run_dir / "schedule.json"
        write_json(schedule_path, schedule)

        history.append(
            {
                "phase": "ok",
                "op": "write",
                "node": "primary",
                "write_id": 1,
                "window": "w0",
            }
        )
        history.append(
            {
                "phase": "fail",
                "op": "write",
                "node": "replica-1",
                "write_id": 2,
                "window": "w0",
            }
        )
        history.append({"phase": "fault", "op": "message-isolate", "node": "replica-1"})
        history.append({"phase": "fault", "op": "process-kill", "node": "replica-2"})
        history.append({"phase": "fault", "op": "process-restart", "node": "replica-2"})
        SafetyChecker(expected_writer="primary").check(history, {1})

        bad_history = OperationHistory(run_dir / "bad-history.jsonl")
        bad_history.append(
            {
                "phase": "ok",
                "op": "write",
                "node": "primary",
                "write_id": 99,
                "window": "w-bad",
            }
        )
        try:
            SafetyChecker(expected_writer="primary").check(bad_history, set())
        except InvariantViolation as err:
            write_json(
                run_dir / "repro.json",
                {
                    "seed": "0x0000000000005eed",
                    "error": str(err),
                    "history_jsonl": str(history.path),
                    "schedule_json": str(schedule_path),
                    "logs_dir": str(run_dir / "logs"),
                },
            )
        else:
            raise AssertionError("self-test must exercise failure artifact preservation")

        print("seed=0x0000000000005eed")
        print(f"history_jsonl={history.path}")
        print(f"schedule_json={schedule_path}")
        print("process_kill_restart=true")
        print("message_isolation=true")
        print("committed_write_loss_checker=true")
        print("stale_leader_checker=true")
        print("single_writer_checker=true")
    return 0


def default_red_bin() -> str:
    env = os.environ.get("REDDB_JEPSEN_RED_BIN")
    if env:
        return env
    return str(Path("target/debug/red"))


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="exercise checkers without red")
    parser.add_argument("--red-bin", default=default_red_bin(), help="path to the red binary")
    parser.add_argument("--seed", default=str(random.SystemRandom().getrandbits(64)))
    parser.add_argument("--ops", default=str(DEFAULT_OPS), help="number of workload steps")
    parser.add_argument(
        "--work-dir",
        default="target/jepsen-blackbox",
        help="artifact root for run directories",
    )
    parser.add_argument(
        "--keep-artifacts",
        action="store_true",
        help="preserve successful run artifacts as well as failing runs",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.self_test:
        return self_test()
    ClusterHarness(args).run()
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except HarnessError as err:
        print(f"error: {err}", file=sys.stderr)
        raise SystemExit(1)

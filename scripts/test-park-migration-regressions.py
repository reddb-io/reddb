#!/usr/bin/env python3
"""Probe a built RedDB binary through HTTP and a real persistent restart."""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def run():
    binary = os.environ.get('REDDB_TEST_BINARY') or shutil.which('red')
    assert binary, 'Set REDDB_TEST_BINARY to the built executable'
    failures = []
    with tempfile.TemporaryDirectory(prefix='reddb-migration-regression-') as tmp:
        root = Path(tmp)
        http, wire = port(), port()
        base = f'http://127.0.0.1:{http}'

        def query(sql):
            req = urllib.request.Request(base + '/query', json.dumps({'query': sql}).encode(),
                                         {'content-type': 'application/json'})
            with urllib.request.urlopen(req, timeout=10) as response:
                return json.load(response)

        def start():
            log = (root / 'engine.log').open('ab')
            process = subprocess.Popen([binary, 'server', '--path', str(root / 'data.rdb'),
                                        '--http', '--http-bind', f'127.0.0.1:{http}',
                                        '--wire-bind', f'127.0.0.1:{wire}', '--no-auth', '--no-log-file'],
                                       stdout=log, stderr=log)
            log.close()
            for _ in range(150):
                if process.poll() is not None:
                    raise RuntimeError('Test engine failed to start')
                try:
                    urllib.request.urlopen(base + '/health/ready', timeout=.2).close()
                    return process
                except (OSError, urllib.error.URLError):
                    time.sleep(.1)
            process.terminate()
            process.wait(timeout=15)
            raise RuntimeError('Test engine did not become ready')

        process = start()
        try:
            query('CREATE TABLE transfer_a (id INTEGER PRIMARY KEY, value TEXT NOT NULL)')
            query('CREATE TABLE transfer_b (id INTEGER PRIMARY KEY, other TEXT NOT NULL)')
            query("INSERT INTO transfer_a (id, value) VALUES (1, 'one')")
            query("INSERT INTO transfer_b (id, other) VALUES (2, 'two')")
            for command in ['DESCRIBE', 'SHOW CREATE TABLE']:
                for table in ['transfer_a', 'transfer_a', 'transfer_b', 'transfer_a']:
                    try:
                        result = query(command + ' ' + table)
                        assert table in json.dumps(result) or command == 'DESCRIBE'
                    except (urllib.error.HTTPError, AssertionError) as error:
                        failures.append({'probe': command, 'table': table, 'error': str(error)})
            before = query("SELECT * FROM red.indices WHERE collection = 'transfer_a'")
            assert 'idx_id' in json.dumps(before), 'Baseline implicit index was not created'
        finally:
            process.terminate()
            process.wait(timeout=20)
        process = start()
        try:
            after = query("SELECT * FROM red.indices WHERE collection = 'transfer_a'")
            if 'idx_id' not in json.dumps(after):
                failures.append({'probe': 'implicit_index_restart', 'error': 'idx_id disappeared'})
            rows = query('SELECT * FROM transfer_a WHERE id = 1')
            assert 'one' in json.dumps(rows), 'Rows were lost after restart'
        finally:
            process.terminate()
            process.wait(timeout=20)
    print(json.dumps({'passed': not failures, 'failures': failures}, indent=2))
    return int(bool(failures))


if __name__ == '__main__':
    raise SystemExit(run())

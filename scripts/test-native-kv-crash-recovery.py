#!/usr/bin/env python3
"""Verify acknowledged native KV recovery through the Rust RedWire client.

Owns fresh synthetic fixtures only. Kill means SIGKILL on Unix and
TerminateProcess on Windows, not a power cut. No checkpoint is requested between
native KV creation/writes and the first two kills. This does not reproduce the
private Hunter fixture from #2536; it covers its public process/transport contract.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.request


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')


class Engine:
    def __init__(self, binary, root):
        self.binary = str(binary)
        self.root = root
        self.process = None
        self.boots = 0
        self.environment = {key: value for key, value in os.environ.items()
                            if not key.startswith(('REDDB_', 'RED_MAX_'))}
        self.environment['REDDB_MEMORY_BUDGET'] = '256MiB'
        with socket.socket() as http, socket.socket() as wire:
            http.bind(('127.0.0.1', 0))
            wire.bind(('127.0.0.1', 0))
            self.http_port = http.getsockname()[1]
            self.wire_port = wire.getsockname()[1]
        self.origin = f'http://127.0.0.1:{self.http_port}'
        self.uri = f'red://127.0.0.1:{self.wire_port}'

    def start(self, preset):
        assert self.process is None, 'Owned writer must be stopped before reopening'
        self.boots += 1
        command = [self.binary, 'server', '--path', str(self.root / 'data.rdb'),
                   '--http', '--http-bind', f'127.0.0.1:{self.http_port}',
                   '--wire-bind', f'127.0.0.1:{self.wire_port}',
                   '--grpc-bind', '127.0.0.1:0', '--workers', '2',
                   '--no-auth', '--no-log-file']
        if preset:
            command.extend(['--storage-preset', preset])
        with (self.root / f'boot-{self.boots}.log').open('wb') as log:
            self.process = subprocess.Popen(command, env=self.environment,
                                            stdout=log, stderr=log)
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError(f'Owned engine exited; inspect boot-{self.boots}.log')
            try:
                with urllib.request.urlopen(self.origin + '/health/ready', timeout=.25) as response:
                    if response.status == 200:
                        return
            except (OSError, urllib.error.URLError):
                time.sleep(.05)
        raise RuntimeError('Owned engine did not become ready within 45 seconds')

    def query(self, sql, expect_failure=False, rows=True):
        # --bind with an explicit red:// URI selects the published Rust driver;
        # `red connect` is a legacy gRPC REPL and would test another transport.
        result = subprocess.run([self.binary, 'query', '--bind', self.uri,
                                 sql, '--format', 'json'], env=self.environment,
                                capture_output=True, text=True, timeout=20)
        if expect_failure:
            assert result.returncode != 0, 'Query succeeded without its owned RedWire server'
            return None
        if result.returncode:
            raise RuntimeError(f'RedWire query failed ({sql[:80]}...): {result.stderr[:800]}')
        if not rows:
            return None
        records = json.loads(result.stdout)
        assert isinstance(records, list), f'Unexpected Rust client rows: {records}'
        return records

    def execute(self, sql):
        self.query(sql, rows=False)

    def checkpoint(self):
        request = urllib.request.Request(self.origin + '/checkpoint', data=b'{}',
                                         headers={'content-type': 'application/json'})
        with urllib.request.urlopen(request, timeout=20) as response:
            assert response.status == 200, 'Checkpoint was not acknowledged'

    def kill(self):
        process, self.process = self.process, None
        assert process is not None and process.poll() is None, 'Owned writer already exited'
        process.kill()
        assert process.wait(timeout=20) != 0, 'Writer must exit uncleanly'

    def cleanup(self):
        if self.process is not None:
            process, self.process = self.process, None
            if process.poll() is None:
                process.kill()
            process.wait(timeout=20)


def verify(engine, expected, padding):
    models = {row['name']: row['model'] for row in engine.query('SHOW COLLECTIONS')}
    assert models['financial_fixture'] == 'table', models
    assert all(models.get(name) == 'table' for name in padding), 'Overflow catalog changed'
    assert engine.query('SELECT id, balance FROM financial_fixture ORDER BY id') == [
        {'id': 1, 'balance': 1100}, {'id': 2, 'balance': -1100}], 'Acknowledged table changed'
    for name, values in expected.items():
        assert models[name] == 'kv', f'{name}: model changed: {models}'
        for key, value in values.items():
            rows = engine.query(f'KV GET {name}.{key}')
            assert len(rows) == 1 and rows[0]['kind'] == 'kv', f'{name}.{key}: missing KV row'
            actual = rows[0]['value']
            if isinstance(value, dict):
                # The Rust row formatter exposes native JSON as JSON text.
                assert isinstance(actual, str), 'Native JSON row representation changed'
                actual = json.loads(actual)
            assert actual == value, f'{name}.{key}: acknowledged value changed'


def run_case(binary, output, name, preset, existing_paged, catalog_entries=0):
    root = output / name
    root.mkdir()
    engine = Engine(binary, root)
    receipt = {'scenario': name, 'transport': 'RedWire', 'passed': False,
               'explicitCheckpointsBetweenKvInitializationAndKill': 0, 'kills': 0, 'verifiedReopens': 0}
    expected = {}
    padding = [f'catalog_padding_{index:03d}_' + 'p' * 40 for index in range(catalog_entries)]
    receipt['catalogPaddingCollections'] = len(padding)
    try:
        engine.start('serverless' if existing_paged else preset)
        engine.execute('CREATE TABLE financial_fixture (id INTEGER PRIMARY KEY, balance INTEGER)')
        engine.execute('INSERT INTO financial_fixture (id,balance) VALUES (1,1100),(2,-1100)')
        for collection in padding:
            engine.execute('CREATE TABLE ' + collection + ' (id INTEGER PRIMARY KEY, note TEXT)')
        if padding:
            assert sum(len(collection.encode()) for collection in padding) > 16_384, 'Catalog did not exceed one page'
        if existing_paged or padding:
            # Prepare an existing paged/overflow catalog before native KV DDL.
            # No checkpoint follows that later KV initialization before its kills.
            engine.checkpoint()
            engine.kill()
            receipt['kills'] += 1
            engine.start(preset)
        for cycle in range(2):
            collection = f'native_accounts_{cycle}'
            engine.execute('CREATE KV ' + collection)
            expected[collection] = {}
            for key, value in [('funded', 1100), ('sequence', cycle + 3),
                               ('payload', {'cycle': cycle, 'note': 'synthetic', 'nested': [1, 2, 3]}),
                               ('overflow', ''.join(hashlib.sha256(f'{cycle}:{index}'.encode()).hexdigest()
                                                    for index in range(192)))]:
                literal = "'" + value.replace("'", "''") + "'" if isinstance(value, str) else json.dumps(value)
                engine.execute(f'KV PUT {collection}.{key} = {literal}')
                expected[collection][key] = value
            if cycle:
                engine.execute('KV PUT native_accounts_0.sequence = 4')
                expected['native_accounts_0']['sequence'] = 4
            engine.kill()  # Kill immediately after the last write acknowledgement.
            receipt['kills'] += 1
            # Negative control: never fall back to an embedded/in-memory client.
            engine.query('KV GET native_accounts_0.funded', expect_failure=True)
            engine.start(preset)
            verify(engine, expected, padding)
            receipt['verifiedReopens'] += 1
        # Re-check the same models and values after an explicit checkpoint too.
        engine.checkpoint()
        engine.kill()
        receipt['kills'] += 1
        engine.start(preset)
        verify(engine, expected, padding)
        receipt['verifiedReopens'] += 1
        receipt.update(passed=True, collections=len(expected),
                       values=sum(len(values) for values in expected.values()))
    except Exception as error:
        receipt['error'] = str(error)
        raise
    finally:
        engine.cleanup()
        write_json(root / 'receipt.json', receipt)
        print(json.dumps(receipt), flush=True)
    return receipt


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default=os.environ.get('REDDB_TEST_BINARY') or shutil.which('red'))
    parser.add_argument('--output', type=Path, required=True,
                        help='New owned directory; existing paths are rejected and fixtures are retained')
    args = parser.parse_args()
    if not args.binary:
        parser.error('Set --binary or REDDB_TEST_BINARY to the built executable')
    binary = Path(args.binary).resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    with binary.open('rb') as artifact:
        digest = hashlib.file_digest(artifact, 'sha256').hexdigest()
    receipt = {'binarySha256': digest, 'passed': False, 'cases': []}
    try:
        for name, preset, existing, catalog in [('default', None, False, 0),
                                                ('serverless', 'serverless', False, 0),
                                                ('default-on-paged', None, True, 0),
                                                ('catalog-overflow', 'serverless', False, 320)]:
            receipt['cases'].append(run_case(binary, args.output, name, preset, existing, catalog))
        receipt['passed'] = True
    finally:
        write_json(args.output / 'receipt.json', receipt)
    return 0


if __name__ == '__main__':
    raise SystemExit(run())

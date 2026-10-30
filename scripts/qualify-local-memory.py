#!/usr/bin/env python3
"""Qualify a supplied RedDB artifact in a private, bounded Linux systemd service.

Uses synthetic cloud-vault data and loopback listeners. Never uses an existing
store, changes host limits, or deletes evidence. The client/sampler stays outside
the engine cgroup. Exit nonzero on budget, integrity, OOM, or storage regressions.
"""
import argparse
import base64
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import random
import secrets
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid

MIB = 1024**2


def command(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=45).stdout.strip()


def rows(response):
    result = response.get('result', {})
    return [row['values'] for row in result['records']] if 'records' in result else result.get('rows', [])


def storage(directory):
    allocated = logical = 0
    for path in directory.rglob('*'):
        try:
            if path.is_file():
                stat = path.stat()
                allocated += stat.st_blocks * 512
                logical += stat.st_size
        except FileNotFoundError:
            pass  # A background checkpoint may retire a file during observation.
    components = {}
    for name, label in [('data.rdb', 'mainBytes'), ('data.rdb-uwal', 'walBytes')]:
        try:
            components[label] = (directory / name).stat().st_size
        except FileNotFoundError:
            components[label] = 0
    return {'allocatedBytes': allocated, 'logicalBytes': logical, **components}


class Engine:
    def __init__(self, args):
        self.args = args
        self.directory = args.output
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=False)
        self.data = self.directory / 'data'
        self.data.mkdir(mode=0o700)
        for name in ['head', 'customer']:
            (self.directory / (name + '.password')).write_text(secrets.token_urlsafe(32))
        self.unit = None
        self.token = None
        self.sampling_stop = threading.Event()
        self.failure = None
        self.samples = []
        self.events = []
        self.boot = 0
        self.latencies = []

    def log(self, phase, **data):
        event = {'phase': phase, 'at': time.time(), **data}
        self.events.append(event)
        with (self.directory / 'events.jsonl').open('a') as output:
            output.write(json.dumps(event) + '\n')
        print(json.dumps(event), flush=True)

    def request(self, path, body):
        if self.failure:
            raise RuntimeError(self.failure)
        headers = {'Content-Type': 'application/json'}
        if self.token:
            headers['Authorization'] = 'Bearer ' + self.token
        request = urllib.request.Request(self.url + path, json.dumps(body).encode(), headers)
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise RuntimeError(f'HTTP {error.code}: {error.read(2048).decode()}') from error

    def query(self, query, params=None):
        started = time.monotonic()
        response = self.request('/query', {'query': query, 'params': params or []})
        self.latencies.append(time.monotonic() - started)
        if response.get('ok') is not True:
            raise RuntimeError(str(response.get('error', response))[:1000])
        return rows(response)

    def sample(self):
        while not self.sampling_stop.is_set():
            try:
                sample = {'at': time.time(), 'boot': self.boot}
                for name in ['memory.current', 'memory.peak', 'memory.events']:
                    sample[name] = (self.cgroup / name).read_text().strip()
                sample.update(storage(self.data))
                self.samples.append(sample)
                with (self.directory / 'samples.jsonl').open('a') as output:
                    output.write(json.dumps(sample) + '\n')
                if sample['allocatedBytes'] > self.args.disk_limit_mib * MIB:
                    self.failure = 'Allocated storage exceeded the external test guard'
                    command('systemctl', '--user', 'kill', '--signal=SIGKILL', self.unit)
                    return
                if any(int(line.split()[1]) for line in sample['memory.events'].splitlines()
                       if line.split()[0] in ['oom', 'oom_kill']):
                    self.failure = 'Engine cgroup reported OOM'
                    return
            except FileNotFoundError:
                return  # Unit exit; systemd Result/MemoryPeak is captured separately.
            except Exception as error:
                self.failure = 'Sampler failed: ' + str(error)
                return
            self.sampling_stop.wait(.2)

    def start(self):
        self.boot += 1
        self.unit = 'reddb-qualify-' + uuid.uuid4().hex[:12] + '.service'
        reservations = [socket.socket() for _ in range(3)]
        for reservation in reservations:
            reservation.bind(('127.0.0.1', 0))
        ports = [reservation.getsockname()[1] for reservation in reservations]
        for reservation in reservations:
            reservation.close()
        self.url = f'http://127.0.0.1:{ports[0]}'
        self.token = None
        log = self.directory / f'engine-{self.boot}.log'
        args = ['systemd-run', '--user', '--unit=' + self.unit,
                f'--property=MemoryMax={self.args.memory_mib}M', '--property=MemorySwapMax=0',
                f'--property=CPUQuota={self.args.cpu_percent}%', '--property=RuntimeMaxSec=1800',
                '--property=TimeoutStopSec=30', '--property=LimitCORE=0',
                '--property=StandardOutput=append:' + str(log),
                '--property=StandardError=append:' + str(log),
                '--working-directory=' + str(self.directory), '/usr/bin/env', '-i',
                'HOME=' + str(self.directory), 'PATH=/usr/bin:/bin']
        if self.args.budget_mib:
            args.append(f'REDDB_MEMORY_BUDGET={self.args.budget_mib}MiB')
        if self.boot > 1:
            args.append('REDDB_CERTIFICATE_FILE=' + str(self.directory / 'cert.pem'))
        args += [str(self.args.binary), 'server', '--path', str(self.data / 'data.rdb'),
                 '--vault', 'true', '--http', '--http-bind', f'127.0.0.1:{ports[0]}',
                 '--wire-bind', f'127.0.0.1:{ports[1]}', '--grpc-bind', f'127.0.0.1:{ports[2]}',
                 '--bootstrap-preset', 'cloud', '--cloud-head-admin', 'red_admin',
                 '--cloud-head-admin-password-file', str(self.directory / 'head.password'),
                 '--customer-admin', 'admin', '--customer-admin-password-file',
                 str(self.directory / 'customer.password'), '--bootstrap-cert-out',
                 str(self.directory / 'cert.pem'), '--no-log-file']
        if self.boot > 1:
            args.append('--no-create-if-missing')
        started = time.monotonic()
        command(*args)
        relative = command('systemctl', '--user', 'show', self.unit, '--property=ControlGroup', '--value')
        assert relative.startswith('/') and relative.endswith(self.unit)
        self.cgroup = Path('/sys/fs/cgroup' + relative)
        assert int((self.cgroup / 'memory.max').read_text()) == self.args.memory_mib * MIB
        assert (self.cgroup / 'memory.swap.max').read_text().strip() == '0'
        cpu = (self.cgroup / 'cpu.max').read_text().split()
        assert int(cpu[0]) / int(cpu[1]) <= self.args.cpu_percent / 100
        self.sampling_stop.clear()
        self.sampler = threading.Thread(target=self.sample, daemon=True)
        self.sampler.start()
        last_error = None
        while time.monotonic() - started < 120:
            try:
                self.token = self.request('/auth/login', {
                    'username': 'red_admin',
                    'password': (self.directory / 'head.password').read_text(),
                })['token']
                break
            except Exception as error:
                last_error = error
                state = command('systemctl', '--user', 'show', self.unit, '--property=ActiveState', '--value')
                if state in ['failed', 'inactive']:
                    raise RuntimeError(f'Engine exited before login: {error}') from error
                time.sleep(.2)
        else:
            raise RuntimeError(f'Readiness timeout: {last_error}')
        budget = self.query("SELECT * FROM red.stats WHERE collection='red.memory_budget'")
        metrics = {row['metric']: row['value'] for row in budget if row.get('entity') is None}
        self.log('ready', boot=self.boot, seconds=round(time.monotonic() - started, 3),
                 budget=metrics, memoryMax=self.args.memory_mib * MIB, cgroup=relative)
        expected = (self.args.budget_mib or self.args.memory_mib) * MIB
        assert metrics['resolved_bytes'] == expected, ('wrong detected budget', metrics, expected)
        assert metrics['source'] == ('config' if self.args.budget_mib else 'cgroup-v2')

    def stop(self, crash=False):
        if not self.unit:
            return
        try:
            if crash:
                command('systemctl', '--user', 'kill', '--kill-whom=main', '--signal=SIGKILL', self.unit)
                for _ in range(100):
                    state = command('systemctl', '--user', 'show', self.unit, '--property=ActiveState', '--value')
                    if state in ['failed', 'inactive']:
                        break
                    time.sleep(.05)
            else:
                command('systemctl', '--user', 'stop', self.unit)
            status = command('systemctl', '--user', 'show', self.unit,
                             '--property=Result,ExecMainCode,ExecMainStatus,MemoryPeak')
            self.log('stopped', boot=self.boot, intentionalCrash=crash, status=status)
            if 'Result=oom-kill' in status:
                raise RuntimeError('Engine was OOM-killed')
            if crash:
                assert 'ExecMainStatus=9' in status, status
            else:
                assert 'Result=success' in status, status
        finally:
            self.sampling_stop.set()
            if hasattr(self, 'sampler'):
                self.sampler.join(timeout=3)
            self.unit = None


def payload(table, identity, revision):
    if table == 'compressed':
        return f'{identity}:{revision}:' + 'x' * (24 * 1024)
    size = 240 if table == 'inline_rows' else 24 * 1024
    return base64.b64encode(random.Random(f'{table}:{identity}:{revision}').randbytes(size)).decode()[:size]


def insert(engine, table, records):
    items = list(records.items())
    for offset in range(0, len(items), 16):
        params, placeholders = [], []
        for identity, row in items[offset:offset + 16]:
            start = len(params) + 1
            params.extend([identity, *row])
            placeholders.append('(' + ','.join(f'${index}' for index in range(start, start + 4)) + ')')
        engine.query(f'INSERT INTO {table} (id,group_id,payload,revision) VALUES ' + ','.join(placeholders), params)


def verify(engine, expected, counter, documents):
    checked = 0
    for table, records in expected.items():
        observed = {}
        for group in range(4):
            for row in engine.query(f'SELECT id,group_id,payload,revision FROM {table} WHERE group_id=$1', [group]):
                identity = int(row['id'])
                assert identity not in observed, ('duplicate', table, identity)
                observed[identity] = [row['group_id'], row['payload'], row['revision']]
        assert observed == records, ('payload mismatch', table, len(observed), len(records))
        assert engine.query(f'SELECT COUNT(*) AS n FROM {table}')[0]['n'] == len(records)
        checked += len(records)
    assert engine.query('KV GET qualification_kv.counter')[0]['value'] == counter
    actual = engine.query('SELECT * FROM qualification_documents')
    assert sorted(row['payload'] for row in actual) == sorted(documents), 'document payload mismatch'
    return {'checkedRows': checked, 'checkedDocuments': len(documents), 'counter': counter}


def qualify(engine):
    engine.start()
    if engine.args.probe_only:
        return
    expected = {}
    for table, count in [('inline_rows', 512), ('overflow', 256), ('compressed', 256)]:
        engine.query(f'CREATE TABLE {table} (id INTEGER PRIMARY KEY,group_id INTEGER,payload TEXT,revision INTEGER)')
        engine.query(f'CREATE INDEX {table}_group ON {table}(group_id)')
        expected[table] = {identity: [identity % 4, payload(table, identity, 0), 0]
                           for identity in range(1, count + 1)}
        insert(engine, table, expected[table])
    engine.query('CREATE KV qualification_kv')
    engine.query('KV PUT qualification_kv.counter = 0')
    engine.query('CREATE DOCUMENT qualification_documents')
    documents = [base64.b64encode(random.Random(index).randbytes(192 * 1024)).decode() for index in range(32)]
    for index, document in enumerate(documents):
        engine.query('INSERT INTO qualification_documents DOCUMENT VALUES (' + json.dumps({'id': index, 'payload': document}) + ')')
    engine.request('/checkpoint', {})
    engine.log('seeded', **verify(engine, expected, 0, documents), **storage(engine.data))
    warm = None
    for cycle in range(1, engine.args.cycles + 1):
        started = time.monotonic()
        for table, records in expected.items():
            updated, deleted = cycle % 4, (cycle + 1) % 4
            text = payload(table, 0, cycle)
            engine.query(f'UPDATE {table} SET payload=$1,revision=$2 WHERE group_id=$3', [text, cycle, updated])
            for identity, row in records.items():
                if row[0] == updated:
                    records[identity] = [updated, text, cycle]
            engine.query(f'DELETE FROM {table} WHERE group_id=$1', [deleted])
            replacements = {identity: [deleted, payload(table, identity, cycle), cycle]
                            for identity, row in records.items() if row[0] == deleted}
            records.update(replacements)
            insert(engine, table, replacements)
            if cycle % 4 == 0:
                engine.query(f'DROP INDEX {table}_group ON {table}')
                engine.query(f'CREATE INDEX {table}_group ON {table}(group_id)')
        engine.query(f'KV PUT qualification_kv.counter = {cycle}')
        if engine.args.drain_seconds:
            time.sleep(engine.args.drain_seconds)
        if engine.args.vacuum:
            engine.log('vacuum', cycle=cycle, result=engine.query('VACUUM'))
        checkpoint = time.monotonic()
        engine.request('/checkpoint', {})
        checkpoint = time.monotonic() - checkpoint
        # Bursts can temporarily create MVCC history faster than paced GC.
        # Compare equivalent, drained generations; keep the burst observations.
        if cycle % 4 == 0 and engine.args.settle_seconds:
            engine.log('before-settle', cycle=cycle, **storage(engine.data))
            time.sleep(engine.args.settle_seconds)
            for _ in range(2):
                engine.request('/checkpoint', {})
        proof = verify(engine, expected, cycle, documents)
        space = storage(engine.data)
        engine.log('cycle', cycle=cycle, seconds=round(time.monotonic() - started, 3),
                   checkpointSeconds=round(checkpoint, 3), **proof, **space)
        if cycle == 4:
            warm = space['allocatedBytes']
        if warm is not None and (cycle % 4 == 0 or not engine.args.settle_seconds):
            assert space['allocatedBytes'] <= warm + 16 * MIB, ('storage failed to plateau', warm, space)
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as workers:
        tasks = [workers.submit(engine.query, 'SELECT id,payload FROM overflow WHERE group_id=$1', [index % 4])
                 for index in range(24)]
        for task in tasks:
            assert len(task.result()) == 64
    engine.log('concurrent-reads', queries=24, concurrency=4)
    counter = engine.args.cycles
    crash_baseline = None
    for crash in range(engine.args.crashes):
        counter += 1
        engine.query(f'KV PUT qualification_kv.counter = {counter}')
        for table, records in expected.items():
            text = payload(table, 1, counter)
            engine.query(f'UPDATE {table} SET payload=$1,revision=$2 WHERE id=1', [text, counter])
            records[1] = [1, text, counter]
        (engine.directory / 'acknowledged.json').write_text(json.dumps({'tables': expected, 'counter': counter}))
        engine.log('acknowledged-before-kill', crash=crash, counter=counter)
        engine.stop(crash=True)
        engine.start()
        space = storage(engine.data)
        engine.log('crash-recovery', crash=crash, **verify(engine, expected, counter, documents), **space)
        if crash == 3:
            crash_baseline = space['allocatedBytes']
        if crash_baseline is not None:
            assert space['allocatedBytes'] <= crash_baseline + 16 * MIB, ('repeated recovery growth', space)
    engine.stop()
    engine.start()
    engine.log('clean-reopen', **verify(engine, expected, counter, documents), **storage(engine.data))
    for _ in range(5):
        engine.request('/checkpoint', {})
    engine.log('idle-checkpoints', **storage(engine.data))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True, help='New private evidence directory; never reused')
    parser.add_argument('--memory-mib', type=int, choices=[256, 512, 1024], default=512)
    parser.add_argument('--budget-mib', type=int, help='Explicit budget control; omit to test auto-detection')
    parser.add_argument('--cpu-percent', type=int, choices=[25, 100], default=100)
    parser.add_argument('--disk-limit-mib', type=int, default=2048)
    parser.add_argument('--cycles', type=int, default=12)
    parser.add_argument('--crashes', type=int, default=8)
    parser.add_argument('--probe-only', action='store_true')
    parser.add_argument('--settle-seconds', type=float, default=10,
                        help='Drain after each four-cycle burst before comparing equivalent generations; 0 tests sustained input')
    parser.add_argument('--drain-seconds', type=float, default=0,
                        help='Diagnostic control: allow background reclamation between cycles')
    parser.add_argument('--vacuum', action='store_true',
                        help='Diagnostic control: explicit VACUUM before checkpoint')
    args = parser.parse_args()
    args.binary = args.binary.resolve(strict=True)
    args.output = args.output.resolve()
    assert args.cycles >= 4
    assert args.crashes >= 1
    assert args.drain_seconds >= 0 and args.settle_seconds >= 0
    assert not args.budget_mib or 0 < args.budget_mib <= args.memory_mib
    os.umask(0o077)
    engine = Engine(args)
    report = {'binary': str(args.binary), 'sha256': hashlib.file_digest(args.binary.open('rb'), 'sha256').hexdigest(),
              'memoryMiB': args.memory_mib, 'budgetMiB': args.budget_mib, 'cpuPercent': args.cpu_percent,
              'drainSeconds': args.drain_seconds, 'settleSeconds': args.settle_seconds,
              'explicitVacuum': args.vacuum, 'cycles': args.cycles, 'crashes': args.crashes, 'passed': False}
    try:
        qualify(engine)
        engine.stop()
        if engine.failure:
            raise RuntimeError(engine.failure)
        report['passed'] = True
    except Exception as error:
        report['error'] = repr(error)
        try:
            engine.stop()
        except Exception as stop_error:
            report['stopError'] = repr(stop_error)
    finally:
        report['events'] = engine.events
        report['peakBytes'] = max((int(row['memory.peak']) for row in engine.samples), default=0)
        report['storage'] = storage(engine.data)
        (engine.directory / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({key: value for key, value in report.items() if key != 'events'}), flush=True)
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())

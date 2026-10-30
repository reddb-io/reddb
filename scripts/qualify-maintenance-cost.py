#!/usr/bin/env python3
"""Compare paced MVCC reclamation under identical sustained, bounded workloads.

Each ABBA case starts a fresh synthetic store using qualify-local-memory's
external cgroup sampler and artifact launcher. No credentials or existing
database are accepted. Latencies include scheduling delay (no coordinated
omission); results are diagnostic, not a universal performance guarantee.
"""
import argparse
import concurrent.futures
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import statistics
import sys
import threading
import time

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    'qualification', Path(__file__).with_name('qualify-local-memory.py'))
qualification = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualification)


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]


def counters(engine):
    cpu = {key: int(value) for key, value in
           (line.split() for line in (engine.cgroup / 'cpu.stat').read_text().splitlines())}
    io_path = engine.cgroup / 'io.stat'
    if not io_path.exists():
        return {'cpu': cpu, 'io': None}  # Controller may not be delegated to user services.
    io = {}
    for line in io_path.read_text().splitlines():
        for field in line.split()[1:]:
            key, value = field.split('=')
            io[key] = io.get(key, 0) + int(value)
    return {'cpu': cpu, 'io': io}


def run_case(args, enabled, ordinal):
    case_args = argparse.Namespace(**vars(args))
    case_args.output = args.output / f'{ordinal}-{"on" if enabled else "off"}'
    case_args.budget_mib = None
    engine = qualification.Engine(case_args)
    result = {'enabled': enabled, 'passed': False}
    try:
        engine.start()
        engine.query(f'SET CONFIG runtime.mvcc.autovacuum.enabled = {str(enabled).lower()}')
        engine.query('SET CONFIG runtime.result_cache.enabled = false')
        engine.query('CREATE TABLE cost_rows (id INTEGER PRIMARY KEY, payload TEXT, revision INTEGER)')
        payloads = [qualification.payload('overflow', i, 0) for i in range(args.workers)]
        for identity in range(args.rows):
            engine.query('INSERT INTO cost_rows (id,payload,revision) VALUES ($1,$2,0)',
                         [identity, payloads[identity % args.workers]])
        engine.request('/checkpoint', {})
        engine.log('seeded', **qualification.storage(engine.data))
        barrier = threading.Barrier(args.workers)
        observations = []

        def worker(identity):
            barrier.wait(timeout=30)
            start = time.monotonic()
            samples = []
            for step in range(1, args.steps + 1):
                due = start + (step - 1) / args.steps_per_second
                time.sleep(max(0, due - time.monotonic()))
                lag = max(0, time.monotonic() - due)
                # Independent writers: every read verifies its acknowledged
                # version and exact overflow payload despite concurrent GC.
                before = time.monotonic()
                engine.query('UPDATE cost_rows SET revision=$1 WHERE id=$2', [step, identity])
                write_done = time.monotonic()
                rows = engine.query('SELECT payload,revision FROM cost_rows WHERE id=$1', [identity])
                done = time.monotonic()
                assert rows == [{'payload': payloads[identity], 'revision': step}], 'read-after-write mismatch'
                samples.append({'lag': lag, 'write': write_done - before,
                                'read': done - write_done, 'scheduledPair': done - due})
                if step % 100 == 0:
                    engine.log('progress', worker=identity, step=step,
                               **qualification.storage(engine.data))
            return samples

        before_counters = counters(engine)
        started = time.monotonic()
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
            for samples in pool.map(worker, range(args.workers)):
                observations.extend(samples)
        elapsed = time.monotonic() - started
        after_counters = counters(engine)
        result['cgroupDelta'] = {
            group: None if fields is None else {key: value - before_counters[group].get(key, 0)
                    for key, value in fields.items()}
            for group, fields in after_counters.items()}
        result['seconds'] = elapsed
        result['requests'] = len(observations) * 2
        result['requestsPerSecond'] = result['requests'] / elapsed
        result['latencyMs'] = {
            metric: {name: percentile([r[metric] * 1000 for r in observations], quantile)
                     for name, quantile in [('p50', .5), ('p95', .95), ('p99', .99), ('max', 1)]}
            for metric in ['write', 'read', 'scheduledPair', 'lag']}
        result['loadedStorage'] = qualification.storage(engine.data)
        assert engine.query('SELECT COUNT(*) AS n FROM cost_rows')[0]['n'] == args.rows
        engine.request('/checkpoint', {})
        result['checkpointStorage'] = qualification.storage(engine.data)
        engine.stop(crash=True)
        engine.start()
        for identity in range(args.workers):
            assert engine.query('SELECT payload,revision FROM cost_rows WHERE id=$1', [identity]) == [
                {'payload': payloads[identity], 'revision': args.steps}]
        assert engine.query('SELECT COUNT(*) AS n FROM cost_rows')[0]['n'] == args.rows
        result['recoveryVerified'] = True
        engine.stop()
        if engine.failure:
            raise RuntimeError(engine.failure)
        result['passed'] = True
    except Exception as error:
        result['error'] = repr(error)
        try:
            engine.stop()
        except Exception as stop_error:
            result['stopError'] = repr(stop_error)
    result['peakBytes'] = max((int(sample['memory.peak']) for sample in engine.samples), default=0)
    (engine.directory / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result), flush=True)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--memory-mib', type=int, choices=[512, 1024], default=512)
    parser.add_argument('--cpu-percent', type=int, choices=[25, 100], default=100)
    parser.add_argument('--disk-limit-mib', type=int, default=1024)
    parser.add_argument('--workers', type=int, default=4)
    parser.add_argument('--rows', type=int, default=256)
    parser.add_argument('--steps', type=int, default=1000)
    parser.add_argument('--steps-per-second', type=float, default=20)
    args = parser.parse_args()
    assert 1 <= args.workers <= 8 and args.workers <= args.rows <= 1024
    assert 100 <= args.steps <= 4000 and 0 < args.steps_per_second <= 100
    args.binary = args.binary.resolve(strict=True)
    args.output = args.output.resolve()
    os.umask(0o077)
    args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    report = {'sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              'parameters': {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
              'cases': []}
    for ordinal, enabled in enumerate([False, True, True, False], 1):
        result = run_case(args, enabled, ordinal)
        report['cases'].append(result)
        (args.output / 'summary.json').write_text(json.dumps(report, indent=2) + '\n')
        if not result['passed']:
            return 1
    report['comparison'] = {}
    for enabled in [False, True]:
        cases = [case for case in report['cases'] if case['enabled'] == enabled]
        report['comparison']['on' if enabled else 'off'] = {
            'medianRequestsPerSecond': statistics.median(c['requestsPerSecond'] for c in cases),
            'medianWriteP99Ms': statistics.median(c['latencyMs']['write']['p99'] for c in cases),
            'medianScheduledPairP99Ms': statistics.median(c['latencyMs']['scheduledPair']['p99'] for c in cases),
            'maxPeakBytes': max(c['peakBytes'] for c in cases)}
    (args.output / 'summary.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report['comparison']), flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

#!/usr/bin/env python3
"""
Fast smoke benchmark — RedDB vs Postgres, 100K rows.
Target: 5-8 min total. Used to validate perf changes without waiting 40min.

- N = 100_000 (10× less than definitive)
- Select iterations = 50 (vs 200)
- Point lookups = 500 (vs 1000)
- Per-scenario timeout = 30s (skip if query stream hangs)
- No reboot, RAM, or disk measurements — focused on ops/s only
- Single dataset (users table, standard types)
"""
import json, time, random, subprocess, os, sys

N = 100_000
CHUNK = 25_000
SELECT_ITERS = 50
POINT_ITERS = 500
SCENARIO_TIMEOUT = 30.0
random.seed(42)
CITIES = ["NYC","London","Tokyo","Paris","Berlin","Sydney","Toronto","Dubai","Singapore","Mumbai"]
REDDB = "/home/cyber/Work/FF/reddb/target/release/red"
VENV = "/home/cyber/Work/FF/reddb/drivers/python/.venv/lib/python3.12/site-packages"
if VENV not in sys.path:
    sys.path.insert(0, VENV)


def bench_capped(fn, timeout=SCENARIO_TIMEOUT):
    """Run fn while capping wall time. Returns (ms, iterations_actually_done)
    so the caller can compute ops/s honestly even when we aborted early."""
    t0 = time.perf_counter()
    done = fn(timeout)
    ms = (time.perf_counter() - t0) * 1000
    return ms, done


def run_reddb():
    print("\n=== REDDB ===")
    r = {}

    subprocess.run(["fuser","-k","19051/tcp","19052/tcp"], capture_output=True)
    time.sleep(1)
    subprocess.run(["rm","-rf","/tmp/br"])
    os.makedirs("/tmp/br", exist_ok=True)

    t0 = time.perf_counter()
    proc = subprocess.Popen(
        [REDDB,"server","--path","/tmp/br/d.rdb",
         "--grpc-bind","127.0.0.1:19051","--wire-bind","127.0.0.1:19052"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    import reddb_python
    for _ in range(100):
        try:
            c = reddb_python.wire_connect("127.0.0.1:19052"); c.close(); break
        except Exception:
            time.sleep(0.05)
    r["boot_ms"] = round((time.perf_counter()-t0)*1000)
    print(f"  boot:            {r['boot_ms']:>6} ms")

    conn = reddb_python.connect("127.0.0.1:19051")
    wc = reddb_python.wire_connect("127.0.0.1:19052")

    # Seed
    dataset = [
        {"id": i+1, "name": f"U_{i}", "email": f"u{i}@t.com",
         "age": random.randint(18, 80), "city": random.choice(CITIES),
         "score": round(random.uniform(0, 100), 2), "active": i % 2 == 0}
        for i in range(N)
    ]
    t0 = time.perf_counter()
    for s in range(0, N, CHUNK):
        e = min(s+CHUNK, N)
        pj = [json.dumps({"fields": rec}) for rec in dataset[s:e]]
        conn.bulk_insert("users", pj)
    ins_ms = (time.perf_counter()-t0)*1000
    r["insert"] = int(N/ins_ms*1000)
    print(f"  insert:          {r['insert']:>6} ops/sec ({ins_ms:.0f}ms)")

    conn.execute("CREATE INDEX idx_city ON users (city) USING HASH")
    conn.execute("CREATE INDEX idx_age ON users (age) USING BTREE")

    def run_select(name, sqls, iters):
        def capped(timeout):
            deadline = time.perf_counter() + timeout
            i = 0
            for sql in sqls:
                if time.perf_counter() > deadline or i >= iters:
                    break
                wc.query_raw(sql); i += 1
            return i
        ms, done = bench_capped(capped)
        ops = int(done/ms*1000) if ms > 0 and done > 0 else 0
        note = "" if done == iters else f" [aborted at {done}/{iters}]"
        print(f"  {name:<18}{ops:>6} ops/sec{note}")
        return ops

    # No-filter LIMIT (alpha blocker: used to hang at 0 qps — needs
    # LIMIT pushdown into the table scan).
    r["select_no_filter"] = run_select(
        "select_no_filter:", ["SELECT * FROM users LIMIT 100"] * SELECT_ITERS, SELECT_ITERS)

    # Point lookups
    lids = [random.randint(1, N) for _ in range(POINT_ITERS)]
    r["select_point"] = run_select(
        "select_point:", [f"SELECT * FROM users WHERE _entity_id = {i}" for i in lids], POINT_ITERS)

    # Range
    rqs = [(random.randint(18, 70), random.randint(18, 70)+10) for _ in range(SELECT_ITERS)]
    r["select_range"] = run_select(
        "select_range:", [f"SELECT * FROM users WHERE age BETWEEN {l} AND {h} LIMIT 100" for l, h in rqs],
        SELECT_ITERS)

    # Filtered
    fqs = [(random.choice(CITIES), random.randint(18, 60)) for _ in range(SELECT_ITERS)]
    r["select_filtered"] = run_select(
        "select_filtered:", [f"SELECT * FROM users WHERE city = '{c}' AND age > {a} LIMIT 100" for c, a in fqs],
        SELECT_ITERS)

    # Aggregates
    r["agg_count"] = run_select("agg_count:", ["SELECT COUNT(*) FROM users"]*SELECT_ITERS, SELECT_ITERS)
    r["agg_groupby"] = run_select(
        "agg_groupby:", ["SELECT city, COUNT(*) FROM users GROUP BY city"]*SELECT_ITERS, SELECT_ITERS)

    # Update single — 100 iterations (was 1000 in definitive)
    uids = [random.randint(1, N) for _ in range(100)]
    r["update_single"] = run_select(
        "update_single:", [f"UPDATE users SET score = 99 WHERE _entity_id = {u}" for u in uids], 100)

    proc.terminate(); proc.wait()
    return r


def run_pg():
    print("\n=== POSTGRES ===")
    import psycopg2
    from psycopg2.extras import execute_values

    subprocess.run(["docker","rm","-f","pg-bench"], capture_output=True)
    subprocess.run(["docker","run","-d","--name","pg-bench",
                    "-e","POSTGRES_PASSWORD=bench",
                    "-p","5432:5432","postgres:16-alpine"],
                   capture_output=True)

    r = {}
    t0 = time.perf_counter()
    # Retry until PG accepts connections (up to 15s).
    conn = None
    for _ in range(150):
        try:
            conn = psycopg2.connect(host="127.0.0.1", dbname="postgres",
                                    user="postgres", password="bench")
            break
        except Exception:
            time.sleep(0.1)
    if conn is None:
        raise RuntimeError("pg-bench failed to accept connections within 15s")
    conn.autocommit = True
    r["boot_ms"] = round((time.perf_counter()-t0)*1000)
    print(f"  boot:            {r['boot_ms']:>6} ms")
    cur = conn.cursor()

    cur.execute("CREATE TABLE users (id INT PRIMARY KEY, name TEXT, email TEXT, age INT, "
                "city TEXT, score FLOAT, active BOOLEAN)")

    dataset = [
        (i+1, f"U_{i}", f"u{i}@t.com", random.randint(18, 80),
         random.choice(CITIES), round(random.uniform(0, 100), 2), i % 2 == 0)
        for i in range(N)
    ]
    t0 = time.perf_counter()
    for s in range(0, N, CHUNK):
        e = min(s+CHUNK, N)
        execute_values(cur, "INSERT INTO users (id, name, email, age, city, score, active) VALUES %s",
                       dataset[s:e])
    ins_ms = (time.perf_counter()-t0)*1000
    r["insert"] = int(N/ins_ms*1000)
    print(f"  insert:          {r['insert']:>6} ops/sec ({ins_ms:.0f}ms)")

    cur.execute("CREATE INDEX idx_age ON users(age)")
    cur.execute("CREATE INDEX idx_city_age ON users(city, age)")

    def run_select(name, sqls, iters):
        def capped(timeout):
            deadline = time.perf_counter() + timeout
            i = 0
            for sql in sqls:
                if time.perf_counter() > deadline or i >= iters:
                    break
                cur.execute(sql)
                if cur.description is not None:
                    cur.fetchall()
                i += 1
            return i
        ms, done = bench_capped(capped)
        ops = int(done/ms*1000) if ms > 0 and done > 0 else 0
        note = "" if done == iters else f" [aborted at {done}/{iters}]"
        print(f"  {name:<18}{ops:>6} ops/sec{note}")
        return ops

    r["select_no_filter"] = run_select(
        "select_no_filter:", ["SELECT * FROM users LIMIT 100"] * SELECT_ITERS, SELECT_ITERS)

    lids = [random.randint(1, N) for _ in range(POINT_ITERS)]
    r["select_point"] = run_select(
        "select_point:", [f"SELECT * FROM users WHERE id = {i}" for i in lids], POINT_ITERS)

    rqs = [(random.randint(18, 70), random.randint(18, 70)+10) for _ in range(SELECT_ITERS)]
    r["select_range"] = run_select(
        "select_range:", [f"SELECT * FROM users WHERE age BETWEEN {l} AND {h} LIMIT 100" for l, h in rqs],
        SELECT_ITERS)

    fqs = [(random.choice(CITIES), random.randint(18, 60)) for _ in range(SELECT_ITERS)]
    r["select_filtered"] = run_select(
        "select_filtered:", [f"SELECT * FROM users WHERE city = '{c}' AND age > {a} LIMIT 100" for c, a in fqs],
        SELECT_ITERS)

    r["agg_count"] = run_select("agg_count:", ["SELECT COUNT(*) FROM users"]*SELECT_ITERS, SELECT_ITERS)
    r["agg_groupby"] = run_select(
        "agg_groupby:", ["SELECT city, COUNT(*) FROM users GROUP BY city"]*SELECT_ITERS, SELECT_ITERS)

    uids = [random.randint(1, N) for _ in range(100)]
    r["update_single"] = run_select(
        "update_single:", [f"UPDATE users SET score = 99 WHERE id = {u}" for u in uids], 100)

    subprocess.run(["docker","rm","-f","pg-bench"], capture_output=True)
    return r


def print_table(rdb, pg):
    print("\n### Summary (100K rows)\n")
    print(f"{'KPI':<18} {'RedDB':>10} {'Postgres':>10} {'Ratio':>8}")
    print("-" * 50)
    for key in ["insert", "select_no_filter", "select_point", "select_range", "select_filtered",
                "agg_count", "agg_groupby", "update_single"]:
        rd = rdb.get(key, 0); pv = pg.get(key, 0)
        ratio = f"{rd/pv:.2f}x" if pv > 0 else "—"
        print(f"{key:<18} {rd:>10} {pv:>10} {ratio:>8}")


if __name__ == "__main__":
    rdb = run_reddb()
    pg = run_pg()
    print_table(rdb, pg)
    print("\nDONE")

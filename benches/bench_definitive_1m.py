#!/usr/bin/env python3
"""
╔══════════════════════════════════════════════════════════════════════╗
║  DEFINITIVE BENCHMARK — RedDB vs PostgreSQL — 1M rows              ║
║                                                                      ║
║  Dataset A: Standard types (int, float, string, bool)                ║
║  Dataset B: RedDB special types (vector, json, timestamp, ip)        ║
║                                                                      ║
║  18 KPIs across insert, select, update, delete, aggregation          ║
╚══════════════════════════════════════════════════════════════════════╝
"""
import json, time, random, subprocess, os, sys

# ─── Configuration ───
N = 1_000_000
CHUNK = 50_000
POINT_N = 1000
RANGE_N = 200
FILTER_N = 200
UPDATE_SINGLE_N = 1000
UPDATE_MULTI_N = 5   # UPDATE WHERE city = 'NYC' (affects ~100K rows each)
DELETE_SINGLE_N = 1000
DELETE_MULTI_N = 3    # DELETE WHERE age > 75
AGG_N = 50
SELECT_NOFILT_N = 200

random.seed(42)
CITIES = ["NYC","London","Tokyo","Paris","Berlin","Sydney","Toronto","Dubai","Singapore","Mumbai"]
REDDB = "/home/cyber/Work/FF/reddb/target/release/red"
VENV = "/home/cyber/Work/FF/reddb/drivers/python/.venv/lib/python3.12/site-packages"
if VENV not in sys.path: sys.path.insert(0, VENV)

def bench(fn):
    t0 = time.perf_counter(); fn(); return (time.perf_counter() - t0) * 1000

def ops(n, ms):
    return int(n / ms * 1000) if ms > 0 else 0

def fmt(n):
    return f"{n:>12,}"

# ─── Generate Dataset A: Standard Rust types ───
print("Generating Dataset A (standard types)...")
DS_A = [{"id":i+1,"name":f"User_{i}","email":f"u{i}@test.com",
         "age":random.randint(18,80),"city":random.choice(CITIES),
         "score":round(random.uniform(0,100),2),"active":i%2==0}
        for i in range(N)]

LIDS = [random.randint(1, N) for _ in range(POINT_N)]
RQ = [(random.randint(18, 70), random.randint(18, 70) + 10) for _ in range(RANGE_N)]
FQ = [(random.choice(CITIES), random.randint(18, 60)) for _ in range(FILTER_N)]

results = {}

# ═══════════════════════════════════════════════════════════════════
# REDDB
# ═══════════════════════════════════════════════════════════════════
def run_reddb():
    print("\n" + "="*70)
    print("  REDDB — Dataset A (Standard Types) — 1M rows")
    print("="*70)

    subprocess.run(["fuser", "-k", "19051/tcp", "19052/tcp"], capture_output=True)
    time.sleep(1)
    subprocess.run(["rm", "-rf", "/tmp/br"])
    os.makedirs("/tmp/br", exist_ok=True)

    # ── 1. FIRST BOOT ──
    t0 = time.perf_counter()
    proc = subprocess.Popen([REDDB, "server", "--path", "/tmp/br/d.rdb",
        "--grpc-bind", "127.0.0.1:19051", "--wire-bind", "127.0.0.1:19052"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    import reddb_python
    for _ in range(100):
        try:
            c = reddb_python.wire_connect("127.0.0.1:19052")
            c.query("SELECT 1 FROM __health__ LIMIT 1")
            c.close()
            break
        except: time.sleep(0.05)
    first_boot_ms = (time.perf_counter() - t0) * 1000
    print(f"  1. first_boot:       {first_boot_ms:>8.0f} ms")
    results["r_first_boot"] = round(first_boot_ms)

    conn = reddb_python.connect("127.0.0.1:19051")
    wc = reddb_python.wire_connect("127.0.0.1:19052")

    # ── 3. INSERT 1M ──
    pj = None  # build in chunks to save memory
    t0 = time.perf_counter()
    for start in range(0, N, CHUNK):
        end = min(start + CHUNK, N)
        chunk = [json.dumps({"fields": r}) for r in DS_A[start:end]]
        conn.bulk_insert("users", chunk)
    insert_ms = (time.perf_counter() - t0) * 1000
    print(f"  3. insert_1m:        {fmt(ops(N, insert_ms))} ops/sec ({insert_ms:.0f}ms)")
    results["r_insert"] = ops(N, insert_ms)

    # Create indexes
    conn.execute("CREATE INDEX idx_city ON users (city) USING HASH")
    conn.execute("CREATE INDEX idx_age ON users (age) USING BTREE")

    # Memory
    try:
        pid = subprocess.getoutput('pgrep -f "red server"').strip().split('\n')[0]
        rss = int(open(f"/proc/{pid}/status").read().split("VmRSS:")[1].split()[0]) // 1024
        print(f"     memory:           {rss:>8} MB")
        results["r_memory"] = rss
    except: pass

    # ── 4. UPDATE single row ──
    uids = [random.randint(1, N) for _ in range(UPDATE_SINGLE_N)]
    ms = bench(lambda: [wc.query_raw(f"UPDATE users SET age = 99 WHERE _entity_id = {uid}") for uid in uids])
    print(f"  4. update_single:    {fmt(ops(UPDATE_SINGLE_N, ms))} ops/sec")
    results["r_update_single"] = ops(UPDATE_SINGLE_N, ms)

    # ── 5. UPDATE multi rows ──
    ms = bench(lambda: [wc.query_raw(f"UPDATE users SET score = 0 WHERE city = '{random.choice(CITIES)}'") for _ in range(UPDATE_MULTI_N)])
    print(f"  5. update_multi:     {fmt(ops(UPDATE_MULTI_N, ms))} ops/sec ({ms:.0f}ms, ~100K rows each)")
    results["r_update_multi"] = ops(UPDATE_MULTI_N, ms)

    # ── 6. DELETE single row ──
    dids = [random.randint(1, N) for _ in range(DELETE_SINGLE_N)]
    ms = bench(lambda: [wc.query_raw(f"DELETE FROM users WHERE _entity_id = {did}") for did in dids])
    print(f"  6. delete_single:    {fmt(ops(DELETE_SINGLE_N, ms))} ops/sec")
    results["r_delete_single"] = ops(DELETE_SINGLE_N, ms)

    # ── 7. DELETE multi rows ──
    ms = bench(lambda: [wc.query_raw(f"DELETE FROM users WHERE age > {78 + i}") for i in range(DELETE_MULTI_N)])
    print(f"  7. delete_multi:     {fmt(ops(DELETE_MULTI_N, ms))} ops/sec ({ms:.0f}ms)")
    results["r_delete_multi"] = ops(DELETE_MULTI_N, ms)

    # ── 8. SELECT no filter ──
    ms = bench(lambda: [wc.query_raw("SELECT * FROM users LIMIT 100") for _ in range(SELECT_NOFILT_N)])
    print(f"  8. select_no_filter: {fmt(ops(SELECT_NOFILT_N, ms))} ops/sec")
    results["r_select_nofilt"] = ops(SELECT_NOFILT_N, ms)

    # ── 9. SELECT point ──
    ms = bench(lambda: [wc.query_raw(f"SELECT * FROM users WHERE _entity_id = {rid}") for rid in LIDS])
    print(f"  9. select_point:     {fmt(ops(POINT_N, ms))} ops/sec")
    results["r_select_point"] = ops(POINT_N, ms)

    # ── 10. SELECT range ──
    ms = bench(lambda: [wc.query_raw(f"SELECT * FROM users WHERE age BETWEEN {l} AND {h} LIMIT 100") for l,h in RQ])
    print(f" 10. select_range:     {fmt(ops(RANGE_N, ms))} ops/sec")
    results["r_select_range"] = ops(RANGE_N, ms)

    # ── 11. SELECT filtered ──
    ms = bench(lambda: [wc.query_raw(f"SELECT * FROM users WHERE city = '{c}' AND age > {a} LIMIT 100") for c,a in FQ])
    print(f" 11. select_filtered:  {fmt(ops(FILTER_N, ms))} ops/sec")
    results["r_select_filtered"] = ops(FILTER_N, ms)

    # ── 12-16. Aggregations ──
    ms = bench(lambda: [wc.query_raw("SELECT COUNT(*) FROM users") for _ in range(AGG_N)])
    print(f" 12. agg_count:        {fmt(ops(AGG_N, ms))} ops/sec")
    results["r_agg_count"] = ops(AGG_N, ms)

    ms = bench(lambda: [wc.query_raw("SELECT AVG(age) FROM users") for _ in range(AGG_N)])
    print(f" 13. agg_avg:          {fmt(ops(AGG_N, ms))} ops/sec")
    results["r_agg_avg"] = ops(AGG_N, ms)

    ms = bench(lambda: [wc.query_raw("SELECT MIN(score), MAX(score) FROM users") for _ in range(AGG_N)])
    print(f" 14. agg_min_max:      {fmt(ops(AGG_N, ms))} ops/sec")
    results["r_agg_minmax"] = ops(AGG_N, ms)

    ms = bench(lambda: [wc.query_raw("SELECT SUM(score) FROM users") for _ in range(AGG_N)])
    print(f" 15. agg_sum:          {fmt(ops(AGG_N, ms))} ops/sec")
    results["r_agg_sum"] = ops(AGG_N, ms)

    ms = bench(lambda: [wc.query_raw("SELECT city, COUNT(*), AVG(age) FROM users GROUP BY city") for _ in range(AGG_N)])
    print(f" 16. agg_group_by:     {fmt(ops(AGG_N, ms))} ops/sec")
    results["r_agg_groupby"] = ops(AGG_N, ms)

    # ── 2. REBOOT ──
    conn.close(); wc.close()
    proc.terminate(); proc.wait(); time.sleep(1)
    t0 = time.perf_counter()
    proc2 = subprocess.Popen([REDDB, "server", "--path", "/tmp/br/d.rdb",
        "--grpc-bind", "127.0.0.1:19051"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(100):
        try:
            import grpc
            sys.path.insert(0, "/home/cyber/Work/FF/reddb-benchmark")
            from adapters.proto import reddb_pb2 as pb, reddb_pb2_grpc as rpc
            ch = grpc.insecure_channel("127.0.0.1:19051")
            stub = rpc.RedDbStub(ch)
            stub.Health(pb.Empty())
            ch.close()
            break
        except: time.sleep(0.05)
    reboot_ms = (time.perf_counter() - t0) * 1000
    print(f"  2. reboot:           {reboot_ms:>8.0f} ms")
    results["r_reboot"] = round(reboot_ms)
    proc2.terminate(); proc2.wait()

# ═══════════════════════════════════════════════════════════════════
# POSTGRESQL
# ═══════════════════════════════════════════════════════════════════
def run_postgresql():
    print("\n" + "="*70)
    print("  POSTGRESQL — Dataset A (Standard Types) — 1M rows")
    print("="*70)

    try:
        subprocess.run(["docker", "start", "pg-bench"], check=True, capture_output=True)
    except:
        subprocess.run(["docker", "run", "-d", "--name", "pg-bench", "-e", "POSTGRES_PASSWORD=bench",
                        "-p", "5432:5432", "postgres:16-alpine"], capture_output=True)
    time.sleep(4)

    import psycopg2
    from psycopg2.extras import execute_values

    t0 = time.perf_counter()
    conn = psycopg2.connect(host="127.0.0.1", dbname="postgres", user="postgres", password="bench")
    conn.autocommit = True
    first_boot_ms = (time.perf_counter() - t0) * 1000
    print(f"  1. first_boot:       {first_boot_ms:>8.0f} ms (connect)")
    results["p_first_boot"] = round(first_boot_ms)

    cur = conn.cursor()
    cur.execute("DROP TABLE IF EXISTS users")
    cur.execute("""CREATE TABLE users (
        id INT PRIMARY KEY, name TEXT, email TEXT, age INT, city TEXT, score FLOAT, active BOOLEAN)""")

    # Insert
    pg_rows = [(r["id"],r["name"],r["email"],r["age"],r["city"],r["score"],r["active"]) for r in DS_A]
    t0 = time.perf_counter()
    for start in range(0, N, CHUNK):
        end = min(start + CHUNK, N)
        execute_values(cur, "INSERT INTO users (id,name,email,age,city,score,active) VALUES %s", pg_rows[start:end])
    insert_ms = (time.perf_counter() - t0) * 1000
    print(f"  3. insert_1m:        {fmt(ops(N, insert_ms))} ops/sec ({insert_ms:.0f}ms)")
    results["p_insert"] = ops(N, insert_ms)

    cur.execute("CREATE INDEX idx_age ON users(age)")
    cur.execute("CREATE INDEX idx_city_age ON users(city, age)")

    # Update single
    uids = [random.randint(1,N) for _ in range(UPDATE_SINGLE_N)]
    ms = bench(lambda: [cur.execute("UPDATE users SET age=99 WHERE id=%s",(uid,)) for uid in uids])
    print(f"  4. update_single:    {fmt(ops(UPDATE_SINGLE_N, ms))} ops/sec")
    results["p_update_single"] = ops(UPDATE_SINGLE_N, ms)

    # Update multi
    ms = bench(lambda: [cur.execute("UPDATE users SET score=0 WHERE city=%s",(random.choice(CITIES),)) for _ in range(UPDATE_MULTI_N)])
    print(f"  5. update_multi:     {fmt(ops(UPDATE_MULTI_N, ms))} ops/sec ({ms:.0f}ms)")
    results["p_update_multi"] = ops(UPDATE_MULTI_N, ms)

    # Delete single
    dids = [random.randint(1,N) for _ in range(DELETE_SINGLE_N)]
    ms = bench(lambda: [cur.execute("DELETE FROM users WHERE id=%s",(did,)) for did in dids])
    print(f"  6. delete_single:    {fmt(ops(DELETE_SINGLE_N, ms))} ops/sec")
    results["p_delete_single"] = ops(DELETE_SINGLE_N, ms)

    # Delete multi
    ms = bench(lambda: [cur.execute("DELETE FROM users WHERE age > %s",(78+i,)) for i in range(DELETE_MULTI_N)])
    print(f"  7. delete_multi:     {fmt(ops(DELETE_MULTI_N, ms))} ops/sec ({ms:.0f}ms)")
    results["p_delete_multi"] = ops(DELETE_MULTI_N, ms)

    # Select no filter
    ms = bench(lambda: [cur.execute("SELECT * FROM users LIMIT 100") or cur.fetchall() for _ in range(SELECT_NOFILT_N)])
    print(f"  8. select_no_filter: {fmt(ops(SELECT_NOFILT_N, ms))} ops/sec")
    results["p_select_nofilt"] = ops(SELECT_NOFILT_N, ms)

    # Select point
    ms = bench(lambda: [cur.execute("SELECT * FROM users WHERE id=%s",(rid,)) or cur.fetchall() for rid in LIDS])
    print(f"  9. select_point:     {fmt(ops(POINT_N, ms))} ops/sec")
    results["p_select_point"] = ops(POINT_N, ms)

    # Select range
    ms = bench(lambda: [cur.execute("SELECT * FROM users WHERE age BETWEEN %s AND %s LIMIT 100",(l,h)) or cur.fetchall() for l,h in RQ])
    print(f" 10. select_range:     {fmt(ops(RANGE_N, ms))} ops/sec")
    results["p_select_range"] = ops(RANGE_N, ms)

    # Select filtered
    ms = bench(lambda: [cur.execute("SELECT * FROM users WHERE city=%s AND age>%s LIMIT 100",(c,a)) or cur.fetchall() for c,a in FQ])
    print(f" 11. select_filtered:  {fmt(ops(FILTER_N, ms))} ops/sec")
    results["p_select_filtered"] = ops(FILTER_N, ms)

    # Aggregations
    ms = bench(lambda: [cur.execute("SELECT COUNT(*) FROM users") or cur.fetchone() for _ in range(AGG_N)])
    print(f" 12. agg_count:        {fmt(ops(AGG_N, ms))} ops/sec")
    results["p_agg_count"] = ops(AGG_N, ms)

    ms = bench(lambda: [cur.execute("SELECT AVG(age) FROM users") or cur.fetchone() for _ in range(AGG_N)])
    print(f" 13. agg_avg:          {fmt(ops(AGG_N, ms))} ops/sec")
    results["p_agg_avg"] = ops(AGG_N, ms)

    ms = bench(lambda: [cur.execute("SELECT MIN(score), MAX(score) FROM users") or cur.fetchone() for _ in range(AGG_N)])
    print(f" 14. agg_min_max:      {fmt(ops(AGG_N, ms))} ops/sec")
    results["p_agg_minmax"] = ops(AGG_N, ms)

    ms = bench(lambda: [cur.execute("SELECT SUM(score) FROM users") or cur.fetchone() for _ in range(AGG_N)])
    print(f" 15. agg_sum:          {fmt(ops(AGG_N, ms))} ops/sec")
    results["p_agg_sum"] = ops(AGG_N, ms)

    ms = bench(lambda: [cur.execute("SELECT city, COUNT(*), AVG(age) FROM users GROUP BY city") or cur.fetchall() for _ in range(AGG_N)])
    print(f" 16. agg_group_by:     {fmt(ops(AGG_N, ms))} ops/sec")
    results["p_agg_groupby"] = ops(AGG_N, ms)

    # Reboot
    cur.close(); conn.close()
    t0 = time.perf_counter()
    subprocess.run(["docker", "restart", "pg-bench"], capture_output=True)
    for _ in range(100):
        try:
            c2 = psycopg2.connect(host="127.0.0.1", dbname="postgres", user="postgres", password="bench")
            c2.close(); break
        except: time.sleep(0.1)
    reboot_ms = (time.perf_counter() - t0) * 1000
    print(f"  2. reboot:           {reboot_ms:>8.0f} ms")
    results["p_reboot"] = round(reboot_ms)

    # Memory/disk
    try:
        conn2 = psycopg2.connect(host="127.0.0.1", dbname="postgres", user="postgres", password="bench")
        conn2.autocommit = True
        c2 = conn2.cursor()
        c2.execute("SELECT pg_total_relation_size('users')")
        disk = c2.fetchone()[0] // 1024 // 1024
        print(f"     disk:             {disk:>8} MB")
        results["p_disk"] = disk
        c2.close(); conn2.close()
    except: pass

    subprocess.run(["docker", "stop", "pg-bench"], capture_output=True)

# ═══════════════════════════════════════════════════════════════════
# RUN
# ═══════════════════════════════════════════════════════════════════
print("╔══════════════════════════════════════════════════════════════════════╗")
print("║  DEFINITIVE BENCHMARK — 1M rows — Dataset A (Standard Types)       ║")
print("╚══════════════════════════════════════════════════════════════════════╝")

run_reddb()
run_postgresql()

# ═══════════════════════════════════════════════════════════════════
# RESULTS TABLE
# ═══════════════════════════════════════════════════════════════════
def ratio(rk, pk):
    r, p = results.get(rk, 0), results.get(pk, 1)
    if p == 0: return "—"
    v = r / p
    return f"**{v:.1f}x**" if v >= 1.0 else f"_{v:.1f}x_"

print("\n\n## Definitive Benchmark — Dataset A — 1M rows\n")
print("| # | KPI | RedDB Wire | PostgreSQL | Ratio |")
print("|---|-----|-----------|-----------|-------|")
print(f"|  1 | first_boot | {results.get('r_first_boot',0)}ms | {results.get('p_first_boot',0)}ms | — |")
print(f"|  2 | reboot | {results.get('r_reboot',0)}ms | {results.get('p_reboot',0)}ms | — |")
print(f"|  3 | insert_1m | {results.get('r_insert',0):,} | {results.get('p_insert',0):,} | {ratio('r_insert','p_insert')} |")
print(f"|  4 | update_single | {results.get('r_update_single',0):,} | {results.get('p_update_single',0):,} | {ratio('r_update_single','p_update_single')} |")
print(f"|  5 | update_multi | {results.get('r_update_multi',0):,} | {results.get('p_update_multi',0):,} | {ratio('r_update_multi','p_update_multi')} |")
print(f"|  6 | delete_single | {results.get('r_delete_single',0):,} | {results.get('p_delete_single',0):,} | {ratio('r_delete_single','p_delete_single')} |")
print(f"|  7 | delete_multi | {results.get('r_delete_multi',0):,} | {results.get('p_delete_multi',0):,} | {ratio('r_delete_multi','p_delete_multi')} |")
print(f"|  8 | select_no_filter | {results.get('r_select_nofilt',0):,} | {results.get('p_select_nofilt',0):,} | {ratio('r_select_nofilt','p_select_nofilt')} |")
print(f"|  9 | select_point | {results.get('r_select_point',0):,} | {results.get('p_select_point',0):,} | {ratio('r_select_point','p_select_point')} |")
print(f"| 10 | select_range | {results.get('r_select_range',0):,} | {results.get('p_select_range',0):,} | {ratio('r_select_range','p_select_range')} |")
print(f"| 11 | select_filtered | {results.get('r_select_filtered',0):,} | {results.get('p_select_filtered',0):,} | {ratio('r_select_filtered','p_select_filtered')} |")
print(f"| 12 | agg COUNT | {results.get('r_agg_count',0):,} | {results.get('p_agg_count',0):,} | {ratio('r_agg_count','p_agg_count')} |")
print(f"| 13 | agg AVG | {results.get('r_agg_avg',0):,} | {results.get('p_agg_avg',0):,} | {ratio('r_agg_avg','p_agg_avg')} |")
print(f"| 14 | agg MIN/MAX | {results.get('r_agg_minmax',0):,} | {results.get('p_agg_minmax',0):,} | {ratio('r_agg_minmax','p_agg_minmax')} |")
print(f"| 15 | agg SUM | {results.get('r_agg_sum',0):,} | {results.get('p_agg_sum',0):,} | {ratio('r_agg_sum','p_agg_sum')} |")
print(f"| 16 | agg GROUP BY | {results.get('r_agg_groupby',0):,} | {results.get('p_agg_groupby',0):,} | {ratio('r_agg_groupby','p_agg_groupby')} |")
print(f"| 17 | memory | {results.get('r_memory',0)} MB | — | — |")
print()
print("*ops/sec — higher is better. **bold** = RedDB wins.*")

#!/usr/bin/env python3
"""
╔══════════════════════════════════════════════════════════════════════╗
║  DEFINITIVE DUAL BENCHMARK — RedDB vs PostgreSQL — 1M rows         ║
║                                                                      ║
║  Report A: Standard types (int, float, string, bool)                 ║
║  Report B: Optimized types (timestamp, json, ip, tags)               ║
║                                                                      ║
║  19 KPIs per report. Fresh start each run.                           ║
╚══════════════════════════════════════════════════════════════════════╝
"""
import json, time, random, subprocess, os, sys

# ─── Config ───
N = 1_000_000
CHUNK = 50_000
random.seed(42)
CITIES = ["NYC","London","Tokyo","Paris","Berlin","Sydney","Toronto","Dubai","Singapore","Mumbai"]
REDDB = "/home/cyber/Work/FF/reddb/target/release/red"
VENV = "/home/cyber/Work/FF/reddb/drivers/python/.venv/lib/python3.12/site-packages"
if VENV not in sys.path: sys.path.insert(0, VENV)

def bench(fn):
    t0 = time.perf_counter(); fn(); return (time.perf_counter() - t0) * 1000
def ops(n, ms): return int(n/ms*1000) if ms > 0 else 0
def fmt(n): return f"{n:>12,}"
def dir_size_mb(p):
    if not os.path.exists(p): return 0.0
    return round(sum(os.path.getsize(os.path.join(dp,f)) for dp,_,fns in os.walk(p) for f in fns)/1024/1024, 1)
def get_rss(pid):
    try: return int(open(f"/proc/{pid}/status").read().split("VmRSS:")[1].split()[0])//1024
    except: return 0

# ═══════════════════════════════════════════════════════════════
def generate_dataset_a():
    """Standard Rust types: int, float, string, bool"""
    return [{"id":i+1,"name":f"User_{i}","email":f"u{i}@test.com",
             "age":random.randint(18,80),"city":random.choice(CITIES),
             "score":round(random.uniform(0,100),2),"active":i%2==0}
            for i in range(N)]

def generate_dataset_b():
    """RedDB optimized types: timestamp, json-as-text, ip, tags"""
    return [{"id":i+1,"sensor":f"S_{i}","reading":round(i*0.123,3),
             "ts":1700000000+i,"ip":f"10.0.{(i//256)%256}.{i%256}",
             "tags":f"env:prod,region:{CITIES[i%10].lower()}",
             "priority":i%5,"config":json.dumps({"v":i%10,"src":"api"})}
            for i in range(N)]

# ═══════════════════════════════════════════════════════════════
def run_reddb(dataset, label, table):
    print(f"\n{'='*70}")
    print(f"  REDDB — {label} — 1M rows")
    print(f"{'='*70}")
    r = {}

    # Kill + clean
    subprocess.run(["fuser","-k","19051/tcp","19052/tcp"], capture_output=True)
    time.sleep(1)
    subprocess.run(["rm","-rf","/tmp/br"])
    os.makedirs("/tmp/br", exist_ok=True)

    # 1. First boot
    t0 = time.perf_counter()
    proc = subprocess.Popen([REDDB,"server","--path","/tmp/br/d.rdb",
        "--grpc-bind","127.0.0.1:19051","--wire-bind","127.0.0.1:19052"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    import reddb_python
    for _ in range(100):
        try:
            c=reddb_python.wire_connect("127.0.0.1:19052"); c.close(); break
        except: time.sleep(0.05)
    r["first_boot"] = round((time.perf_counter()-t0)*1000)
    print(f"  1. first_boot:       {r['first_boot']:>8} ms")

    conn = reddb_python.connect("127.0.0.1:19051")
    wc = reddb_python.wire_connect("127.0.0.1:19052")
    pid = subprocess.getoutput('pgrep -f "red server"').strip().split('\n')[0]

    # 3. Insert 1M
    t0 = time.perf_counter()
    for s in range(0,N,CHUNK):
        e=min(s+CHUNK,N)
        pj=[json.dumps({"fields":rec}) for rec in dataset[s:e]]
        conn.bulk_insert(table, pj)
    ins_ms = (time.perf_counter()-t0)*1000
    r["insert"] = ops(N,ins_ms)
    print(f"  3. insert_1m:        {fmt(r['insert'])} ops/sec ({ins_ms:.0f}ms)")

    # Resources post-insert
    r["ram_post"] = get_rss(pid)
    r["disk_post"] = dir_size_mb("/tmp/br")
    print(f"     ram:              {r['ram_post']:>8} MB")
    print(f"     disk:             {r['disk_post']:>8} MB")

    # Create indexes
    field_eq = "city" if table == "users" else "tags"
    field_range = "age" if table == "users" else "ts"
    field_score = "score" if table == "users" else "reading"
    conn.execute(f"CREATE INDEX idx_eq ON {table} ({field_eq}) USING HASH")
    conn.execute(f"CREATE INDEX idx_rng ON {table} ({field_range}) USING BTREE")

    # 4. Update single
    uids = [random.randint(1,N) for _ in range(1000)]
    ms = bench(lambda: [wc.query_raw(f"UPDATE {table} SET {field_score} = 99 WHERE _entity_id = {u}") for u in uids])
    r["update_single"] = ops(1000,ms)
    print(f"  4. update_single:    {fmt(r['update_single'])} ops/sec")

    # 5. Update multi
    eq_val = "'NYC'" if table == "users" else "'env:prod,region:nyc'"
    ms = bench(lambda: [wc.query_raw(f"UPDATE {table} SET {field_score} = 0 WHERE {field_eq} = {eq_val}") for _ in range(3)])
    r["update_multi"] = ops(3,ms)
    print(f"  5. update_multi:     {fmt(r['update_multi'])} ops/sec ({ms:.0f}ms)")

    # 6. Delete single
    dids = [random.randint(1,N) for _ in range(1000)]
    ms = bench(lambda: [wc.query_raw(f"DELETE FROM {table} WHERE _entity_id = {d}") for d in dids])
    r["delete_single"] = ops(1000,ms)
    print(f"  6. delete_single:    {fmt(r['delete_single'])} ops/sec")

    # 7. Delete multi
    if table == "users":
        ms = bench(lambda: [wc.query_raw(f"DELETE FROM {table} WHERE {field_range} > {78+i}") for i in range(3)])
    else:
        ms = bench(lambda: [wc.query_raw(f"DELETE FROM {table} WHERE {field_range} > {1700000000+N-1000+i*300}") for i in range(3)])
    r["delete_multi"] = ops(3,ms)
    print(f"  7. delete_multi:     {fmt(r['delete_multi'])} ops/sec ({ms:.0f}ms)")

    # 8. Select no filter
    ms = bench(lambda: [wc.query_raw(f"SELECT * FROM {table} LIMIT 100") for _ in range(200)])
    r["select_nofilt"] = ops(200,ms)
    print(f"  8. select_no_filter: {fmt(r['select_nofilt'])} ops/sec")

    # 9. Select point
    lids = [random.randint(1,N) for _ in range(1000)]
    ms = bench(lambda: [wc.query_raw(f"SELECT * FROM {table} WHERE _entity_id = {rid}") for rid in lids])
    r["select_point"] = ops(1000,ms)
    print(f"  9. select_point:     {fmt(r['select_point'])} ops/sec")

    # 10. Select range
    if table == "users":
        rqs = [(random.randint(18,70),random.randint(18,70)+10) for _ in range(200)]
        ms = bench(lambda: [wc.query_raw(f"SELECT * FROM {table} WHERE {field_range} BETWEEN {l} AND {h} LIMIT 100") for l,h in rqs])
    else:
        rqs = [(1700000000+random.randint(0,N),1700000000+random.randint(0,N)+5000) for _ in range(200)]
        ms = bench(lambda: [wc.query_raw(f"SELECT * FROM {table} WHERE {field_range} BETWEEN {l} AND {h} LIMIT 100") for l,h in rqs])
    r["select_range"] = ops(200,ms)
    print(f" 10. select_range:     {fmt(r['select_range'])} ops/sec")

    # 11. Select filtered
    if table == "users":
        fqs = [(random.choice(CITIES),random.randint(18,60)) for _ in range(200)]
        ms = bench(lambda: [wc.query_raw(f"SELECT * FROM {table} WHERE city = '{c}' AND age > {a} LIMIT 100") for c,a in fqs])
    else:
        ms = bench(lambda: [wc.query_raw(f"SELECT * FROM {table} WHERE tags = 'env:prod,region:nyc' AND ts > {1700000000+random.randint(0,N)} LIMIT 100") for _ in range(200)])
    r["select_filtered"] = ops(200,ms)
    print(f" 11. select_filtered:  {fmt(r['select_filtered'])} ops/sec")

    # 12-16. Aggregations
    ms = bench(lambda: [wc.query_raw(f"SELECT COUNT(*) FROM {table}") for _ in range(50)])
    r["agg_count"] = ops(50,ms)
    print(f" 12. agg COUNT:        {fmt(r['agg_count'])} ops/sec")

    ms = bench(lambda: [wc.query_raw(f"SELECT AVG({field_range}) FROM {table}") for _ in range(50)])
    r["agg_avg"] = ops(50,ms)
    print(f" 13. agg AVG:          {fmt(r['agg_avg'])} ops/sec")

    ms = bench(lambda: [wc.query_raw(f"SELECT MIN({field_score}), MAX({field_score}) FROM {table}") for _ in range(50)])
    r["agg_minmax"] = ops(50,ms)
    print(f" 14. agg MIN/MAX:      {fmt(r['agg_minmax'])} ops/sec")

    ms = bench(lambda: [wc.query_raw(f"SELECT SUM({field_score}) FROM {table}") for _ in range(50)])
    r["agg_sum"] = ops(50,ms)
    print(f" 15. agg SUM:          {fmt(r['agg_sum'])} ops/sec")

    ms = bench(lambda: [wc.query_raw(f"SELECT {field_eq}, COUNT(*) FROM {table} GROUP BY {field_eq}") for _ in range(50)])
    r["agg_groupby"] = ops(50,ms)
    print(f" 16. agg GROUP BY:     {fmt(r['agg_groupby'])} ops/sec")

    # Peak resources
    r["ram_peak"] = get_rss(pid)
    r["disk_peak"] = dir_size_mb("/tmp/br")
    print(f"     ram (peak):       {r['ram_peak']:>8} MB")
    print(f"     disk (peak):      {r['disk_peak']:>8} MB")

    # 2. Reboot
    conn.close(); wc.close()
    proc.terminate(); proc.wait(); time.sleep(1)
    t0 = time.perf_counter()
    proc2 = subprocess.Popen([REDDB,"server","--path","/tmp/br/d.rdb",
        "--grpc-bind","127.0.0.1:19051"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(100):
        try:
            import grpc; sys.path.insert(0,"/home/cyber/Work/FF/reddb-benchmark")
            from adapters.proto import reddb_pb2 as pb, reddb_pb2_grpc as rpc
            ch=grpc.insecure_channel("127.0.0.1:19051"); stub=rpc.RedDbStub(ch)
            stub.Health(pb.Empty()); ch.close(); break
        except: time.sleep(0.05)
    r["reboot"] = round((time.perf_counter()-t0)*1000)
    print(f"  2. reboot:           {r['reboot']:>8} ms")
    proc2.terminate(); proc2.wait()
    return r

# ═══════════════════════════════════════════════════════════════
def run_postgresql(dataset, label, table):
    print(f"\n{'='*70}")
    print(f"  POSTGRESQL — {label} — 1M rows")
    print(f"{'='*70}")
    r = {}

    # Fresh container
    subprocess.run(["docker","rm","-f","pg-bench"], capture_output=True)
    subprocess.run(["docker","run","-d","--name","pg-bench","-e","POSTGRES_PASSWORD=bench",
                    "-p","5432:5432","postgres:16-alpine"], capture_output=True)
    time.sleep(5)

    import psycopg2
    from psycopg2.extras import execute_values

    t0 = time.perf_counter()
    conn = psycopg2.connect(host="127.0.0.1",dbname="postgres",user="postgres",password="bench")
    conn.autocommit = True
    r["first_boot"] = round((time.perf_counter()-t0)*1000)
    print(f"  1. first_boot:       {r['first_boot']:>8} ms")
    cur = conn.cursor()

    if table == "users":
        cur.execute("CREATE TABLE users (id INT PRIMARY KEY,name TEXT,email TEXT,age INT,city TEXT,score FLOAT,active BOOLEAN)")
        cols = "(id,name,email,age,city,score,active)"
        pg_rows = [(d["id"],d["name"],d["email"],d["age"],d["city"],d["score"],d["active"]) for d in dataset]
        field_eq,field_range,field_score = "city","age","score"
    else:
        cur.execute("CREATE TABLE sensors (id INT PRIMARY KEY,sensor TEXT,reading FLOAT,ts BIGINT,ip TEXT,tags TEXT,priority INT,config TEXT)")
        cols = "(id,sensor,reading,ts,ip,tags,priority,config)"
        pg_rows = [(d["id"],d["sensor"],d["reading"],d["ts"],d["ip"],d["tags"],d["priority"],d["config"]) for d in dataset]
        field_eq,field_range,field_score = "tags","ts","reading"

    t0 = time.perf_counter()
    for s in range(0,N,CHUNK):
        e=min(s+CHUNK,N)
        execute_values(cur, f"INSERT INTO {table} {cols} VALUES %s", pg_rows[s:e])
    ins_ms = (time.perf_counter()-t0)*1000
    r["insert"] = ops(N,ins_ms)
    print(f"  3. insert_1m:        {fmt(r['insert'])} ops/sec ({ins_ms:.0f}ms)")

    cur.execute(f"CREATE INDEX idx_rng ON {table}({field_range})")
    if table == "users":
        cur.execute(f"CREATE INDEX idx_eq ON {table}({field_eq},{field_range})")
    else:
        cur.execute(f"CREATE INDEX idx_eq ON {table}({field_eq})")

    # Resources
    pg_mem = subprocess.getoutput("docker stats pg-bench --no-stream --format '{{.MemUsage}}'").split("/")[0].strip()
    cur.execute(f"SELECT pg_total_relation_size('{table}')")
    pg_disk = cur.fetchone()[0]//1024//1024
    r["ram_post"] = pg_mem; r["disk_post"] = pg_disk
    print(f"     ram:              {pg_mem:>8}")
    print(f"     disk:             {pg_disk:>8} MB")

    # 4. Update single
    uids = [random.randint(1,N) for _ in range(1000)]
    ms = bench(lambda: [cur.execute(f"UPDATE {table} SET {field_score}=99 WHERE id=%s",(u,)) for u in uids])
    r["update_single"] = ops(1000,ms)
    print(f"  4. update_single:    {fmt(r['update_single'])} ops/sec")

    # 5. Update multi
    eq_val = "NYC" if table=="users" else "env:prod,region:nyc"
    ms = bench(lambda: [cur.execute(f"UPDATE {table} SET {field_score}=0 WHERE {field_eq}=%s",(eq_val,)) for _ in range(3)])
    r["update_multi"] = ops(3,ms)
    print(f"  5. update_multi:     {fmt(r['update_multi'])} ops/sec ({ms:.0f}ms)")

    # 6-7. Delete
    dids = [random.randint(1,N) for _ in range(1000)]
    ms = bench(lambda: [cur.execute(f"DELETE FROM {table} WHERE id=%s",(d,)) for d in dids])
    r["delete_single"] = ops(1000,ms)
    print(f"  6. delete_single:    {fmt(r['delete_single'])} ops/sec")

    if table == "users":
        ms = bench(lambda: [cur.execute(f"DELETE FROM {table} WHERE age > %s",(78+i,)) for i in range(3)])
    else:
        ms = bench(lambda: [cur.execute(f"DELETE FROM {table} WHERE ts > %s",(1700000000+N-1000+i*300,)) for i in range(3)])
    r["delete_multi"] = ops(3,ms)
    print(f"  7. delete_multi:     {fmt(r['delete_multi'])} ops/sec ({ms:.0f}ms)")

    # 8-11. Selects
    ms = bench(lambda: [cur.execute(f"SELECT * FROM {table} LIMIT 100") or cur.fetchall() for _ in range(200)])
    r["select_nofilt"] = ops(200,ms)
    print(f"  8. select_no_filter: {fmt(r['select_nofilt'])} ops/sec")

    lids = [random.randint(1,N) for _ in range(1000)]
    ms = bench(lambda: [cur.execute(f"SELECT * FROM {table} WHERE id=%s",(rid,)) or cur.fetchall() for rid in lids])
    r["select_point"] = ops(1000,ms)
    print(f"  9. select_point:     {fmt(r['select_point'])} ops/sec")

    if table == "users":
        rqs = [(random.randint(18,70),random.randint(18,70)+10) for _ in range(200)]
    else:
        rqs = [(1700000000+random.randint(0,N),1700000000+random.randint(0,N)+5000) for _ in range(200)]
    ms = bench(lambda: [cur.execute(f"SELECT * FROM {table} WHERE {field_range} BETWEEN %s AND %s LIMIT 100",rq) or cur.fetchall() for rq in rqs])
    r["select_range"] = ops(200,ms)
    print(f" 10. select_range:     {fmt(r['select_range'])} ops/sec")

    if table == "users":
        fqs = [(random.choice(CITIES),random.randint(18,60)) for _ in range(200)]
        ms = bench(lambda: [cur.execute(f"SELECT * FROM {table} WHERE city=%s AND age>%s LIMIT 100",(c,a)) or cur.fetchall() for c,a in fqs])
    else:
        ms = bench(lambda: [cur.execute(f"SELECT * FROM {table} WHERE tags=%s AND ts>%s LIMIT 100",("env:prod,region:nyc",1700000000+random.randint(0,N))) or cur.fetchall() for _ in range(200)])
    r["select_filtered"] = ops(200,ms)
    print(f" 11. select_filtered:  {fmt(r['select_filtered'])} ops/sec")

    # 12-16. Aggregations
    ms = bench(lambda: [cur.execute(f"SELECT COUNT(*) FROM {table}") or cur.fetchone() for _ in range(50)])
    r["agg_count"] = ops(50,ms)
    print(f" 12. agg COUNT:        {fmt(r['agg_count'])} ops/sec")

    ms = bench(lambda: [cur.execute(f"SELECT AVG({field_range}) FROM {table}") or cur.fetchone() for _ in range(50)])
    r["agg_avg"] = ops(50,ms)
    print(f" 13. agg AVG:          {fmt(r['agg_avg'])} ops/sec")

    ms = bench(lambda: [cur.execute(f"SELECT MIN({field_score}),MAX({field_score}) FROM {table}") or cur.fetchone() for _ in range(50)])
    r["agg_minmax"] = ops(50,ms)
    print(f" 14. agg MIN/MAX:      {fmt(r['agg_minmax'])} ops/sec")

    ms = bench(lambda: [cur.execute(f"SELECT SUM({field_score}) FROM {table}") or cur.fetchone() for _ in range(50)])
    r["agg_sum"] = ops(50,ms)
    print(f" 15. agg SUM:          {fmt(r['agg_sum'])} ops/sec")

    ms = bench(lambda: [cur.execute(f"SELECT {field_eq},COUNT(*),AVG({field_range}) FROM {table} GROUP BY {field_eq}") or cur.fetchall() for _ in range(50)])
    r["agg_groupby"] = ops(50,ms)
    print(f" 16. agg GROUP BY:     {fmt(r['agg_groupby'])} ops/sec")

    # Peak resources
    pg_mem2 = subprocess.getoutput("docker stats pg-bench --no-stream --format '{{.MemUsage}}'").split("/")[0].strip()
    cur.execute(f"SELECT pg_total_relation_size('{table}')")
    pg_disk2 = cur.fetchone()[0]//1024//1024
    r["ram_peak"] = pg_mem2; r["disk_peak"] = pg_disk2
    print(f"     ram (peak):       {pg_mem2:>8}")
    print(f"     disk (peak):      {pg_disk2:>8} MB")

    # 2. Reboot
    cur.close(); conn.close()
    t0 = time.perf_counter()
    subprocess.run(["docker","restart","pg-bench"], capture_output=True)
    for _ in range(100):
        try:
            c2=psycopg2.connect(host="127.0.0.1",dbname="postgres",user="postgres",password="bench")
            c2.close(); break
        except: time.sleep(0.1)
    r["reboot"] = round((time.perf_counter()-t0)*1000)
    print(f"  2. reboot:           {r['reboot']:>8} ms")

    subprocess.run(["docker","rm","-f","pg-bench"], capture_output=True)
    return r

# ═══════════════════════════════════════════════════════════════
def print_report(label, rdb, pg):
    def ratio(rk, pk):
        rv, pv = rdb.get(rk, 0), pg.get(pk, 1)
        if isinstance(rv, str) or isinstance(pv, str): return "—"
        if pv == 0: return "—"
        v = rv / pv
        return f"**{v:.1f}x**" if v >= 1.0 else f"_{v:.1f}x_"

    print(f"\n\n## {label} — 1M rows\n")
    print("| # | KPI | RedDB Wire | PostgreSQL | Ratio |")
    print("|---|-----|-----------|-----------|-------|")
    for i, (name, rk) in enumerate([
        ("first_boot","first_boot"),("reboot","reboot"),("insert_1m","insert"),
        ("update_single","update_single"),("update_multi","update_multi"),
        ("delete_single","delete_single"),("delete_multi","delete_multi"),
        ("select_no_filter","select_nofilt"),("select_point","select_point"),
        ("select_range","select_range"),("select_filtered","select_filtered"),
        ("agg COUNT","agg_count"),("agg AVG","agg_avg"),("agg MIN/MAX","agg_minmax"),
        ("agg SUM","agg_sum"),("agg GROUP BY","agg_groupby"),
    ], 1):
        rv = rdb.get(rk, 0)
        pv = pg.get(rk, 0)
        if rk in ("first_boot","reboot"):
            print(f"| {i:>2} | {name} | {rv}ms | {pv}ms | — |")
        else:
            rv_s = f"{rv:,}" if isinstance(rv,int) else str(rv)
            pv_s = f"{pv:,}" if isinstance(pv,int) else str(pv)
            print(f"| {i:>2} | {name} | {rv_s} | {pv_s} | {ratio(rk,rk)} |")

    print(f"\n### Resources\n")
    print("| Resource | RedDB | PostgreSQL |")
    print("|----------|-------|-----------|")
    print(f"| RAM (post-insert) | {rdb.get('ram_post',0)} MB | {pg.get('ram_post','?')} |")
    print(f"| RAM (peak) | {rdb.get('ram_peak',0)} MB | {pg.get('ram_peak','?')} |")
    print(f"| Disk (post-insert) | {rdb.get('disk_post',0)} MB | {pg.get('disk_post',0)} MB |")
    print(f"| Disk (peak) | {rdb.get('disk_peak',0)} MB | {pg.get('disk_peak',0)} MB |")

# ═══════════════════════════════════════════════════════════════
# RUN
# ═══════════════════════════════════════════════════════════════
print("╔══════════════════════════════════════════════════════════════════════╗")
print("║  DEFINITIVE DUAL BENCHMARK — 1M rows — Fresh Start Each Run       ║")
print("╚══════════════════════════════════════════════════════════════════════╝")

# Report A: Standard types
print("\n\n" + "█"*70)
print("█  REPORT A: STANDARD TYPES (int, float, string, bool)")
print("█"*70)
random.seed(42)
ds_a = generate_dataset_a()
rdb_a = run_reddb(ds_a, "Standard Types", "users")
random.seed(42)
ds_a = generate_dataset_a()
pg_a = run_postgresql(ds_a, "Standard Types", "users")
print_report("Report A: Standard Types", rdb_a, pg_a)

# Report B: Optimized types
print("\n\n" + "█"*70)
print("█  REPORT B: OPTIMIZED TYPES (timestamp, json, ip, tags)")
print("█"*70)
random.seed(42)
ds_b = generate_dataset_b()
rdb_b = run_reddb(ds_b, "Optimized Types", "sensors")
random.seed(42)
ds_b = generate_dataset_b()
pg_b = run_postgresql(ds_b, "Optimized Types", "sensors")
print_report("Report B: Optimized Types", rdb_b, pg_b)

print("\n\nDONE")

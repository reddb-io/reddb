import concurrent.futures, hashlib, json, os, pathlib, signal, socket, subprocess, sys, tempfile, time, urllib.error, urllib.request
binary = pathlib.Path(sys.argv[1]).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='reddb-http-recovery-', dir=os.environ.get('REDD_TEST_TMPDIR')))
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0)); port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}'
path=root/'test.rdb'
log=(root/'server.log').open('w')
args=[str(binary),'server','--http','--http-bind',f'127.0.0.1:{port}','--grpc-bind','127.0.0.1:0','--wire-bind','127.0.0.1:0','--path',str(path),'--no-auth','--bootstrap-preset','simple','--workers','1','--no-log-file']
p=None

def start():
    global p
    p=subprocess.Popen(args,stdout=log,stderr=log)
    for _ in range(240):
        if p.poll() is not None: raise RuntimeError(f'server exited {p.returncode}, log: {root}')
        try:
            with urllib.request.urlopen(base+'/health/ready',timeout=1) as r:
                if json.load(r).get('status')=='ready': return
        except (OSError, ValueError): pass
        time.sleep(.25)
    raise RuntimeError(f'server did not become ready, log: {root}')

def query(sql,params=None):
    data={'query':sql}
    if params is not None:data['params']=params
    request=urllib.request.Request(base+'/query',data=json.dumps(data).encode(),headers={'content-type':'application/json'})
    with urllib.request.urlopen(request,timeout=30) as r: result=json.load(r)
    if not result.get('ok'): raise RuntimeError('query error '+str(result.get('error')))
    return result

def rows(table,key):
    result=query(f'SELECT id, observed_at FROM {table} WHERE id = $1',[key])
    return [row['values'] for row in result['result']['records']]

try:
    start()
    for suffix,pk in [('pk',' PRIMARY KEY'),('unconstrained','')]:
        table='capabilities_'+suffix
        query(f'CREATE TABLE {table} (id TEXT{pk}, environment TEXT, park_id TEXT, cluster_id TEXT, observed_at TEXT, payload TEXT)')
        for key in ('A','B'):query(f'INSERT INTO {table} (id,environment,park_id,cluster_id,observed_at,payload) VALUES ($1,$2,$3,$4,$5,$6)',[key,'stg','park','cluster','initial','[]'])
        for n in range(30):
            query(f'UPDATE {table} SET observed_at = $1, payload = $2 WHERE id = $3',[f'serial-{n}','[]','A'])
            assert all(len(rows(table,key))==1 for key in ('A','B'))
        def writer(w):
            for n in range(12):query(f'UPDATE {table} SET observed_at = $1, payload = $2 WHERE id = $3',[f'parallel-{w}-{n}','[]','A'])
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:list(pool.map(writer,range(3)))
        assert all(len(rows(table,key))==1 for key in ('A','B'))
    p.send_signal(signal.SIGTERM); p.wait(timeout=30); assert p.returncode==0
    start()
    for table in ('capabilities_pk','capabilities_unconstrained'):
        assert all(len(rows(table,key))==1 for key in ('A','B'))
        query(f'UPDATE {table} SET observed_at = $1 WHERE id = $2',['acknowledged-before-kill','A'])
        assert rows(table,'A')[0]['observed_at']=='acknowledged-before-kill'
    p.kill();p.wait(timeout=30);start()
    for table in ('capabilities_pk','capabilities_unconstrained'):
        for key in ('A','B'):
            got=rows(table,key);assert len(got)==1
            if key=='A':assert got[0]['observed_at']=='acknowledged-before-kill'
    with binary.open('rb') as f: digest=hashlib.file_digest(f,'sha256').hexdigest()
    print(json.dumps({'binary_sha256':digest,'clean_restart':'pass','controlled_sigkill':'pass','three_writers':'pass','database':str(path)}))
finally:
    if p is not None and p.poll() is None:
        p.terminate()
        try:p.wait(timeout=30)
        except subprocess.TimeoutExpired:p.kill();p.wait()
    log.close()

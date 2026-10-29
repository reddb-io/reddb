import hashlib,json,os,pathlib,signal,socket,subprocess,sys,tempfile,time,urllib.request
old=pathlib.Path(sys.argv[1]).resolve(); new=pathlib.Path(sys.argv[2]).resolve()
root=pathlib.Path(tempfile.mkdtemp(prefix='reddb-upgrade-',dir=os.environ.get('REDD_TEST_TMPDIR')))
with socket.socket() as sock:
    sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
base=f'http://127.0.0.1:{port}'; log=(root/'server.log').open('w'); p=None

def start(binary):
    global p
    args=[str(binary),'server','--http','--http-bind',f'127.0.0.1:{port}','--grpc-bind','127.0.0.1:0','--wire-bind','127.0.0.1:0','--path',str(root/'test.rdb'),'--no-auth','--bootstrap-preset','simple','--workers','1','--no-log-file']
    p=subprocess.Popen(args,stdout=log,stderr=log)
    for _ in range(240):
        if p.poll() is not None:raise RuntimeError(f'server exited {p.returncode}, log {root}')
        try:
            with urllib.request.urlopen(base+'/health/ready',timeout=1) as r:
                if json.load(r).get('status')=='ready':return
        except (OSError,ValueError):pass
        time.sleep(.25)
    raise RuntimeError(f'server not ready, log {root}')

def query(sql,params=None):
    data={'query':sql}
    if params is not None:data['params']=params
    req=urllib.request.Request(base+'/query',data=json.dumps(data).encode(),headers={'content-type':'application/json'})
    with urllib.request.urlopen(req,timeout=30) as r:result=json.load(r)
    if not result.get('ok'):raise RuntimeError('query error '+str(result.get('error')))
    return result

def value():
    result=query('SELECT id, value FROM upgrade_rows WHERE id = $1',['A'])
    records=[x['values'] for x in result['result']['records']]
    assert len(records)==1,records
    return records[0]['value']
try:
    start(old)
    query('CREATE TABLE upgrade_rows (id TEXT PRIMARY KEY, value TEXT)')
    query('INSERT INTO upgrade_rows (id,value) VALUES ($1,$2)',['A','written-by-stable'])
    assert value()=='written-by-stable'
    p.send_signal(signal.SIGTERM);p.wait(timeout=30);assert p.returncode==0
    start(new);assert value()=='written-by-stable'
    query('UPDATE upgrade_rows SET value = $1 WHERE id = $2',['written-by-new','A'])
    assert value()=='written-by-new'
    p.kill();p.wait(timeout=30);start(new);assert value()=='written-by-new'
    hashes=[]
    for binary in (old,new):
        with binary.open('rb') as f:hashes.append(hashlib.file_digest(f,'sha256').hexdigest())
    print(json.dumps({'old_binary_sha256':hashes[0],'new_binary_sha256':hashes[1],'stable_to_new':'pass','controlled_sigkill':'pass'}))
finally:
    if p is not None and p.poll() is None:
        p.terminate()
        try:p.wait(timeout=30)
        except subprocess.TimeoutExpired:p.kill();p.wait()
    log.close()

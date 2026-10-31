from pathlib import Path
import os,sys,subprocess,time,json,shutil,traceback,zipfile,hashlib
os.environ['OPENBLAS_NUM_THREADS']='4';os.environ['OMP_NUM_THREADS']='4'
folder=Path('/workspace/seekdb_hipvs_notebook');root=Path('/workspace/seekdb-v10');lab=Path('/workspace/amd-v10-lab')
dataset_name=sys.argv[1];status=folder/('amd-v10-selected-'+dataset_name+'-status.json');state={'stage':'waiting','results':[],'official_test':False}
def save():status.write_text(json.dumps(state,indent=2))
save()
try:
 for _ in range(600):
  f=folder/'amd-v10-update-status.json'
  if f.exists():
   j=json.loads(f.read_text())
   if j['stage']=='failed':raise RuntimeError(j['error'])
   if j['stage']=='completed':break
  time.sleep(2)
 else:raise RuntimeError('build did not complete')
 wheel=next(Path('/workspace').glob('**/pymysql-1.1.2-py3-none-any.whl'));packages=Path('/workspace/amd-v10-packages');packages.mkdir(exist_ok=True);zipfile.ZipFile(wheel).extractall(packages);sys.path.insert(0,str(packages))
 import pymysql,numpy as np
 variants=Path('/workspace/amd-v10-selected-variants');variants.mkdir(exist_ok=True)
 env=os.environ.copy();env['LD_LIBRARY_PATH']='/opt/hipvs/lib:/opt/rocm/lib'
 # Compile the same new bridge with 16 or 10 iterations; search remains fixed.
 for name,niter in [('n16',16),('n10',10)]:
  p=variants/name;p.mkdir(exist_ok=True)
  cmd=['/opt/rocm/bin/hipcc','-shared','-fPIC','-O2','-pthread','-D__HIP_PLATFORM_AMD__','-DSK_CAGRA_NITER='+str(niter),*(['-DSK_CAGRA_SEARCH_WIDTH=4','-DSK_CAGRA_RANDOM_SAMPLINGS=1'] if name=='n16' else []),'-I/opt/hipvs/include','-I/opt/rocm/include','-I'+str(root/'tools/hipvs_bridge'),str(root/'tools/hipvs_bridge/seekdb_cuvs_bridge.c'),'-L/opt/hipvs/lib','-lcuvs_c','-Wl,-rpath,/opt/hipvs/lib:/opt/rocm/lib','-o',str(p/'libseekdb_cuvs_bridge.so')]
  x=subprocess.run(cmd,env=env,capture_output=True,text=True,timeout=180)
  if x.returncode:raise RuntimeError(x.stderr[-3000:])
 if dataset_name=='sift':
  base=np.load(folder/'amd-sift512.npz')['base'];qs=np.load(folder/'amd-sift-queries4096.npz')['queries']
 else:
  rng=np.random.default_rng(20261104+256);base=rng.normal(size=(250000,256)).astype('float32');qs=rng.normal(size=(4096,256)).astype('float32')
 truth_file=lab/('final-'+dataset_name+'-truth.npy')
 if truth_file.exists():truth=np.load(truth_file)
 else:
  state['stage']='exact_reference';save();truth=np.empty((len(qs),100),dtype=np.int64);norm=np.einsum('ij,ij->i',base,base)
  for i in range(0,len(qs),32):
   q=qs[i:i+32];dist=norm[None,:]+np.einsum('ij,ij->i',q,q)[:,None]-2*q@base.T
   ix=np.argpartition(dist,99,axis=1)[:,:100];order=np.argsort(np.take_along_axis(dist,ix,axis=1),axis=1);truth[i:i+len(q)]=np.take_along_axis(ix,order,axis=1)
  np.save(truth_file,truth)
 n,dim=base.shape;inputs=['['+','.join(format(float(x),'.9g') for x in q)+']' for q in qs]
 data=Path('/workspace/amd-v10-'+dataset_name+'.tsv')
 if not data.exists():
  state['stage']='write_input';save()
  with data.open('w') as f:
   for i,row in enumerate(base):f.write(str(i*17+10001)+'\t['+','.join(format(float(v),'.9g') for v in row)+']\n')
 del base
 pairs=[('scored16','/workspace/amd-v10-scored16-seekdb','n16'),('compact10',str(root/'build_release/src/observer/seekdb'),'n10')]
 for kind,binary,bridge in (pairs if dataset_name=='sift' else pairs[::-1]):
  run=Path('/workspace/amd-v10-selected-'+dataset_name+'-'+kind);run.mkdir(exist_ok=True)
  env=os.environ.copy();env['LD_LIBRARY_PATH']=str(variants/bridge)+':/opt/hipvs/lib:/opt/rocm/lib'
  cmd=[binary,'--base-dir='+str(run),'--port=2885','--nodaemon','--parameter','_memory_budget=12G','--parameter','datafile_size=4G','--parameter','datafile_maxsize=8G','--parameter','log_disk_size=2G','--log-level','INFO']
  log=(run/'stdout.log').open('a');proc=subprocess.Popen(cmd,env=env,stdout=log,stderr=subprocess.STDOUT)
  state.update(stage='start',running=kind,pid=proc.pid);save()
  def connect():
   for _ in range(150):
    if proc.poll() is not None:raise RuntimeError('server exit: '+(run/'stdout.log').read_text(errors='replace')[-2500:])
    try:return pymysql.connect(host='127.0.0.1',port=2885,user='root',autocommit=True,local_infile=True,connect_timeout=2,read_timeout=180,write_timeout=180)
    except Exception:time.sleep(1)
   raise RuntimeError('server start timeout')
  try:
   conn=connect();cur=conn.cursor();cur.execute('set ob_query_timeout=300000000');cur.execute('create database if not exists amd_latest');cur.execute('use amd_latest');cur.execute('drop table if exists items');cur.execute('create table items(id bigint primary key,v vector('+str(dim)+'))')
   state['stage']='load';save();cur.execute("load data local infile %s into table items fields terminated by '\\t' lines terminated by '\\n' (id,v)",(str(data),));cur.execute('select count(*) from items');assert cur.fetchone()[0]==n
   result={'kind':kind,'niter':16 if bridge=='n16' else 10,'dataset':dataset_name+' public/proxy data, not official split','rows':n,'dim':dim,'query_count':len(qs),'queries':{}}
   state['stage']='ddl';save();t=time.perf_counter();cur.execute('create vector index idx on items(v) with (distance=l2,type=hnsw,lib=cuvs,m=16,ef_construction=200,ef_search=64)');cur.execute('select id from items order by l2_distance(v,%s) approximate limit 10',(inputs[0],));first=cur.fetchall();result['build_first_sec']=time.perf_counter()-t
   assert len(first)==10
   for k in [10,100]:
    state.update(stage='query',topk=k);save();hits=0;times=[]
    for i,q in enumerate(inputs):
     t=time.perf_counter();cur.execute('select id from items order by l2_distance(v,%s) approximate limit '+str(k),(q,));external=[int(x[0]) for x in cur.fetchall()];times.append(time.perf_counter()-t)
     ids=[(v-10001)//17 for v in external];assert len(ids)==k and len(set(ids))==k and min(ids)>=0 and max(ids)<n and all((v-10001)%17==0 for v in external)
     hits+=len(set(ids)&set(truth[i,:k].tolist()))
    result['queries'][str(k)]={'recall':hits/(len(qs)*k),'qps':len(qs)/sum(times),'p95_ms':float(np.quantile(times,.95)*1000),'ids_valid':True}
   state['results'].append(result);save()
   # A small CPU index uses its own normal SQL path.
   cur.execute('drop table if exists cpu_items');cur.execute('create table cpu_items(id bigint primary key,v vector('+str(dim)+'))');cur.execute('insert into cpu_items select * from items where id<27001');cur.execute('create vector index cpu_idx on cpu_items(v) with (distance=l2,type=hnsw,lib=vsag,m=16,ef_construction=200,ef_search=64)');cur.execute('select id from cpu_items order by l2_distance(v,%s) approximate limit 10',(inputs[0],));assert len(cur.fetchall())==10;result['vsag_sql_passed']=True;save()
   # Restart exactly our server and validate the persisted GPU graph again.
   conn.close();proc.terminate();proc.wait(timeout=90);cmd[0]=str(root/'build_release/src/observer/seekdb');proc=subprocess.Popen(cmd,env=env,stdout=log,stderr=subprocess.STDOUT);state.update(stage='restart',pid=proc.pid);save()
   conn=connect();cur=conn.cursor();cur.execute('use amd_latest');cur.execute('set ob_query_timeout=300000000');hits=0
   for i,q in enumerate(inputs[:32]):
    cur.execute('select id from items order by l2_distance(v,%s) approximate limit 100',(q,));ids=[(int(x[0])-10001)//17 for x in cur.fetchall()];assert len(ids)==100 and len(set(ids))==100;hits+=len(set(ids)&set(truth[i,:100].tolist()))
   result['restart_recall100']=hits/3200;result['restart_passed']=True;save();conn.close()
  finally:
   if proc.poll() is None:
    proc.terminate()
    try:proc.wait(timeout=90)
    except subprocess.TimeoutExpired:proc.kill();proc.wait()
   log.close()
 state['stage']='completed';save()
except Exception as e:state.update(stage='failed',error=str(e));save();traceback.print_exc()

from pathlib import Path
import ctypes as C,json,sys,time,threading,statistics,subprocess,os,traceback
import numpy as np
ROOT=Path(os.environ.get('AMD_BRIDGE_ROOT','/tmp/seekdb-cagra-validation'))
def worker():
 kind=sys.argv[2];n=int(sys.argv[3]);dim=int(sys.argv[4]);nq=128;k=10
 rng=np.random.default_rng(20261028+n+dim)
 base=rng.normal(size=(n,dim)).astype(np.float32);query=rng.normal(size=(nq,dim)).astype(np.float32)
 truth_dist=(base*base).sum(1)[None,:]+(query*query).sum(1)[:,None]-2*(query@base.T)
 truth=np.argsort(truth_dist,axis=1)[:,:k]
 dll=C.CDLL(str(ROOT/('lib'+kind+'.so')))
 fp=C.POINTER(C.c_float);up=C.POINTER(C.c_uint)
 dll.seekdb_cuvs_build.argtypes=[fp,C.c_long,C.c_long];dll.seekdb_cuvs_build.restype=C.c_void_p
 dll.seekdb_cuvs_search.argtypes=[C.c_void_p,fp,C.c_long,C.c_long,up,fp];dll.seekdb_cuvs_search.restype=C.c_int
 dll.seekdb_cuvs_free.argtypes=[C.c_void_p]
 ids=np.empty((nq,k),np.uint32);dist=np.empty((nq,k),np.float32)
 def search(qcount=nq,topk=k):
  code=dll.seekdb_cuvs_search(h,query.ctypes.data_as(fp),qcount,topk,ids.ctypes.data_as(up),dist.ctypes.data_as(fp))
  if code:raise RuntimeError('GPU search failed '+str(code))
 start=time.perf_counter();h=dll.seekdb_cuvs_build(base.ctypes.data_as(fp),n,dim)
 if not h:raise RuntimeError('GPU CAGRA build failed')
 try:
  search();build_first=time.perf_counter()-start
  assert (ids<n).all() and all(len(set(row))==k for row in ids),'Invalid neighbor IDs'
  actual=((base[ids]-query[:,None,:])**2).sum(2)
  assert np.allclose(actual,dist,rtol=2e-4,atol=2e-4),'Wrong FP32 L2 distances'
  recalls={}
  for cutoff in [10]:recalls[str(cutoff)]=sum(len(set(a[:cutoff])&set(b[:cutoff])) for a,b in zip(ids,truth))/(nq*cutoff)
  timings=[]
  for _ in range(15):
   start=time.perf_counter();search();timings.append(time.perf_counter()-start)
  # Scratch growth and shrinking sizes must stay valid, including one query.
  for qcount,topk in [(1,1),(64,10),(1,5),(128,10)]:search(qcount,topk)
  print(json.dumps({'kind':kind,'n':n,'dim':dim,'nq':nq,'k':k,'build_first_sec':build_first,
   'batch_sec':statistics.median(timings),'batch_samples':timings,'recall':recalls,'ids_and_distances_valid':True}),flush=True)
 finally:dll.seekdb_cuvs_free(h)
def main():
 if len(sys.argv)>1 and sys.argv[1]=='worker':
  failure=[]
  def run():
   try:worker()
   except BaseException as e:traceback.print_exc();failure.append(str(e))
  threading.stack_size(32*1024*1024);t=threading.Thread(target=run);t.start();t.join()
  if failure:raise SystemExit(1)
 else:
  results=[]
  for n,dim in [(2048,32),(10000,128),(50000,128)]:
   for round_id,order in enumerate([['baseline','candidate'],['candidate','baseline'],['baseline','candidate']]):
    for kind in order:
     p=subprocess.run([sys.executable,__file__,'worker',kind,str(n),str(dim)],capture_output=True,text=True,timeout=900)
     with (ROOT/'benchmark-log.txt').open('a') as f:f.write(p.stdout+'\n'+p.stderr+'\n')
     if p.returncode:raise RuntimeError(f'{kind} {n} failed: '+p.stderr[-1800:])
     lines=[line for line in p.stdout.splitlines() if line.startswith('{')];r=json.loads(lines[-1]);r['round']=round_id
     results.append(r);(ROOT/'bridge-results.json').write_text(json.dumps(results,indent=2))
     print(kind,n,round_id,'build',r['build_first_sec'],'recall',r['recall'],flush=True)
  summary=[]
  for n,dim in [(2048,32),(10000,128),(50000,128)]:
   b=[r for r in results if r['n']==n and r['kind']=='baseline'];c=[r for r in results if r['n']==n and r['kind']=='candidate']
   bm=statistics.median(r['build_first_sec'] for r in b);cm=statistics.median(r['build_first_sec'] for r in c)
   bq=statistics.median(r['batch_sec'] for r in b);cq=statistics.median(r['batch_sec'] for r in c)
   quality=all(min(r['recall'][k] for r in c)>=min(r['recall'][k] for r in b)-.005 for k in ['10'])
   summary.append({'n':n,'dim':dim,'baseline_build_first_sec':bm,'candidate_build_first_sec':cm,'build_speedup':bm/cm,
    'batch_speedup':bq/cq,'quality_gate':quality,'candidate_min_recall':{k:min(r['recall'][k] for r in c) for k in ['10']}})
  (ROOT/'bridge-summary.json').write_text(json.dumps(summary,indent=2))
  print(json.dumps(summary),flush=True)
if __name__=='__main__':main()

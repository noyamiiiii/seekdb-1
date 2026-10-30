import os
os.environ['OPENBLAS_NUM_THREADS']='4'
os.environ['OMP_NUM_THREADS']='4'
from pathlib import Path
import numpy as np,ctypes as C,time,json,statistics,sys
root=Path(__file__).parent
kind=sys.argv[1];dim=int(sys.argv[2]);n=int(os.environ.get('AMD_TEST_ROWS','10000'));nq=64
rng=np.random.default_rng(20261030+dim)
base=rng.normal(size=(n,dim)).astype('float32');queries=rng.normal(size=(nq,dim)).astype('float32')
exact=((base*base).sum(1)[None,:]+(queries*queries).sum(1)[:,None]-2*queries@base.T)
truth=np.argsort(exact,axis=1)[:,:100]
dll=C.CDLL(str(root/('libdispatch_'+kind+'.so')))
fp=C.POINTER(C.c_float);up=C.POINTER(C.c_uint);vp=C.c_void_p
for name,args,res in [('test_build',[fp,C.c_long,C.c_long],vp),('test_query',[vp,fp,C.c_long,up,fp,C.c_int],C.c_int),('test_save',[vp,C.c_char_p],C.c_int),('test_load',[C.c_char_p,C.c_long,C.c_long],vp),('test_free',[vp],C.c_int)]:
 f=getattr(dll,name);f.argtypes=args;f.restype=res
out={'kind':kind,'dim':dim,'rows':n,'query_count':nq,'real_gpu':True,'sql_test':False,'modes':{}}
def query(h,i,k,reuse):
 ids=np.empty(k,'uint32');dist=np.empty(k,'float32')
 rc=dll.test_query(h,queries[i].ctypes.data_as(fp),k,ids.ctypes.data_as(up),dist.ctypes.data_as(fp),reuse)
 if rc:raise RuntimeError('search rc='+str(rc))
 assert (ids<n).all() and len(set(ids.tolist()))==k
 actual=((base[ids]-queries[i])**2).sum(1)
 assert np.allclose(dist,actual,rtol=3e-4,atol=3e-4),'distance mismatch'
 return ids,dist
start=time.perf_counter();h=dll.test_build(base.ctypes.data_as(fp),n,dim)
if not h:raise RuntimeError('Build failed')
try:
 query(h,0,100,0);out['build_first_sec']=time.perf_counter()-start
 snap=root/(kind+'-'+str(dim)+'.snapshot');assert dll.test_save(h,str(snap).encode())==0
 before=query(h,0,100,0)
 assert dll.test_free(h)==0;h=None
 start=time.perf_counter();h=dll.test_load(str(snap).encode(),n,dim)
 if not h:raise RuntimeError('Snapshot reload failed')
 out['reload_sec']=time.perf_counter()-start
 after=query(h,0,100,0)
 assert np.array_equal(before[0],after[0]);assert np.allclose(before[1],after[1]);out['snapshot_equal']=True
 for k in [10,100]:
  reference=[query(h,i,k,0) for i in range(nq)]
  reuse_results=[query(h,i,k,1) for i in range(nq)]
  exact_ids_equal=all(np.array_equal(a[0],b[0]) for a,b in zip(reference,reuse_results))
  repeated=[query(h,i,k,0) for i in range(nq)]
  fresh_repeat_equal=all(np.array_equal(a[0],b[0]) for a,b in zip(reference,repeated))
  reuse_recall=sum(len(set(a[0].tolist())&set(t[:k].tolist())) for a,t in zip(reuse_results,truth))/(nq*k)
  recall=sum(len(set(a[0].tolist())&set(t[:k].tolist())) for a,t in zip(reference,truth))/(nq*k)
  samples={0:[],1:[]}
  ids=np.empty(k,'uint32');dist=np.empty(k,'float32')
  qp=[q.ctypes.data_as(fp) for q in queries]
  for r in range(5):
   for reuse in ([0,1] if r%2==0 else [1,0]):
    for i in range(10):assert dll.test_query(h,qp[i],k,ids.ctypes.data_as(up),dist.ctypes.data_as(fp),reuse)==0
    start=time.perf_counter()
    for i in range(256):assert dll.test_query(h,qp[i%nq],k,ids.ctypes.data_as(up),dist.ctypes.data_as(fp),reuse)==0
    samples[reuse].append((time.perf_counter()-start)/256)
  out['modes'][str(k)]={'recall':recall,'exact_ids_equal':exact_ids_equal,'fresh_repeat_equal':fresh_repeat_equal,'reuse_recall':reuse_recall,'recall_delta':reuse_recall-recall,'ids_and_distances_valid':True,'fresh_seconds':samples[0],'reuse_seconds':samples[1],'gpu_query_dispatch_speedup':statistics.median(samples[0])/statistics.median(samples[1])}
finally:
 if h:dll.test_free(h)
(root/(kind+'-'+str(dim)+'.json')).write_text(json.dumps(out,indent=2));print(json.dumps(out),flush=True)

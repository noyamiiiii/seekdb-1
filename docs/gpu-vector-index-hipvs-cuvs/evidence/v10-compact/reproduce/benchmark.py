import os
os.environ['OPENBLAS_NUM_THREADS']='4'
os.environ['OMP_NUM_THREADS']='4'
from pathlib import Path
import ctypes as C, hashlib, json, sys, time
import numpy as np
root=Path(__file__).parent
kind,dataset=sys.argv[1:3]
source=root/(dataset+'.npz')
if not source.exists():
 dim=int(dataset.removeprefix('gauss'));rng=np.random.default_rng(20261104+dim)
 base=rng.normal(size=(250000,dim)).astype('float32')
 queries=rng.normal(size=(512,dim)).astype('float32')
else:
 data=np.load(source);base=np.ascontiguousarray(data['base']);queries=np.ascontiguousarray(data['queries'])
n,dim=base.shape;nq=len(queries)
truth_path=root/(dataset+'-truth.npy')
if truth_path.exists():truth=np.load(truth_path)
else:
 truth=np.empty((nq,100),np.int64);norm=np.einsum('ij,ij->i',base,base)
 for a in range(0,nq,32):
  q=queries[a:a+32];d=norm[None,:]+np.einsum('ij,ij->i',q,q)[:,None]-2*q@base.T
  ids=np.argpartition(d,99,axis=1)[:,:100]
  order=np.argsort(np.take_along_axis(d,ids,axis=1),axis=1)
  truth[a:a+len(q)]=np.take_along_axis(ids,order,axis=1)
 np.save(truth_path,truth)
dll=C.CDLL(str(root/('libdispatch_'+kind+'.so')))
fp=C.POINTER(C.c_float);up=C.POINTER(C.c_uint);vp=C.c_void_p
for name,args,res in [('test_build',[fp,C.c_long,C.c_long],vp),('test_query',[vp,fp,C.c_long,up,fp,C.c_int],C.c_int),('test_save',[vp,C.c_char_p],C.c_int),('test_load',[C.c_char_p,C.c_long,C.c_long],vp),('test_free',[vp],C.c_int)]:
 f=getattr(dll,name);f.argtypes=args;f.restype=res
out={'kind':kind,'dataset':dataset,'rows':n,'dim':dim,'queries':nq,'sql_test':False,'real_gpu':True,'modes':{}}
reuse=0 if kind=='baseline' else 1
def query(h,i,k):
 ids=np.empty(k,'uint32');dist=np.empty(k,'float32');t=time.perf_counter()
 rc=dll.test_query(h,queries[i].ctypes.data_as(fp),k,ids.ctypes.data_as(up),dist.ctypes.data_as(fp),reuse)
 seconds=time.perf_counter()-t
 assert rc==0,('query',rc)
 assert (ids<n).all() and len(set(ids.tolist()))==k,'bad or duplicate ID'
 actual=np.einsum('ij,ij->i',base[ids]-queries[i],base[ids]-queries[i])
 assert np.isfinite(dist).all() and np.allclose(dist,actual,rtol=3e-4,atol=3e-3),'distance mismatch'
 return ids,seconds
print('BUILD',kind,dataset,flush=True)
t=time.perf_counter();h=dll.test_build(base.ctypes.data_as(fp),n,dim)
assert h,'build failed'
try:
 query(h,0,100);out['build_first_sec']=time.perf_counter()-t
 snap=root/(kind+'-'+dataset+'.snapshot');t=time.perf_counter()
 assert dll.test_save(h,str(snap).encode())==0
 out['serialize_sec']=time.perf_counter()-t;out['snapshot_bytes']=snap.stat().st_size
 digest=hashlib.sha256(snap.read_bytes()).hexdigest()
 assert dll.test_free(h)==0;h=None;t=time.perf_counter()
 h=dll.test_load(str(snap).encode(),n,dim);assert h,'load failed'
 query(h,0,100);out['reload_first_sec']=time.perf_counter()-t
 snap2=snap.with_suffix('.reload');assert dll.test_save(h,str(snap2).encode())==0
 assert digest==hashlib.sha256(snap2.read_bytes()).hexdigest(),'graph changed on reload'
 snap.unlink();snap2.unlink();out['snapshot_byte_equal']=True
 for k in [10,100]:
  hits=0;times=[]
  for i in range(min(30,nq)):query(h,i,k)
  for i in range(nq):
   ids,sec=query(h,i,k);times.append(sec);hits+=len(set(ids.tolist())&set(truth[i,:k].tolist()))
  out['modes'][str(k)]={'recall':hits/(nq*k),'query_seconds':sum(times),'qps':nq/sum(times),'p95_seconds':float(np.quantile(times,.95)),'ids_distances_valid':True}
finally:
 if h:dll.test_free(h)
(root/(kind+'-'+dataset+'.json')).write_text(json.dumps(out,indent=2))
print(json.dumps(out),flush=True)

from pathlib import Path
import ctypes as C,numpy as np,threading,time,json,subprocess,os,sys,traceback,statistics
root=Path(os.environ.get('AMD_BRIDGE_ROOT','/tmp/seekdb-cagra-validation'))
def main():
 env=os.environ.copy();env['LD_LIBRARY_PATH']='/opt/hipvs/lib:/opt/rocm/lib';env['OPENBLAS_NUM_THREADS']='4'
 cmd=['/opt/rocm/bin/hipcc','-shared','-fPIC','-O2','-pthread','-D__HIP_PLATFORM_AMD__','-I/opt/hipvs/include','-I/opt/rocm/include',str(root/'candidate-final.c'),'-L/opt/hipvs/lib','-lcuvs_c','-Wl,-rpath,/opt/hipvs/lib:/opt/rocm/lib','-o',str(root/'libcandidate-final.so')]
 compile_result=subprocess.run(cmd,capture_output=True,text=True,env=env,timeout=180)
 (root/'final-compile.txt').write_text(compile_result.stdout+compile_result.stderr)
 assert compile_result.returncode==0,compile_result.stderr
 if len(sys.argv)<2:
  p=subprocess.run([sys.executable,__file__,'test'],capture_output=True,text=True,env=env,timeout=600)
  (root/'final-test-log.txt').write_text(p.stdout+p.stderr)
  if p.returncode:raise RuntimeError(p.stderr)
  return
 failures=[]
 def run():
  try:check()
  except BaseException as e:traceback.print_exc();failures.append(str(e))
 threading.stack_size(32*1024*1024);t=threading.Thread(target=run);t.start();t.join()
 if failures:raise RuntimeError(str(failures))
def check():
 dll=C.CDLL(str(root/'libcandidate-final.so'));fp=C.POINTER(C.c_float);up=C.POINTER(C.c_uint)
 dll.seekdb_cuvs_build.argtypes=[fp,C.c_long,C.c_long];dll.seekdb_cuvs_build.restype=C.c_void_p
 dll.seekdb_cuvs_search.argtypes=[C.c_void_p,fp,C.c_long,C.c_long,up,fp];dll.seekdb_cuvs_search.restype=C.c_int
 dll.seekdb_cuvs_free.argtypes=[C.c_void_p]
 rng=np.random.default_rng(20261028);base=rng.normal(size=(10000,128)).astype('float32')
 query=(base[:128]+rng.normal(scale=.05,size=(128,128))).astype('float32')
 truth=np.argsort((base*base).sum(1)[None,:]+(query*query).sum(1)[:,None]-2*query@base.T,axis=1)[:,:100]
 assert not dll.seekdb_cuvs_build(None,100,128)
 assert not dll.seekdb_cuvs_build(base.ctypes.data_as(fp),0,128)
 assert not dll.seekdb_cuvs_build(base.ctypes.data_as(fp),10000,2**62)
 dll.seekdb_cuvs_free(None)
 h=dll.seekdb_cuvs_build(base.ctypes.data_as(fp),len(base),128);assert h
 checks=[]
 def search(nq,k):
  ids=np.empty((nq,k),np.uint32);dist=np.empty((nq,k),np.float32)
  rc=dll.seekdb_cuvs_search(h,query.ctypes.data_as(fp),nq,k,ids.ctypes.data_as(up),dist.ctypes.data_as(fp))
  assert rc==0,rc
  assert (ids<len(base)).all() and all(len(set(row))==k for row in ids)
  actual=((base[ids]-query[:nq,None,:])**2).sum(2)
  assert np.allclose(actual,dist,rtol=2e-4,atol=2e-4)
  assert np.all(dist[:,1:]>=dist[:,:-1]-1e-4)
  recall=sum(len(set(a)&set(b[:k])) for a,b in zip(ids,truth[:nq]))/(nq*k)
  return {'nq':nq,'k':k,'recall':recall}
 try:
  for nq,k in [(1,10),(64,100),(128,100),(1,1),(128,10),(32,100)]:checks.append(search(nq,k))
  dummy=np.empty(1,np.uint32)
  assert dll.seekdb_cuvs_search(h,query.ctypes.data_as(fp),1,10001,dummy.ctypes.data_as(up),None)!=0
  assert dll.seekdb_cuvs_search(h,query.ctypes.data_as(fp),2**62,100,dummy.ctypes.data_as(up),None)!=0
  assert dll.seekdb_cuvs_search(None,query.ctypes.data_as(fp),1,1,dummy.ctypes.data_as(up),None)!=0
  # Independent result arrays, one shared index: check serialized scratch reuse.
  errors=[]
  def concurrent():
   try:
    for _ in range(4):search(16,10)
   except BaseException as e:errors.append(str(e))
  threads=[threading.Thread(target=concurrent) for _ in range(4)]
  for t in threads:t.start()
  for t in threads:t.join()
  assert not errors,errors
 finally:dll.seekdb_cuvs_free(h)
 report={'passed':True,'checks':checks,'concurrent_threads':4,'concurrent_searches':16,'ids_and_fp32_distances_valid':True,
  'invalid_input_and_overflow_rejected':True,'full_server_build':False,'official_score':None}
 (root/'final-validation.json').write_text(json.dumps(report,indent=2))
 print(json.dumps(report),flush=True)
if __name__=='__main__':main()

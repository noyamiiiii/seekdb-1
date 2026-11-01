from pathlib import Path
import os,subprocess,json,ctypes as C
import numpy as np
root=Path('/workspace/seekdb-v10');lab=Path('/workspace/amd-v10-lab')
out=Path('/workspace/seekdb_hipvs_notebook/amd-v10-export-gpu.json');state={'stage':'compile','tests':[]}
def save():out.write_text(json.dumps(state,indent=2))
save()
try:
 code=(lab/'dispatch.cpp').read_text().replace('case 4:seekdb_cuvs_free', 'case 5:j->rc=seekdb_cuvs_export_dataset(j->h,const_cast<float*>(j->base),j->n,j->dim);break;\n case 4:seekdb_cuvs_free')
 code=code.replace('#include "seekdb_cuvs_bridge.h"','#include "/workspace/seekdb-v10/tools/hipvs_bridge/seekdb_cuvs_bridge.h"')
 code+='\nextern "C" int test_export(void*h,float*base,long n,long d){Job j{};j.op=5;j.h=h;j.base=base;j.n=n;j.dim=d;return dispatch(j,false);}\n'
 p=lab/'dispatch-export.cpp';p.write_text(code)
 env=os.environ.copy();env['LD_LIBRARY_PATH']=str(root)+':/opt/hipvs/lib:/opt/rocm/lib'
 cmd=['g++','-std=c++17','-shared','-fPIC','-O2','-pthread','-I'+str(root/'tools/hipvs_bridge'),'-I'+str(root/'src/oblib/lib/vector'),str(p),'-L'+str(root),'-lseekdb_cuvs_bridge','-Wl,-rpath,'+str(root)+':/opt/hipvs/lib:/opt/rocm/lib','-o',str(lab/'libexport.so')]
 p=subprocess.run(cmd,env=env,capture_output=True,text=True,timeout=120)
 if p.returncode:raise RuntimeError(p.stderr[-3000:])
 dll=C.CDLL(str(lab/'libexport.so'));fp=C.POINTER(C.c_float);vp=C.c_void_p
 for name,args,res in [('test_build',[fp,C.c_long,C.c_long],vp),('test_export',[vp,fp,C.c_long,C.c_long],C.c_int),('test_free',[vp],C.c_int),('test_save',[vp,C.c_char_p],C.c_int),('test_load',[C.c_char_p,C.c_long,C.c_long],vp)]:
  f=getattr(dll,name);f.argtypes=args;f.restype=res
 for dim in [7,127,128,129,256]:
  state.update(stage='test',dimension=dim);save();base=np.random.default_rng(171+dim).normal(size=(1000,dim)).astype('float32');copy=np.empty_like(base)
  h=dll.test_build(base.ctypes.data_as(fp),1000,dim);assert h,'GPU build'
  try:
   assert dll.test_export(h,copy.ctypes.data_as(fp),1000,dim)==0,'GPU export'
   assert np.array_equal(base.view('uint32'),copy.view('uint32')),'export byte mismatch'
   snap=lab/'export.snapshot';assert dll.test_save(h,str(snap).encode())==0
   dll.test_free(h);h=None;h=dll.test_load(str(snap).encode(),1000,dim);assert h
   assert dll.test_export(h,copy.ctypes.data_as(fp),1000,dim)==0
   assert np.array_equal(base.view('uint32'),copy.view('uint32')),'reload export byte mismatch'
   state['tests'].append({'dim':dim,'rows':1000,'fp32_bitwise_equal_before_after_reload':True});save();snap.unlink()
  finally:
   if h:dll.test_free(h)
 state['stage']='completed';save()
except Exception as e:state.update(stage='failed',error=str(e));save()

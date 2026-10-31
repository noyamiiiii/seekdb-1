from pathlib import Path
import json, os, subprocess, sys, time, ctypes as C, hashlib, traceback
root=Path(__file__).parent;out=Path('/workspace/seekdb_hipvs_notebook/amd-v10-status.json')
state={'stage':'compile','base_commit':'1230d0955f2d948f5c61e5221dde0a1c26584c53','results':[],'sql_test':False}
def save():out.write_text(json.dumps(state,indent=2))
save();env=os.environ.copy();env['LD_LIBRARY_PATH']=str(root)+':/opt/hipvs/lib:/opt/rocm/lib';env['OPENBLAS_NUM_THREADS']='4'
hip=C.CDLL('/opt/rocm/lib/libamdhip64.so');hip.hipSetDevice(0);hip.hipMemGetInfo.argtypes=[C.POINTER(C.c_size_t),C.POINTER(C.c_size_t)]
def used():
 f=C.c_size_t();t=C.c_size_t();return t.value-f.value if hip.hipMemGetInfo(C.byref(f),C.byref(t))==0 else 0
try:
 configs=[('baseline',None),('n16',16),('n12',12),('n10',10),('n8',8)]
 for name,niter in configs:
  state['running']='compile-'+name;save()
  source=root/('baseline.c' if niter is None else 'candidate.c');flags=[] if niter is None else ['-DSK_CAGRA_NITER='+str(niter)]
  commands=[['/opt/rocm/bin/hipcc','-shared','-fPIC','-O2','-pthread','-D__HIP_PLATFORM_AMD__','-I/opt/hipvs/include','-I/opt/rocm/include','-I'+str(root),str(source),*flags,'-L/opt/hipvs/lib','-lcuvs_c','-Wl,-rpath,/opt/hipvs/lib:/opt/rocm/lib','-o',str(root/('libbridge_'+name+'.so'))],['g++','-std=c++17','-O2','-shared','-fPIC','-pthread','-I'+str(root),str(root/'dispatch.cpp'),'-L'+str(root),'-lbridge_'+name,'-Wl,-rpath,'+str(root),'-o',str(root/('libdispatch_'+name+'.so'))]]
  for cmd in commands:
   p=subprocess.run(cmd,env=env,capture_output=True,text=True,timeout=180)
   if p.returncode:raise RuntimeError(name+' compile '+p.stderr[-3000:])
 state['stage']='benchmark';save()
 datasets=sys.argv[1:] or ['gauss128','gauss256']
 for dataset in datasets:
  for name,niter in configs:
   state['running']=name+'-'+dataset;save();initial=used();peak=initial
   log=(root/(name+'-'+dataset+'.log')).open('w');t=time.monotonic()
   p=subprocess.Popen(['python3',str(root/'benchmark.py'),name,dataset],env=env,stdout=log,stderr=subprocess.STDOUT)
   while p.poll() is None:
    peak=max(peak,used())
    if time.monotonic()-t>300:p.kill();p.wait();break
    time.sleep(.05)
   log.close();entry={'config':name,'niter':niter,'dataset':dataset,'exit':p.returncode,'gpu_initial':initial,'gpu_peak':peak,'gpu_increment':peak-initial}
   if p.returncode:entry['error']=(root/(name+'-'+dataset+'.log')).read_text()[-3000:]
   else:entry['measurement']=json.loads((root/(name+'-'+dataset+'.json')).read_text())
   state['results'].append(entry);save()
   if p.returncode:raise RuntimeError('GPU benchmark failed; see results')
 state['stage']='completed';save()
except BaseException as e:state['stage']='failed';state['error']=str(e);save();traceback.print_exc()

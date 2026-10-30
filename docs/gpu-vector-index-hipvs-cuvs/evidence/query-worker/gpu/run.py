from pathlib import Path
import subprocess,os,json,traceback
root=Path(__file__).parent
report=Path('/workspace/seekdb_hipvs_notebook/amd-v5-gpu-status.json')
state={'stage':'compile','results':[]}
def save():report.write_text(json.dumps(state,indent=2))
save();env=os.environ.copy();env['LD_LIBRARY_PATH']=str(root)+':/opt/hipvs/lib:/opt/rocm/lib';env['OPENBLAS_NUM_THREADS']='4'
try:
 for kind in ['baseline','candidate']:
  cmd=['/opt/rocm/bin/hipcc','-shared','-fPIC','-O2','-pthread','-D__HIP_PLATFORM_AMD__','-I/opt/hipvs/include','-I/opt/rocm/include','-I'+str(root),str(root/(kind+'.c')),'-L/opt/hipvs/lib','-lcuvs_c','-Wl,-rpath,/opt/hipvs/lib:/opt/rocm/lib','-o',str(root/('libbridge_'+kind+'.so'))]
  p=subprocess.run(cmd,env=env,capture_output=True,text=True,timeout=180);(root/(kind+'-build.txt')).write_text(p.stdout+p.stderr)
  if p.returncode:raise RuntimeError(kind+' bridge compile: '+p.stderr[-3500:])
  cmd=['g++','-std=c++17','-O2','-shared','-fPIC','-pthread','-I'+str(root),str(root/'dispatch.cpp'),'-L'+str(root),'-lbridge_'+kind,'-Wl,-rpath,'+str(root),'-o',str(root/('libdispatch_'+kind+'.so'))]
  p=subprocess.run(cmd,env=env,capture_output=True,text=True,timeout=120)
  if p.returncode:raise RuntimeError(kind+' dispatch compile: '+p.stderr[-3500:])
 state['stage']='gpu_benchmark';save()
 for dim in [128,256]:
  for kind in ['baseline','candidate']:
   state['running']=kind+'-'+str(dim);save()
   p=subprocess.run(['python3',str(root/'benchmark.py'),kind,str(dim)],env=env,capture_output=True,text=True,timeout=600)
   (root/(kind+'-'+str(dim)+'.log')).write_text(p.stdout+p.stderr)
   if p.returncode:raise RuntimeError(kind+' benchmark: '+p.stderr[-3500:])
   state['results'].append(json.loads((root/(kind+'-'+str(dim)+'.json')).read_text()));save()
 state['stage']='completed';save()
except BaseException as e:
 state['stage']='failed';state['error']=str(e);save();traceback.print_exc()

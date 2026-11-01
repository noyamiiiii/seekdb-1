from pathlib import Path
import subprocess,os,json,time
root=Path('/workspace/amd-v10-lab');folder=Path('/workspace/seekdb_hipvs_notebook');out=folder/'amd-v10-phase6-status.json';state={'stage':'waiting','results':[]}
def save():out.write_text(json.dumps(state,indent=2))
save()
try:
 # Keep the final SQL timing isolated from the search sweep.
 for _ in range(600):
  p=folder/'amd-v10-final-gauss256-status.json'
  if p.exists() and json.loads(p.read_text())['stage'] in ['completed','failed']:break
  time.sleep(2)
 else:raise RuntimeError('SQL run did not finish')
 env=os.environ.copy();env['LD_LIBRARY_PATH']=str(root)+':/opt/hipvs/lib:/opt/rocm/lib';env['OPENBLAS_NUM_THREADS']='4'
 for r,w in [(20,16)]:
  name='n8w'+str(w)+'r'+str(r);state.update(stage='compile',running=name);save()
  s=(root/'candidate.c').read_text().replace('h->sp->search_width = 4;',f'h->sp->search_width = {w}; h->sp->num_random_samplings = {r};')
  p=root/(name+'.c');p.write_text(s)
  commands=[['/opt/rocm/bin/hipcc','-shared','-fPIC','-O2','-pthread','-D__HIP_PLATFORM_AMD__','-DSK_CAGRA_NITER=8','-I/opt/hipvs/include','-I/opt/rocm/include','-I'+str(root),str(p),'-L/opt/hipvs/lib','-lcuvs_c','-Wl,-rpath,/opt/hipvs/lib:/opt/rocm/lib','-o',str(root/('libbridge_'+name+'.so'))],['g++','-std=c++17','-O2','-shared','-fPIC','-pthread','-I'+str(root),str(root/'dispatch.cpp'),'-L'+str(root),'-lbridge_'+name,'-Wl,-rpath,'+str(root),'-o',str(root/('libdispatch_'+name+'.so'))]]
  for cmd in commands:
   p=subprocess.run(cmd,env=env,capture_output=True,text=True,timeout=180)
   if p.returncode:raise RuntimeError(p.stderr[-3000:])
  for dataset in ['gauss256','gauss128','sift512']:
   state.update(stage='benchmark',running=name+'-'+dataset);save()
   p=subprocess.run(['python3',str(root/'benchmark.py'),name,dataset],env=env,capture_output=True,text=True,timeout=300)
   entry={'config':name,'random_samplings':r,'dataset':dataset,'exit':p.returncode}
   if p.returncode:entry['error']=(p.stdout+p.stderr)[-2000:]
   else:entry['measurement']=json.loads((root/(name+'-'+dataset+'.json')).read_text())
   state['results'].append(entry);save()
   if p.returncode:break
 state['stage']='completed';save()
except Exception as e:state.update(stage='failed',error=str(e));save()

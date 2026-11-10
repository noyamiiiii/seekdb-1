#!/usr/bin/env python3
"""Execute the official JSON assertions against the actual stdio process."""
import sys,subprocess,tempfile,pathlib,json,math,re,time
class Client:
 def __init__(self,binary,data,args=()):
  self.p=subprocess.Popen([str(binary),'--data-dir',str(data),*args],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,encoding='utf-8',bufsize=1)
  self.sel=None
  self.seq=0;assert json.loads(self.line())=={'type':'ready'},'missing readiness'
 def line(self):
  # The driver reads a bounded response synchronously; CI applies an outer timeout.
  s=self.p.stdout.readline()
  if not s:raise RuntimeError(f'engine exited {self.p.poll()}')
  return s.rstrip('\r\n')
 def sql(self,sql):
  self.seq+=1
  self.p.stdin.write(json.dumps({'id':self.seq,'op':'execute','sql':sql,'search_mode':'exact'},ensure_ascii=False)+'\n');self.p.stdin.flush()
  response=json.loads(self.line());assert response.get('id')==self.seq,response
  assert type(response.get('ok')) is bool,response
  return response
 def close(self,kill=False):
  if self.p.poll() is None:
   if kill:self.p.kill()
   else:
    self.seq+=1;self.p.stdin.write(json.dumps({'id':self.seq,'op':'shutdown'})+'\n');self.p.stdin.flush()
    assert json.loads(self.line())=={'id':self.seq,'ok':True}
    self.p.stdin.close()
   self.p.wait(timeout=15)
 def __del__(self):
  if hasattr(self,'p') and self.p.poll() is None:self.p.kill()
def same(a,b):
 if isinstance(b,int):
  try:return int(a)==b
  except ValueError:return False
 if isinstance(b,float):
  try:return math.isclose(float(a),b,rel_tol=1e-5,abs_tol=1e-5)
  except ValueError:return False
 if isinstance(b,list):
  try:return all(same(x,y) for x,y in zip(a if isinstance(a,list) else json.loads(a),b)) and len(a if isinstance(a,list) else json.loads(a))==len(b)
  except (ValueError,TypeError):return False
 return a==b
def check(actual,expected):
 if 'error' in expected:assert actual.get('error'),(actual,expected);return
 assert not actual.get('error'),actual
 for k in ['affected_rows','columns']:
  if k in expected:assert actual.get(k)==expected[k],(actual,expected)
 if 'rows' in expected:
  rows=actual.get('rows',[]);want=expected['rows']
  assert len(rows)==len(want),(actual,expected)
  if expected.get('unordered'):
   remaining=list(rows)
   for b in want:
    for i,a in enumerate(remaining):
     if len(a)==len(b) and all(same(x,y) for x,y in zip(a,b)):remaining.pop(i);break
    else:raise AssertionError((rows,want))
  else:
   assert all(len(a)==len(b) and all(same(x,y) for x,y in zip(a,b)) for a,b in zip(rows,want)),(actual,expected)
def run(binary,spec):
 manifest=json.loads((spec/'manifest.json').read_text(encoding='utf-8'));passed=[];errors=[];assertions=0
 for case in manifest['cases']:
  with tempfile.TemporaryDirectory(prefix='mvdb-official-') as data:
   client=Client(binary,data);n=0
   try:
    expected=json.loads((spec/case['result']).read_text(encoding='utf-8'))
    for sql in (spec/case['test']).read_text(encoding='utf-8').splitlines():
     sql=sql.strip()
     if not sql:continue
     if sql=='--restart':client.close();client=Client(binary,data);continue
     if sql.startswith('--mode '):check(client.sql("SET vector_mode = '"+sql.split()[1]+"';"),{});continue
     if sql.startswith('--'):continue
     check(client.sql(sql),expected[n]);n+=1;assertions+=1
    assert n==len(expected),(n,len(expected))
    passed.append(case['id']);print('PASS',case['id'],n,flush=True)
   except Exception as e:errors.append({'case':case['id'],'assertion':n+1,'error':repr(e)});print('FAIL',errors[-1],flush=True)
   finally:client.close()
 summary={'cases_passed':len(passed),'cases_total':len(manifest['cases']),'assertions_passed':assertions,'failures':errors,'official_spec_version':manifest['version']}
 print(json.dumps(summary,ensure_ascii=False));return summary
if __name__=='__main__':
 result=run(pathlib.Path(sys.argv[1]).resolve(),pathlib.Path(sys.argv[2]).resolve());sys.exit(bool(result['failures']))

#!/usr/bin/env python3
import sys,pathlib,tempfile,subprocess,json,random,math,struct,time,resource,os
from official_cases import Client,check
binary=pathlib.Path(sys.argv[1]).resolve();rng=random.Random(20261109);results=[]
def ok(c,s):r=c.sql(s);assert 'error' not in r,(s[:100],r);return r
def ids(c,s):return [int(x[0]) for x in ok(c,s)['rows']]
# Independent Python model, mixed transactions, deterministic random vectors,
# exact top-k and filtering after insert/update/delete/rollback/restart.
with tempfile.TemporaryDirectory(prefix='mvdb-random-') as d:
 c=Client(binary,d);ok(c,'CREATE TABLE t(id BIGINT PRIMARY KEY, category BIGINT, v VECTOR(13));');state={};snap=None;queries=0
 f32=lambda x:struct.unpack('<f',struct.pack('<f',x))[0]
 for step in range(700):
  op=rng.randrange(10);pk=rng.randrange(120)
  if op<3:
   v=[f32(rng.uniform(-2,2)) for _ in range(13)];cat=rng.randrange(5);s=f"INSERT INTO t VALUES({pk},{cat},'{json.dumps(v)}');";r=c.sql(s)
   if pk in state:assert 'error' in r
   else:assert 'error' not in r;state[pk]=(cat,v)
  elif op==3:
   ok(c,f'DELETE FROM t WHERE id={pk};');state.pop(pk,None)
  elif op==4:
   v=[f32(rng.uniform(-2,2)) for _ in range(13)];ok(c,f"UPDATE t SET v='{json.dumps(v)}' WHERE id={pk};")
   if pk in state:state[pk]=(state[pk][0],v)
  elif op==5 and snap is None:ok(c,'BEGIN;');snap=dict(state)
  elif op==6 and snap is not None:
   if rng.randrange(2):ok(c,'COMMIT;')
   else:ok(c,'ROLLBACK;');state=snap
   snap=None
  elif op==7 and snap is None:
   c.close(kill=True);c=Client(binary,d)
  else:
   q=[f32(rng.uniform(-2,2)) for _ in range(13)];cat=rng.choice([None,0,1,2,3,4,9]);k=rng.randrange(1,20)
   eligible=[(sum((a-b)**2 for a,b in zip(v,q)),pk) for pk,(g,v) in state.items() if cat is None or cat==g]
   expected=[pk for _,pk in sorted(eligible)[:k]]
   where='' if cat is None else f' WHERE category={cat}'
   assert ids(c,f"SELECT id FROM t{where} ORDER BY L2_DISTANCE(v,'{json.dumps(q)}') LIMIT {k};")==expected,(step,expected)
   queries+=1
  if step%37==0:assert sorted(ids(c,'SELECT id FROM t;'))==sorted(state)
 c.close();results.append({'test':'random_model_700_steps','queries':queries,'pass':True})
# Exceed a 96MiB virtual address-space budget using 160MiB of text records.
# Neither startup, bulk transaction, rollback, commit nor recovery may load it all.
with tempfile.TemporaryDirectory(prefix='mvdb-memory-') as d:
 wrapper=pathlib.Path(d)/'limit.py';wrapper.write_text('import os,resource,sys\nresource.setrlimit(resource.RLIMIT_AS,(96*1024**2,96*1024**2))\nos.execv(sys.argv[1],sys.argv[1:])\n')
 launcher=pathlib.Path(d)/'limited';launcher.write_text('#!/bin/sh\nexec python3 "'+str(wrapper)+'" "'+str(binary)+'" "$@"\n');launcher.chmod(0o755)
 c=Client(launcher,d);ok(c,'CREATE TABLE large(id BIGINT PRIMARY KEY, body VARCHAR(2000000));');ok(c,'BEGIN;');text='x'*1048576
 started=time.monotonic()
 for i in range(160):ok(c,f"INSERT INTO large VALUES({i},'{text}');")
 assert ids(c,'SELECT id FROM large ORDER BY id DESC LIMIT 2;')==[159,158]
 ok(c,'COMMIT;');c.close(kill=True);c=Client(launcher,d)
 assert ids(c,'SELECT id FROM large ORDER BY id DESC LIMIT 2;')==[159,158]
 ok(c,'BEGIN;');ok(c,'DELETE FROM large WHERE id<80;');ok(c,'ROLLBACK;');assert ids(c,'SELECT id FROM large ORDER BY id LIMIT 1;')==[0]
 c.close();size=sum(p.stat().st_size for p in pathlib.Path(d).glob('*.heap'))
 results.append({'test':'data_exceeds_address_space','data_bytes':size,'address_space_limit':96*1024**2,'rows':160,'elapsed_sec':round(time.monotonic()-started,3),'pass':True})
# Recovery must repair an incomplete final heap page using committed WAL.
with tempfile.TemporaryDirectory(prefix='mvdb-torn-') as d:
 c=Client(binary,d);ok(c,'CREATE TABLE t(id BIGINT PRIMARY KEY,s VARCHAR(100));');ok(c,"INSERT INTO t VALUES(1,'committed');");c.close(kill=True)
 heap=pathlib.Path(d)/'t.heap'
 with heap.open('r+b') as f:f.truncate(13)
 c=Client(binary,d);assert ids(c,'SELECT id FROM t;')==[1];c.close();results.append({'test':'repair_torn_heap_before_index_rebuild','pass':True})
# Fail before/during recovery repeatedly with the original committed WAL retained.
with tempfile.TemporaryDirectory(prefix='mvdb-recovery-') as d:
 c=Client(binary,d);ok(c,'CREATE TABLE t(id BIGINT PRIMARY KEY);');ok(c,'BEGIN;')
 for b in range(50):ok(c,'INSERT INTO t VALUES '+','.join(f'({b*100+i})' for i in range(100))+';')
 ok(c,'COMMIT;');c.close(kill=True)
 for delay in [0,0.0002,0.0005,0.001,0.003,0.01]:
  p=subprocess.Popen([str(binary),'--data-dir',d],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL);time.sleep(delay);p.kill();p.wait()
 c=Client(binary,d);assert ids(c,'SELECT id FROM t ORDER BY id DESC LIMIT 1;')==[4999];assert len(ids(c,'SELECT id FROM t;'))==5000;c.close();results.append({'test':'repeated_sigkill_during_recovery','kills':6,'committed_rows':5000,'pass':True})
print(json.dumps({'results':results,'all_pass':True},indent=2),flush=True)

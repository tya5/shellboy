"""Synthetic SQL-only planner/timing evidence. No owner/API/user data access."""
import json, pathlib, sqlite3, tempfile, time, statistics, re, platform
REPO=pathlib.Path(__file__).resolve().parent.parent
out={'sqlite_version':sqlite3.sqlite_version,'python':platform.python_version(),'scope':'Python sqlite3, exact production SELECTs only; no Control/wait graph/ThreadSummary decoding, DB-worker queue or filesystem sync timing','fixture':{'threads':1000,'runs':10000,'shell_executions':50000,'live_runs':100,'active_shells':100},'queries':{}}
with tempfile.TemporaryDirectory(prefix='shellboy-issue26-') as temp:
 db=sqlite3.connect(str(pathlib.Path(temp)/'fixture.db'))
 db.create_function('octet_length',1,lambda x: None if x is None else len(x.encode() if isinstance(x,str) else x),deterministic=True)
 for migration in sorted((REPO/'migrations').glob('*.sql')):
  if int(migration.name[:4]) <= 9: db.executescript(migration.read_text())
 db.executemany("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?,?,'/','/AGENTS.md','fixture','fixture',?)",[(f't{i:04}',f'synthetic {i}',f'{i:08}') for i in range(1000)])
 runs=[];turns=[];shells=[]
 for i in range(10000):
  live=i>=9900
  runs.append((f'r{i:05}',f't{i%1000:04}','live' if live else 'completed',f'{i:08}'))
  turns.append((f'u{i:05}',f't{i%1000:04}',f'r{i:05}','running' if live else 'completed',f'{i:08}'))
 db.executemany('INSERT INTO run(id,thread_id,state,created_at)VALUES(?,?,?,?)',runs)
 db.executemany('INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?,?,?,?,?)',turns)
 for i in range(50000):
  active=i>=49900
  shells.append((f's{i:05}',f'r{i%10000:05}',f'u{i%10000:05}',f's{i:05}','running' if active else 'exited',f'{i:08}',None if active else f'{i:08}'))
 db.executemany("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at)VALUES(?,?,?,?,?,'/','{}','UTF-8','UTF-8','closed',?,?)",shells)
 db.executemany("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES(?,0,'stdout',0,x'78','now')",[(f's{i:05}',) for i in range(0,49900,4)])
 db.executemany('INSERT INTO app_meta(key,value)VALUES(?,?)',[(f'index.auto.{i:05}',json.dumps({'state':'running' if i%100==0 else 'done','node_id':str(i)})) for i in range(10000)])
 db.commit()
 wait=(REPO/'src/control/wait.rs').read_text()
 gc=(REPO/'src/gc.rs').read_text()
 thread=(REPO/'src/control/db.rs').read_text()
 queries={
  'wait_runs':("SELECT id,owner_run_id FROM run WHERE state IN ('live','stopping')",()),
  'wait_turns':("SELECT id,run_id FROM turn WHERE state IN ('preparing','running','waiting','recovery_wait')",()),
  'wait_shells':("SELECT id,owner_run_id,turn_id FROM shell_execution WHERE state IN ('starting','running','stopping')",()),
  'owned_active_shells':("SELECT count(*) FROM shell_execution WHERE owner_run_id=?1 AND state IN ('starting','running','stopping')",('r09999',)),
  'thread_live':("SELECT id FROM run WHERE thread_id=?1 AND state='live'",('t0999',)),
  'thread_live_existing_predicate':("SELECT id FROM run WHERE thread_id=?1 AND state IN ('live','stopping') AND state='live'",('t0999',)),
  'thread_page':(re.search(r'connection.prepare\("(SELECT id,title,cwd,parent_thread_id,updated_at,.*?)"\)',thread).group(1),(1000,'','',51)),
  'raw_gc_page':(re.search(r'db.prepare\("(SELECT s.id,s.state,s.ended_at.*?)"\)',gc).group(1),('00020000',0)),
  'auto_authorization':("SELECT key,value FROM app_meta WHERE key LIKE 'index.auto.%' AND json_extract(value,'$.state')='running'",()),
  'auto_authorization_existing_predicate':("SELECT key,value FROM app_meta WHERE key LIKE 'index.auto.%' AND json_valid(value) AND json_extract(value,'$.state')='running'",()),
 }
 def observe(sql,args,repetitions=9):
  plan=[r[3] for r in db.execute('EXPLAIN QUERY PLAN '+sql,args)]
  samples=[];result=None
  for i in range(repetitions):
   start=time.perf_counter_ns(); rows=db.execute(sql,args).fetchall(); samples.append((time.perf_counter_ns()-start)/1000)
   if result is None: result=rows
   assert rows==result
  return {'plan':plan,'p50_us':statistics.median(samples),'min_us':min(samples),'max_us':max(samples),'rows':len(result)},result
 before={}
 for name,(sql,args) in queries.items():
  obs,rows=observe(sql,args);before[name]=rows;out['queries'][name]={'sql':sql,'args':args,'before':obs}
 # Candidate indexes are applied only to this disposable DB, not product sources.
 indexes=["CREATE INDEX candidate_shell_active ON shell_execution(owner_run_id,state) WHERE state IN ('starting','running','stopping')", "CREATE INDEX candidate_run_thread_created ON run(thread_id,created_at DESC,id DESC)", "CREATE INDEX candidate_shell_terminal_time ON shell_execution(COALESCE(ended_at,started_at)) WHERE state IN ('exited','failed','stopped')"]
 out['candidate_indexes']=indexes
 for ddl in indexes: db.execute(ddl)
 for name,(sql,args) in queries.items():
  obs,rows=observe(sql,args);assert rows==before[name];out['queries'][name]['after']=obs
 out['combined_wait_sql_before_p50_us']=sum(out['queries'][k]['before']['p50_us'] for k in ('wait_runs','wait_turns','wait_shells'))
 out['combined_wait_sql_after_p50_us']=sum(out['queries'][k]['after']['p50_us'] for k in ('wait_runs','wait_turns','wait_shells'))
 # Independent RAG has its own schema; no vec module or embedding API is used.
 db.executescript('CREATE TABLE rag_chunks(hash TEXT NOT NULL); CREATE TABLE rag_embeddings(space_id TEXT NOT NULL,hash TEXT NOT NULL,vec_rowid INTEGER NOT NULL,PRIMARY KEY(space_id,hash));')
 db.executemany('INSERT INTO rag_chunks VALUES(?)',[(str(i),) for i in range(50000)])
 db.executemany('INSERT INTO rag_embeddings VALUES(?,?,?)',[(f'space{i%4}',str(i),i) for i in range(50000)])
 rag={'rag_next_vec':("SELECT COALESCE(MAX(vec_rowid),0)+1 FROM rag_embeddings WHERE space_id=?1",('space0',)), 'rag_orphans':("SELECT vec_rowid FROM rag_embeddings e WHERE space_id=?1 AND NOT EXISTS(SELECT 1 FROM rag_chunks c WHERE c.hash=e.hash)",('space0',))}
 for name,(sql,args) in rag.items():
  obs,rows=observe(sql,args,1);before[name]=rows;out['queries'][name]={'sql':sql,'args':args,'before':obs}
 db.executescript('CREATE INDEX candidate_rag_vec ON rag_embeddings(space_id,vec_rowid);CREATE INDEX candidate_rag_hash ON rag_chunks(hash);')
 for name,(sql,args) in rag.items():
  obs,rows=observe(sql,args);assert rows==before[name];out['queries'][name]['after']=obs
print(json.dumps(out,ensure_ascii=False,indent=2))

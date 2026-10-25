#!/usr/bin/env python3
"""Public CLI extraction smoke: anonymous fixtures, no API/owner/user DB."""
import argparse, hashlib, json, os, pathlib, subprocess, sys
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--binary',required=True,type=pathlib.Path)
p.add_argument('--extractor',required=True,type=pathlib.Path)
p.add_argument('--root',required=True,type=pathlib.Path,help='fresh private output directory')
a=p.parse_args();binary=a.binary.resolve(strict=True);backend=a.extractor.resolve(strict=True)
root=a.root.resolve();root.mkdir(parents=True,exist_ok=False)
fixtures=root/'fixtures';database=root/'store'/'rag.db';data=root/'data'
repo=pathlib.Path(__file__).resolve().parent.parent
subprocess.run([sys.executable,str(repo/'tests/fixtures/rag/generate.py'),str(fixtures)],check=True,capture_output=True)
env={k:v for k,v in os.environ.items() if not k.startswith('SHELLBOY_')}
base=[str(binary),'--data-dir',str(data),'--config',str(root/'intentionally-absent.toml'),'rag','--db',str(database)]
evidence=[]
def call(*args,ok=True):
 r=subprocess.run(base+list(args),cwd=root,env=env,capture_output=True,text=True,timeout=180)
 try:j=json.loads(r.stdout)
 except Exception:raise AssertionError(f'non-JSON CLI reply: {args}, exit {r.returncode}') from None
 assert j.get('ok') is ok and ((r.returncode==0) is ok),(args,r.returncode,j)
 evidence.append({'args':list(args),'ok':j['ok']})
 return j.get('result',j)
call('init')
source=call('source','add',str(fixtures),'--name','anonymous-formats')['source_id']
index=['index','--source',source,'--extractor',str(backend)]
dry=call(*index,'--dry-run');assert dry['processed']==0
assert call('status')['documents']==0
assert not database.with_name('shellboy.db').exists()
result=call(*index)
status=call('status');assert status['documents']==5,status
refs={};checks=[]
for extension,marker,count,unit in [('md','SHELLBOY_MARKDOWN_V1',1,'document'),('pdf','SHELLBOY_PDF_V1',1,'page'),('docx','SHELLBOY_DOCX_V1',1,'document_block'),('pptx','SHELLBOY_PPTX_',2,'slide'),('xlsx','SHELLBOY_XLSX_',2,'sheet')]:
 results=call('search','--query',marker,'--scope',f'source:{source}')['items']
 assert len(results)==count,(extension,results)
 for hit in results:
  assert hit['location']['coordinate']=='extracted_utf8_bytes'
  assert hit['location']['source']['unit']==unit,(extension,hit)
  original=call('get','--ref',hit['reference'])
  assert marker in original['text'],(extension,original)
  if extension=='xlsx':assert hit['location']['source']['name'] in ['資料一','資料二']
  refs.setdefault(extension,[]).append((hit['reference'],original['text']))
 checks.append({'format':extension,'hits':len(results),'location':unit})
unchanged=call(*index);assert unchanged['processed']==0 and unchanged['unchanged']==5,unchanged
# Changing current original must not change a saved result, before or after sync.
markdown=fixtures/'synthetic.md';markdown.write_text('# Changed\nSHELLBOY_MARKDOWN_V2 別の保存版。\n')
assert call('get','--ref',refs['md'][0][0])['text']==refs['md'][0][1]
call(*index)
assert call('get','--ref',refs['md'][0][0])['text']==refs['md'][0][1]
assert not call('search','--query','SHELLBOY_MARKDOWN_V1','--scope',f'source:{source}')['items']
assert call('search','--query','SHELLBOY_MARKDOWN_V2','--scope',f'source:{source}')['items']
# Third-party adapter uses the same public index/search/get boundary.
custom=root/'custom';custom.mkdir();(custom/'sample.custom').write_text('anonymous original')
adapter=root/'adapter.py';adapter.write_text('#!/usr/bin/env python3\nimport json,sys\nassert len(sys.argv)==2\nprint(json.dumps({"blocks":[{"text":"SHELLBOY_ADAPTER_V1 外部抽出","location":{"unit":"figure","number":2}}]}))\n');adapter.chmod(0o700)
manifest=root/'adapter.json';manifest.write_text(json.dumps({'schema_version':1,'id':'anonymous','version':'1','executable':str(pathlib.Path(sys.executable).resolve()),'argv':[str(adapter),'{input}'],'extensions':['custom']}))
custom_id=call('source','add',str(custom),'--name','anonymous-adapter')['source_id']
call('index','--source',custom_id,'--adapter',str(manifest))
hit=call('search','--query','SHELLBOY_ADAPTER_V1','--scope',f'source:{custom_id}')['items'][0]
assert hit['location']['source']=={'unit':'figure','number':2}
assert 'SHELLBOY_ADAPTER_V1' in call('get','--ref',hit['reference'])['text']
# A failed new PDF extraction must keep its previously published revision.
pdf=fixtures/'synthetic.pdf'; saved_pdf=pdf.read_bytes();pdf.write_bytes(b'corrupt synthetic PDF')
failed=call(*index,ok=False);assert failed['partial_failure'] and failed['failures']
stale=call('search','--query','SHELLBOY_PDF_V1','--scope',f'source:{source}')['items'][0]
assert stale['provenance']['state']=='stale'
assert call('get','--ref',refs['pdf'][0][0])['text']==refs['pdf'][0][1]
pdf.write_bytes(saved_pdf);call(*index)
# Explicitly invalid config never mattered to any keyword-only call.
assert not (data/'shellboy.db').exists(),'must not create or start a conversation owner'
summary={'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'backend_sha256':hashlib.sha256(backend.read_bytes()).hexdigest(),'formats':checks,'third_party_adapter':True,'saved_revision_after_original_change':True,'previous_revision_retained_on_extraction_failure':True,'owner_database_created':False,'api_configuration_required':False,'operations':len(evidence),'dry_run':dry,'initial_index':result,'unchanged_index':unchanged}
(root/'summary.json').write_text(json.dumps(summary,ensure_ascii=False,indent=2)+'\n')
(root/'operations.json').write_text(json.dumps(evidence,ensure_ascii=False,indent=2)+'\n')
print(json.dumps(summary,ensure_ascii=False,indent=2))

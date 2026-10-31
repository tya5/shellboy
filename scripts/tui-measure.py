#!/usr/bin/env python3
"""CI-only, source-witnessed native P2/P5 measurements (performance is record-only)."""
import argparse
import codecs
import collections
import difflib
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import re
import shlex
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time

HERE = Path(__file__).resolve().parent
VERSION = 'tui-measure-v1'
CAPTURE_LIMIT = 64 * 1024 * 1024
RAW_LITERAL = re.compile(r'(?:b)?r(#+)?"')
CHAR_LITERAL = re.compile(r"(?:b)?'(?:\\.|[^'\\])'")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + '\n')


def checked(args, cwd):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def source_witness(subject):
    # No caller-supplied hash is accepted as evidence.
    head = checked(['git', 'rev-parse', 'HEAD'], subject)
    if not re.fullmatch('[0-9a-f]{40,64}', head):
        raise ValueError('invalid Git HEAD')
    if checked(['git', 'status', '--porcelain', '--untracked-files=no'], subject):
        raise ValueError('subject tracked tree is dirty before instrumentation')
    names = subprocess.check_output(['git', 'ls-files', '-z'], cwd=subject).decode().split('\0')
    return {'head': head, 'tree': checked(['git', 'rev-parse', 'HEAD^{tree}'], subject),
            'files': {name: digest((subject / name).read_bytes()) for name in names if name}}


def harness_witness():
    return digest(b''.join(path.read_bytes() for path in [Path(__file__),
        HERE / 'measurement/tui_hooks.rs', HERE / 'measurement/requirements.txt']))


def rust_mask(source):
    """Remove comments/literals, preserving byte offsets for audited call rewrites."""
    out = list(source)
    i = 0
    while i < len(source):
        start = i
        if source.startswith('//', i):
            end = source.find('\n', i)
            i = len(source) if end < 0 else end
        elif source.startswith('/*', i):
            depth = 1
            i += 2
            while depth and i < len(source):
                if source.startswith('/*', i): depth += 1; i += 2
                elif source.startswith('*/', i): depth -= 1; i += 2
                else: i += 1
            if depth: raise ValueError('unterminated Rust comment')
        else:
            raw = RAW_LITERAL.match(source, i)
            char = CHAR_LITERAL.match(source, i)
            if raw:
                suffix = '"' + (raw.group(1) or '')
                i = source.index(suffix, i + len(raw.group(0))) + len(suffix)
            elif source[i] == '"':
                i += 1
                while i < len(source) and source[i] != '"':
                    i += 2 if source[i] == '\\' else 1
                i += 1
            elif char: i += len(char.group(0))
            else: i += 1; continue
        out[start:i] = [' ' if c != '\n' else '\n' for c in source[start:i]]
    return ''.join(out)


def closing(mask, start, left='(', right=')'):
    depth = 1
    for i in range(start, len(mask)):
        if mask[i] == left: depth += 1
        elif mask[i] == right:
            depth -= 1
            if not depth: return i
    raise ValueError('unbalanced Rust delimiters')


def production_mask(source):
    mask = rust_mask(source)
    out = list(mask)
    for match in re.finditer(r'#\s*\[cfg\(test\)\]\s*(?:pub\s+)?mod\s+\w+\s*\{', mask):
        end = closing(mask, match.end(), '{', '}')
        out[match.start():end+1] = ' ' * (end+1-match.start())
    return ''.join(out)


def instrument(subject, source, witness):
    before = {}
    changes = {}
    sites = []
    def read(name):
        if name not in before: before[name] = (subject/name).read_text()
        return changes.get(name, before[name])
    def replace(name, needle, new, count=1):
        text = read(name)
        if text.count(needle) != count:
            raise ValueError(f'unsupported source hook {name}: {needle!r} count {text.count(needle)}')
        changes[name] = text.replace(needle, new)
    changes['src/lib.rs'] = read('src/lib.rs') + '\n// CI instrumented subject only; never release.\npub(crate) mod ci_tui_measure;\n'
    template = (HERE/'measurement/tui_hooks.rs').read_text()
    changes['src/ci_tui_measure.rs'] = template.replace('SOURCE_WITNESS', source['head']).replace('HARNESS_WITNESS', witness)
    before['src/ci_tui_measure.rs'] = ''
    replace('src/ui/redraw.rs', '    terminal.draw(|frame| {', '    crate::ci_tui_measure::draw();\n    terminal.draw(|frame| {')
    replace('src/ui.rs', '    let terminal = &mut session.terminal;', '    let _ci_flush = crate::ci_tui_measure::FlushOnDrop;\n    let terminal = &mut session.terminal;')
    replace('src/ui.rs', 'let actions_to_send = state.event(event::read()?);', 'let ci_event=event::read()?;\n                let actions_to_send=if crate::ci_tui_measure::key(&ci_event){Vec::new()}else{state.event(ci_event)};')
    replace('src/frontend/admission.rs', '_=future=>{}', '_=crate::ci_tui_measure::scope(future)=>{}')
    replace('src/frontend/admission.rs', '        future.await;', '        crate::ci_tui_measure::scope(future).await;')
    # First spawn belongs to spawn_read; second is mutation work, not read admission.
    text = read('src/frontend/admission.rs')
    needle = '    workers.spawn(async move {'
    if text.count(needle) != 2: raise ValueError('unsupported admission layout')
    changes['src/frontend/admission.rs'] = text.replace(needle, '    crate::ci_tui_measure::ui_read_admitted();\n'+needle, 1)
    text = read('src/frontend.rs'); mask = rust_mask(text)
    starts = list(re.finditer(r'read_workers\.spawn\(', mask))
    if not starts: raise ValueError('no frontend read task hooks')
    for match in reversed(starts):
        end = closing(mask, match.end())
        text = text[:match.start()]+'{crate::ci_tui_measure::ui_read_admitted();read_workers.spawn(crate::ci_tui_measure::scope('+text[match.end():end]+'))}'+text[end+1:]
    changes['src/frontend.rs'] = text
    needle = '    let (sender, receiver) = oneshot::channel();\n    let job = Box::new(move |connection: &mut Connection| {'
    replace('src/storage.rs', needle, '    let ci_category=crate::ci_tui_measure::category::<F>();\n'+needle+'\n        let _ci_job=crate::ci_tui_measure::executing(ci_category,std::any::type_name::<F>());')
    replace('src/storage.rs', '            permit.send(command);', '            permit.send(command);\n            crate::ci_tui_measure::admitted(crate::ci_tui_measure::category::<F>());')
    replace('src/storage.rs', '        Ok(reply)\n    }', '        crate::ci_tui_measure::admitted(crate::ci_tui_measure::category::<F>());\n        Ok(reply)\n    }')
    replace('src/storage.rs', '        if receiver.is_empty() && checkpoint.due(now, changes) {', '        if receiver.is_empty() && checkpoint.due(now, changes) {\n            crate::ci_tui_measure::checkpoint();')
    replace('src/control/driver/shell_output.rs', '            Ok((out, err)) => {', '            Ok((out, err)) => {\n                crate::ci_tui_measure::output_commit();')
    # Cover every production open_with_flags, including program_status, snapshots,
    # Watch pages and dynamic rag flags. Fail closed on unsupported call syntax.
    for path in sorted((subject/'src').rglob('*.rs')):
        name = path.relative_to(subject).as_posix()
        if path.name.endswith('_tests.rs') or path.name == 'tests.rs': continue
        text = read(name)
        if 'Connection::open' not in text: continue
        mask = production_mask(text); edits = []
        for match in re.finditer(r'(?:rusqlite::)?Connection::open_with_flags\(', mask):
            end = closing(mask, match.end())
            args = mask[match.end():end]
            commas = [i for i,c in enumerate(args) if c == ',']
            if len(commas) not in (1,2): raise ValueError(f'unsupported open arguments {name}')
            flags = text[match.end()+commas[0]+1:match.end()+(commas[1] if len(commas)==2 else len(args))].strip()
            if not re.fullmatch(r'[\w\s:|()]+', flags) or re.search(r'\w\s*\(', flags): raise ValueError(f'non-pure open flags {name}: {flags}')
            # Flags are Copy, side-effect-free expressions. The guard covers the
            # connection's lexical scope, not merely SQL execution duration.
            prefix = mask[:match.start()]
            binding = re.search(r'\blet\s+(?:mut\s+)?\w+\s*=\s*$', prefix)
            if not binding: raise ValueError(f'unsupported open binding {name}')
            line = text.count('\n',0,match.start())+1
            site = f'{name}:{line}'; sites.append(site)
            edits.append((binding.start(), f'let _ci_ro_{len(sites)}=crate::ci_tui_measure::ro_scope({flags},{json.dumps(site)});\n    '))
        for at, insertion in reversed(edits): text = text[:at]+insertion+text[at:]
        if edits: changes[name] = text
    if not sites: raise ValueError('no connection hooks found')
    # Only changed source is transferred; manifest witnesses all tracked input.
    changes = {name:text for name,text in changes.items() if before[name] != text}
    patch = ''.join(''.join(difflib.unified_diff(before[name].splitlines(True),text.splitlines(True),fromfile='a/'+name,tofile='b/'+name)) for name,text in changes.items())
    return changes, patch, sites


def prepare(subject, output):
    subject = Path(subject).resolve(); output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    source = source_witness(subject); witness = harness_witness()
    if (subject/'src/ci_tui_measure.rs').exists(): raise ValueError('subject already contains instrumentation module')
    changes, patch, sites = instrument(subject, source, witness)
    manifest = {'version': VERSION, 'source':source, 'harness_sha256':witness,
                'capability':{'native_unix_pty': True, 'readonly_sites':sites,
                              'readonly_scope':'all production Connection::open_with_flags; actual open boundary; lexical duration'},
                'modified':{name:digest(text.encode()) for name,text in changes.items()}}
    # All hooks validated before the first write.
    for name,text in changes.items(): (subject/name).write_text(text)
    (output/'instrumentation.patch').write_text(patch)
    write_json(output/'manifest.json',manifest)
    return manifest


def verify(subject, output):
    manifest = json.loads((output/'manifest.json').read_text())
    if checked(['git','rev-parse','HEAD'],subject) != manifest['source']['head']: raise ValueError('HEAD changed')
    if manifest['harness_sha256'] != harness_witness(): raise ValueError('harness changed')
    files = dict(manifest['source']['files']); files.update(manifest['modified'])
    for name,expected in files.items():
        if digest((subject/name).read_bytes()) != expected: raise ValueError('source witness mismatch: '+name)
    return manifest


def build(subject, output):
    manifest = verify(subject,output)
    command = ['cargo','+1.99.1','build','--locked','--release','--bin','shellboy']
    with (output/'build.log').open('wb') as log:
        status = subprocess.run(command,cwd=subject,stdout=log,stderr=subprocess.STDOUT).returncode
    binary = subject/'target/release/shellboy'
    record = {'command':command,'exit':status,'source':manifest['source']['head'],
              'rustc':checked(['rustc','+1.99.1','-Vv'],subject), 'profile':'release', 'rustflags':os.environ.get('RUSTFLAGS'),
              'cargo_encoded_rustflags':os.environ.get('CARGO_ENCODED_RUSTFLAGS'),
              'host':platform.platform(),
              'binary_sha256':digest(binary.read_bytes()) if status==0 else None}
    write_json(output/'build.json',record)
    if status: raise ValueError('instrumented build failed')

def background_calls(command, count):
    if type(count) is not int or not 1 <= count <= 4:
        raise ValueError('producer count must be an integer from 1 to 4')
    return [{'type':'function_call','id':f'measure-call-{i}',
             'call_id':f'measure-call-{i}','name':'shell',
             'arguments':json.dumps({'command':command,'background':True,'stdin_open':True})}
            for i in range(count)]


def saved_producers(rows, count):
    if (len(rows)!=count or len({r[0] for r in rows})!=count
            or any(r[3:] != ('completed','live') for r in rows)
            or len({(r[1],r[2]) for r in rows})!=1):
        return None
    return dict(execution=rows[0][0],executions=[r[0] for r in rows],
                run=rows[0][1],turn=rows[0][2])


def scenario(subject, output, kind, seconds, cols, smoke=False, producers=1):
    import fcntl, pty, select, struct, termios
    background_calls("", producers)  # Validate bounds before creating any resources.
    producer_count=1 if kind.startswith("p2") else producers
    binary=subject/"target/release/shellboy"
    screen=Screen(cols)  # Validate CI decoder before starting any child or server.
    work=subject
    binary=Path(binary).resolve();work=Path(work).resolve()
    manifest=verify(subject,output)
    build_record=json.loads((output/'build.json').read_text())
    if build_record['exit'] or digest(binary.read_bytes())!=build_record['binary_sha256']:
        raise ValueError('binary witness mismatch')
    native_dir=output/kind
    native_dir.mkdir(parents=True,exist_ok=False)
    result_path=native_dir/'native-metrics.json';data=native_dir/'data';requests=[];errors=[]
    producer="import sys,time\nif not sys.stdin.readline(): sys.exit(0)\nbase=time.monotonic()\ni=0\nwhile True:\n i+=1\n sys.stdout.write('HIDDEN_%08d\\n'%i);sys.stdout.flush()\n time.sleep(max(0,base+i/50-time.monotonic()))\n"
    command='read -r begin && read -r finish' if kind.startswith('p2') else shlex.quote(sys.executable)+' -u -c '+shlex.quote(producer)
    class Proxy(http.server.BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self,*args):pass
        def do_POST(self):
            try:
                body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                index=len(requests);assert index<6,'unexpected repeated model calls';requests.append({'index':index,'body_bytes':len(json.dumps(body))})
                if index==0:
                    output=background_calls(command,producer_count)
                else:
                    output=[{'type':'message','id':f'measure-message-{index}','role':'assistant','status':'completed','content':[{'type':'output_text','text':'NATIVE_MEASURE_READY'}]}]
                event={'type':'response.completed','response':{'id':f'measure-response-{index}','status':'completed','output':output,'usage':{'input_tokens':0,'output_tokens':0,'total_tokens':0}}}
                chunk=('data: '+json.dumps(event)+'\n\n').encode();self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(chunk)));self.send_header('Connection','close');self.end_headers();self.wfile.write(chunk);self.wfile.flush()
            except Exception as e:errors.append(repr(e))
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Proxy);threading.Thread(target=server.serve_forever,daemon=True).start()
    config=native_dir/'config.toml';config.write_text(f'default_connection="fixture"\ndefault_model="fixture"\n[connections.fixture]\nbase_url="http://127.0.0.1:{server.server_port}/v1"\nresponse_max_retries=0\n[io]\noutput_batch_ms=1\n[ui]\nmax_fps=30\npalette="dark"\n')
    ack_read,ack_write=os.pipe();os.set_blocking(ack_read,False)
    env={key:value for key,value in os.environ.items()if not key.startswith('SHELLBOY_') and not any(word in key.upper() for word in ['API_KEY','TOKEN','PROXY']) and key not in ['NO_COLOR','FORCE_COLOR','CLICOLOR_FORCE']}
    env.update(TERM='xterm-256color',NO_PROXY='127.0.0.1,localhost,::1',no_proxy='127.0.0.1,localhost,::1',SHELLBOY_CI_TUI_METRICS=str(result_path),SHELLBOY_CI_TUI_ACK_FD=str(ack_write))
    master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,cols,0,0));os.set_blocking(master,False)
    child=subprocess.Popen([str(binary),'--config',str(config),'--data-dir',str(data)],cwd=native_dir,env=env,stdin=slave,stdout=slave,stderr=slave,pass_fds=(ack_write,));os.close(slave);os.close(ack_write)
    raw=BoundedCapture();window_requests=None;measurement=None;failure=None
    def pump(duration):
        until=time.monotonic()+duration
        while time.monotonic()<until:
            if select.select([master],[],[],min(.02,max(0,until-time.monotonic())))[0]:
                try:chunk=os.read(master,65536)
                except BlockingIOError:continue
                except OSError:return
                raw.append(chunk);screen.feed(chunk)
    def wait(predicate,label,limit=60):
        end=time.monotonic()+limit
        while not predicate():
            assert child.poll()is None,('early exit',child.returncode,label)
            assert not errors,errors
            if time.monotonic()>end:raise AssertionError(label+'\n'+screen.text())
            pump(.02)
    def ack(expected):
        end=time.monotonic()+60
        while time.monotonic()<end:
            if select.select([ack_read],[],[],0)[0]:
                got=os.read(ack_read,1);assert got==expected,(got,expected);return
            assert child.poll()is None,('early exit before ACK',child.returncode)
            pump(.02)
        raise AssertionError('private F12 ACK timed out')
    def rpc(*args):
        response=subprocess.run([str(binary),'--config',str(config),'--data-dir',str(data),*args],cwd=native_dir,env=env,text=True,capture_output=True,timeout=60)
        assert response.returncode==0,(args,response.stdout,response.stderr)
        return json.loads(response.stdout)['result']
    try:
        wait(lambda:(data/'endpoint.json').exists(),'endpoint')
        wait(lambda:'依頼を入力'in screen.text(),'composer')
        watch_root=None
        if kind=='p2-watch':
            watch_root=rpc('thread','create')['thread_id']
            rpc('thread','set','--thread',watch_root,'--title','MEASURE_WATCH_ROOT')
            watch_child=rpc('thread','create','--parent',watch_root)['thread_id']
            rpc('thread','set','--thread',watch_child,'--title','MEASURE_WATCH_CHILD')
            os.write(master,('/switch '+watch_child+'\r').encode())
            wait(lambda:'MEASURE_WATCH_CHILD' in screen.text(),'selected child before fixture request')
        os.write(master,b'create stdin-gated background shells\r')
        wait(lambda:'NATIVE_MEASURE_READY'in screen.text(),'background answer')
        saved={}
        def fixture_ready():
            with sqlite3.connect(f'file:{data/"shellboy.db"}?mode=ro',uri=True) as db:
                row=db.execute("SELECT e.id,e.owner_run_id,e.turn_id,t.state,r.state FROM shell_execution e JOIN turn t ON t.id=e.turn_id JOIN run r ON r.id=e.owner_run_id WHERE e.state='running' ORDER BY e.id").fetchall()
                ready=saved_producers(row,producer_count)
                if ready:
                    saved.update(ready);return True
                return False
        wait(fixture_ready,'saved completed Turn with owned live background shell')
        execution,run,turn=saved['execution'],saved['run'],saved['turn']
        write_json(native_dir/'fixture.json',{'kind':kind,'cols':cols,'rows':40,
            'saved_readiness':saved,'turn_state':'completed','run_state':'live','shell_state':'running',
            'command_sha256':digest(command.encode()),'producer_count':producer_count,
            'producer_target_hz':50 if kind.startswith('p5') else None,
            'aggregate_target_line_hz':50*producer_count if kind.startswith('p5') else None,
            'uses_real_stdin_gate':True,'synthetic_loopback_only':True,'browser_visible':kind in ('p5-list','p2-watch','p5-watch'),
            'browser_mode':'Watch' if kind.endswith('-watch') else ('List' if kind=='p5-list' else None),
            'watch_root':watch_root})
        (native_dir/'fixture-ready.txt').write_text(screen.text())
        if kind.startswith('p5'):
            os.write(master,b'/new\r');pump(2)
            assert 'NATIVE_MEASURE_READY'not in screen.text(),'hidden producer must not remain selected'
            for producer_execution in saved['executions']:
                rpc('shell','write','--execution',producer_execution,'--text','go\n')
        if kind=='p5-watch':
            watch_root=rpc('thread','create')['thread_id']
            rpc('thread','set','--thread',watch_root,'--title','MEASURE_QUIET_ROOT')
        if kind in ('p2-watch','p5-watch'):
            os.write(master,('/switch '+watch_root+'\r').encode())
            title='MEASURE_WATCH_ROOT' if kind=='p2-watch' else 'MEASURE_QUIET_ROOT'
            wait(lambda:title in screen.text(),'selected Watch root')
            os.write(master,b'/threads descendants\r')
            wait(lambda:'子孫の監視' in screen.text() and '読込み中' not in screen.text()
                 and ('MEASURE_WATCH_CHILD' in screen.text() if kind=='p2-watch' else '0件' in screen.text()),
                 'scoped Watch before observation')
            with sqlite3.connect(f'file:{data/"shellboy.db"}?mode=ro',uri=True) as db:
                children=db.execute('SELECT id FROM thread WHERE parent_thread_id=?',(watch_root,)).fetchall()
                assert len(children)==(1 if kind=='p2-watch' else 0),children
                producer_thread=db.execute('SELECT thread_id FROM run WHERE id=?',(run,)).fetchone()[0]
                assert (producer_thread in {row[0] for row in children})==(kind=='p2-watch')
            write_json(native_dir/'watch-scope.json',{'root':watch_root,'direct_children':[row[0] for row in children],
                'producer_thread':producer_thread,'producer_visible':kind=='p2-watch',
                'baseline_watch_available':False,'note':'Candidate-only Watch observation; baseline E has no Watch mode.'})
        if kind=='p5-list':
            os.write(master,b'\x14')
            wait(lambda:'会話一覧' in screen.text() or '会話 ·' in screen.text(),'visible browser before hidden-output observation')
        fixture_record=json.loads((native_dir/'fixture.json').read_text())
        fixture_record['watch_root']=watch_root
        write_json(native_dir/'fixture.json',fixture_record)
        pump(3)
        # All CLI/SQL probes and fixture creation precede this atomic window.
        window_requests=len(requests);os.write(master,b'\x1b[24~');ack(b'S')
        start=time.monotonic();pump(seconds);os.write(master,b'\x1b[24~');ack(b'E');elapsed=time.monotonic()-start
        assert len(requests)==window_requests,('model unexpectedly invoked during observation',window_requests,requests)
        (native_dir/'window-end.txt').write_text(screen.text())
        measurement={'requested_seconds':seconds,'harness_wall_seconds':elapsed,'window_model_requests':len(requests)-window_requests,'execution':execution,'executions':saved['executions'],'producer_count':producer_count,'run':run,'turn':turn}
    except Exception as error:failure=repr(error)
    finally:
        if child.poll()is None:
            try:os.write(master,b'\x11')
            except OSError:pass
            until=time.monotonic()+60
            while child.poll()is None and time.monotonic()<until:pump(.03)
            if child.poll()is None:
                failure=failure or 'graceful cleanup timed out'
                child.terminate()
                try:child.wait(timeout=10)
                except subprocess.TimeoutExpired:child.kill();child.wait(timeout=10)
        os.close(master);os.close(ack_read);server.shutdown();server.server_close()
        (native_dir/'terminal.bin').write_bytes(raw.bytes())
        if child.returncode==0 and data.exists():
            try:
                with sqlite3.connect(f'file:{data/"shellboy.db"}?mode=ro',uri=True) as cleanup_db:
                    remaining=cleanup_db.execute("SELECT count(*) FROM shell_execution WHERE state IN ('starting','running')").fetchone()[0]
                if remaining:failure=failure or 'fixture shell remained live after owner shutdown'
            except sqlite3.Error as error:failure=failure or 'cleanup database check failed: '+str(error)
        result={'version':VERSION,'kind':kind,'cols':cols,'source':manifest,'binary':str(binary),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'platform':sys.platform,'python':sys.version,'harness_sha256':harness_witness(),'measurement':measurement,'proxy_requests':requests,'proxy_errors':errors,'exit':child.returncode,'failure':failure,'native_metrics':json.loads(result_path.read_text())if result_path.exists()else None}
        result['terminal_capture']={'total_bytes':raw.total,'retained_bytes':raw.size,'truncated':raw.total>raw.size}
        result['fixture_ok'], result['fixture_errors'] = validate_result(result,manifest,seconds)
        result['smoke_only']=smoke
        result['performance_policy']='record only; no timing/CPU/read/draw threshold fails the job'
        if result['native_metrics']:
            m=result['native_metrics'];wall=m['wall_seconds']
            result['recorded_observations']={'actual_output_commit_hz':m['output_commits']/wall,
                'p2_ui_reads_zero':m['db_categories']['ui']['executed']==0 and m['read_only_connections']['executed']==0,
                'p5_load_40hz_observed':m['output_commits']/wall>=40 if kind.startswith('p5') else None}
        write_json(native_dir/'result.json',result)
    if not result['fixture_ok']:raise ValueError('fixture invalid: '+repr(result['fixture_errors']))
    return result

class BoundedCapture:
    def __init__(self,limit=CAPTURE_LIMIT):
        self.limit=limit;self.chunks=collections.deque();self.size=0;self.total=0
    def append(self,chunk):
        self.total+=len(chunk);self.chunks.append(chunk);self.size+=len(chunk)
        while self.size>self.limit:
            excess=self.size-self.limit;first=self.chunks.popleft()
            if len(first)>excess:
                self.chunks.appendleft(first[excess:]);self.size-=excess
            else:self.size-=len(first)
    def bytes(self): return b''.join(self.chunks)


class Screen:
    def __init__(self,cols):
        import pyte
        self.screen=pyte.Screen(cols,40)
        self.stream=pyte.Stream(self.screen)
        self.decoder=codecs.getincrementaldecoder('utf-8')('replace')
    def feed(self,chunk):self.stream.feed(self.decoder.decode(chunk))
    def text(self):
        # pyte.display can index past a short line when a wide glyph is clipped
        # at the right edge. Read the bounded grid and skip continuation cells.
        from wcwidth import wcswidth
        lines=[]
        for y in range(self.screen.lines):
            row=[];x=0
            while x<self.screen.columns:
                char=self.screen.buffer[y][x].data or ' '
                row.append(char)
                x+=max(1,wcswidth(char))
            lines.append(''.join(row))
        return '\n'.join(lines)



def validate_result(result,manifest,seconds):
    errors=[];m=result.get('native_metrics');window=result.get('measurement')
    if result.get('failure'):errors.append(result['failure'])
    if result.get('proxy_errors'):errors.append('loopback proxy error')
    if result.get('exit')!=0:errors.append('application did not exit cleanly')
    if not m:return False,errors+['metrics missing']
    if m.get('source_witness')!=manifest['source']['head']:errors.append('embedded source mismatch')
    if m.get('harness_witness')!=manifest['harness_sha256']:errors.append('embedded harness mismatch')
    if m.get('wall_seconds',0)<seconds:errors.append('truncated measurement window')
    if not window or window.get('window_model_requests')!=0:errors.append('unexpected model call')
    if result['kind'].startswith('p5') and m.get('output_commits',0)==0:errors.append('hidden producer produced no committed output')
    for key in ['db_categories','read_only_connections','ui_read_tasks_admitted','draws']:
        if key not in m:errors.append('missing counter '+key)
    return not errors,errors


def scenario_plan(subject, smoke=False, profile="all"):
    if profile not in ("all", "watch"):
        raise ValueError("unknown native observation profile")
    cases=[('p2',2 if smoke else 600),('p5',2 if smoke else 60),('p5-list',2 if smoke else 60)]
    if (subject/'src/ui/watch.rs').exists():
        cases.extend([('p2-watch',2 if smoke else 600),('p5-watch',2 if smoke else 60)])
    return [case for case in cases if profile == "all" or case[0].endswith("-watch")]


def pair(baseline,candidate,output,cols,smoke=False,producers=1,profile="all"):
    if not smoke and os.environ.get('GITHUB_ACTIONS')!='true':
        raise ValueError('full windows run only in manual CI; use --smoke for local fixture qualification')
    if profile == 'watch' and not any((subject/'src/ui/watch.rs').exists() for subject in (baseline,candidate)):
        raise ValueError('Watch profile has no capable subject')
    results=[];errors=[]
    write_json(output/'watch-capabilities.json',{'before':(baseline/'src/ui/watch.rs').exists(),
        'after':(candidate/'src/ui/watch.rs').exists(),
        'profile':profile,'policy':'Missing Watch mode is unavailable, never a zero-cost baseline. Only common scenarios get paired deltas.'})
    for label,subject in [('before',baseline),('after',candidate)]:
        for kind,seconds in scenario_plan(subject,smoke,profile):
            try:results.append(scenario(subject,output/label,kind,seconds,cols,smoke,producers))
            except Exception as error:
                errors.append({'subject':label,'kind':kind,'error':str(error)})
                # Invalid cleanup or capability may contaminate subsequent observations.
                write_json(output/'pair-error.json',errors)
                raise
    write_json(output/'pair.json',{'version':VERSION,'host':platform.platform(),'cols':cols,
               'smoke_only':smoke,'profile':profile,'p5_producer_count':producers,'results':[{'source':r['source']['source']['head'],'kind':r['kind'],'metrics':r['native_metrics']} for r in results],
               'comparison_policy':'same runner, sequential same-profile builds; deltas recorded, never a performance pass/fail'})


def summary(output):
    rows=[]
    for path in sorted(output.rglob('result.json')):
        r=json.loads(path.read_text());m=r.get('native_metrics') or {}
        rows.append({'path':str(path.relative_to(output)),'fixture_ok':r.get('fixture_ok'),
                     'kind':r.get('kind'),'cpu_percent':m.get('cpu_percent_one_core'),
                     'draws':m.get('draws'),'writer':m.get('db_categories'),
                     'readonly':m.get('read_only_connections'),'smoke_only':r.get('smoke_only')})
    write_json(output/'summary.json',{'measurements':rows,'unavailable':(output/'unavailable.json').exists(),
        'policy':'performance values are observations; fixture/build/source errors fail; missing/Windows unavailable is not measurement success'})
    pairs={}
    for row in rows:pairs.setdefault(row['kind'],[]).append(row)
    deltas=[]
    for kind,items in pairs.items():
        before=next((r for r in items if r['path'].startswith('before/')),None)
        after=next((r for r in items if r['path'].startswith('after/')),None)
        if before and after and before['cpu_percent'] is not None and after['cpu_percent'] is not None:
            deltas.append({'kind':kind,'cpu_percentage_point_delta':after['cpu_percent']-before['cpu_percent'],
                           'draw_delta':after['draws']-before['draws']})
    write_json(output/'diff.json',{'same_job_deltas':deltas,'performance_thresholds_enforced':False})


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    sub=parser.add_subparsers(dest='command',required=True)
    for command in ['prepare','build']:
        p=sub.add_parser(command);p.add_argument('--subject',type=Path,required=True);p.add_argument('--output',type=Path,required=True)
    p=sub.add_parser('run-pair');p.add_argument('--baseline',type=Path,required=True);p.add_argument('--candidate',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--cols',type=int,choices=[80,140],required=True);p.add_argument('--smoke',action='store_true');p.add_argument('--producers',type=int,choices=range(1,5),default=1);p.add_argument('--profile',choices=['all','watch'],default='all')
    p=sub.add_parser('unavailable');p.add_argument('--baseline',type=Path,required=True);p.add_argument('--candidate',type=Path,required=True);p.add_argument('--output',type=Path,required=True)
    p=sub.add_parser('summarize');p.add_argument('--output',type=Path,required=True)
    args=parser.parse_args();args.output=args.output.resolve();args.output.mkdir(parents=True,exist_ok=True)
    try:
        if args.command=='prepare':prepare(args.subject.resolve(),args.output)
        elif args.command=='build':build(args.subject.resolve(),args.output)
        elif args.command=='run-pair':pair(args.baseline.resolve(),args.candidate.resolve(),args.output,args.cols,args.smoke,args.producers,args.profile)
        elif args.command=='unavailable':write_json(args.output/'unavailable.json',{
            'status':'unavailable','reason':'Native Windows ConPTY fixture is not implemented; Windows P2/P5 gate remains open',
            'before':source_witness(args.baseline),'after':source_witness(args.candidate),
            'metrics':None,'harness_sha256':harness_witness()})
        else:summary(args.output)
    except Exception as error:
        write_json(args.output/(args.command+'-error.json'),{'status':'error','error':str(error),'command':args.command})
        raise

if __name__=='__main__':main()

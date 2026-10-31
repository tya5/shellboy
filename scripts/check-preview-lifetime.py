#!/usr/bin/env python3
"""Observe preview cancellation and fixed terminal snapshots in a real disposable TUI."""
from pathlib import Path
import argparse
import json, socket, subprocess, sys, tempfile
ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'scripts/dev'))
from pty_capture import Terminal, WATCHDOG_SECONDS
parser=argparse.ArgumentParser(description="Real-TUI DB-gated preview lifetime check; synthetic roots and no clipboard/provider")
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--fixture', type=Path, required=True)
parser.add_argument('--out', type=Path, required=True)
args=parser.parse_args()
BINARY=args.binary.resolve(strict=True)
FIXTURE=args.fixture.resolve(strict=True)
OUT=args.out
OUT.mkdir(parents=True,exist_ok=False)
results=[]
for cols in [80,140]:
    for mode,palette,term,plain in [('dark','dark','xterm-256color',False),('light','light','xterm-256color',False),('plain','dark','xterm-256color',True),('vt100','dark','vt100',False)]:
        case=OUT/f'{cols}-{mode}';case.mkdir()
        with tempfile.TemporaryDirectory(prefix='shellboy-preview-') as scratch:
            root=Path(scratch)
            (root/'.preview-fixture').write_text('synthetic-preview-v1\n')
            (root/'config.toml').write_text('default_connection="fixture"\ndefault_model="offline-fixture"\n[connections.fixture]\nbase_url="http://127.0.0.1:9/v1"\n[ui]\ncopy_backend="osc52"\nbell=false\npalette='+json.dumps(palette)+'\n')
            t=Terminal(FIXTURE,root,cols,palette,term,plain)
            released=False
            def release():
                address=(root/'preview-gate-address').read_text().split(':')
                with socket.create_connection((address[0],int(address[1])),timeout=WATCHDOG_SECONDS): pass
            def scene(label,predicate):
                text=t.expect(predicate,label);(case/f'{label}.txt').write_text(text+'\n');return text
            try:
                scene('ready',lambda s:'F1 help' in s)
                t.send('/switch preview-thread\r')
                pending=scene('blocked-open',lambda s:'出力を取得中' in s and (root/'preview-gate-entered').exists())
                assert '時間内' not in pending and '出力取得に失敗' not in pending
                y=next(i+1 for i,line in enumerate(pending.splitlines()) if 'printf GATED_OUTPUT_END' in line)
                t.send(f'\x1b[<0;5;{y}M\x1b[<0;5;{y}m')
                scene('closed-during-delay',lambda s:'出力を取得中' not in s and '出力取得に失敗' not in s and 'printf GATED_OUTPUT_END' in s)
                t.send(' ')
                second=scene('reopened-during-delay',lambda s:'出力を取得中' in s)
                assert '時間内' not in second and '出力取得に失敗' not in second
                release();released=True
                observed=scene('released-output',lambda s:'│ GATED_OUTPUT_END' in s and '取得 ' in s and '出力を取得中' not in s)
                notice=next(line.strip() for line in observed.splitlines() if '取得 ' in line)
                counter=root/'preview-read-requests'
                requests=counter.read_text()
                assert requests=='2',requests
                for n in range(3):
                    title=f'gate-title-{n}'
                    subprocess.run([str(BINARY),'--config',str(root/'config.toml'),'--data-dir',str(root/'data'),'thread','set','--thread','preview-thread','--title',title],check=True,capture_output=True,timeout=WATCHDOG_SECONDS)
                    text=scene(f'changed-{n}',lambda s:title in s and '│ GATED_OUTPUT_END' in s)
                    assert next(line.strip() for line in text.splitlines() if '取得 ' in line)==notice
                    assert counter.read_text()==requests
                t.send(' ')
                scene('closed-after-result',lambda s:'│ GATED_OUTPUT_END' not in s and '取得 ' not in s and '出力取得に失敗' not in s)
                (case/'raw.vt').write_bytes(t.raw)
                results.append({'cols':cols,'mode':mode,'requests':int(requests),'observation_notice':notice,'changed_events_with_fixed_notice':3})
                print(json.dumps(results[-1],ensure_ascii=False),flush=True)
            finally:
                if not released:
                    try: release()
                    except OSError: pass
                t.close(force=True)
(OUT/'results.json').write_text(json.dumps(results,ensure_ascii=False,indent=2)+'\n')
print('PASS',len(results))

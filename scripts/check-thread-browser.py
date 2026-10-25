#!/usr/bin/env python3
"""Opt-in real-WezTerm review of dense thread rows and parent/child navigation."""
import argparse
import json
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture-root', type=Path, required=True)
    parser.add_argument('--binary', type=Path, default=Path('target/debug/shellboy'))
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--images', action='store_true', help='Use macOS ScreenCaptureKit; otherwise capture terminal cells')
    args = parser.parse_args()
    if not (args.fixture_root / '.shellboy-visual-fixture').is_file():
        parser.error('Create a dedicated visual_fixture first')
    if (args.fixture_root / 'data/endpoint.json').exists():
        parser.error('Dedicated fixture owner must be stopped')
    args.out.mkdir(parents=True, exist_ok=True)
    cases = {
        'wide': (120, 40, 'visual-composer', '/threads', []),
        'collapsed': (120, 40, 'visual-composer', '/threads', ['\x1b[H', '\x1b[B', '\x1b[D']),
        'expanded': (120, 40, 'visual-composer', '/threads', ['\x1b[H', '\x1b[B', '\x1b[D', '\x1b[C', '\x1b[C']),
        'page': (120, 40, 'visual-composer', '/threads', ['\x1b[6~']),
        'narrow': (70, 24, 'visual-composer', '/threads', ['\x1b[6~', '\x1b[F']),
        'filter': (100, 30, 'visual-composer', '/threads 調査タスク 0', []),
        'conversation': (100, 36, 'visual-conversation', None, []),
    }
    cells = {}
    for name, (cols, rows, thread, command, keys) in cases.items():
        out = args.out / (name + ('.png' if args.images else '.txt'))
        cmd = [sys.executable, str(Path(__file__).with_name('capture-tui.py')),
               '--fixture-root', str(args.fixture_root), '--binary', str(args.binary),
               '--thread', thread, '--cols', str(cols), '--rows', str(rows), '--out', str(out)]
        cmd += ['--image-backend', 'screen-capture-kit'] if args.images else ['--text-only']
        if command:
            cmd += ['--command', command]
        for key in keys:
            cmd += ['--keys', key]
        subprocess.run(cmd, check=True, timeout=60)
        cells[name] = out.with_suffix('.txt').read_text()
    checks = {
        'aligned_columns': all(label in cells['wide'] for label in ('会話', '作業場所', '状態', '更新')),
        'known_tree_branches': '├─' in cells['wide'] and '▾' in cells['wide'],
        'unknown_parent_explicit': '[親未取得]' in cells['wide'],
        'collapsed_hides_children': '▸' in cells['collapsed'] and '調査タスク 37' not in cells['collapsed'],
        'expanded_nested_child': '折り返しと結合文字' in cells['expanded'],
        'selected_preview_plain': '確認結果 構造化データ' in cells['expanded'],
        'page_merges_parent': '[親未取得]' not in cells['page'] and '別プロジェクトの親' in cells['page'] and '親が次ページにある子' in cells['page'],
        'narrow_selection_visible': any('›' in line and '親が次ページにある子' in line for line in cells['narrow'].splitlines()),
        'filter_keeps_missing_parent_explicit': '[親未取得]' in cells['filter'] and '調査タスク 0' in cells['filter'],
        'normal_conversation': '調査結果' in cells['conversation'] and 'スレッド ·' not in cells['conversation'],
        'no_raw_multiline_preview': all('```' not in cells[name] and '**構造化データ**' not in cells[name] for name in cases if name != 'conversation'),
    }
    (args.out / 'checks.json').write_text(json.dumps(checks, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(checks, ensure_ascii=False), flush=True)
    if not all(checks.values()):
        raise SystemExit('Thread browser checks failed; review actual cells/images')


if __name__ == '__main__':
    main()

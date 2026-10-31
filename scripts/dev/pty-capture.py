#!/usr/bin/env python3
"""Capture completed Shellboy PTY screens in a disposable fixture, or replay VT."""
import argparse
import json
from pathlib import Path
import sys

from pty_capture import Frames, Terminal

KEYS = {'enter': '\r', 'tab': '\t', 'escape': '\x1b\x1b', 'up': '\x1b[A',
        'down': '\x1b[B', 'left': '\x1b[D', 'right': '\x1b[C', 'home': '\x1b[H',
        'end': '\x1b[F', 'page_up': '\x1b[5~', 'page_down': '\x1b[6~',
        'ctrl_end': '\x1b[1;5F', 'clear': '\x15', 'backspace': '\x7f',
        'delete': '\x1b[3~', 'copy': 'c', 'original': 'o', 'refresh': 'r', 'space': ' '}
KEYS.update({f'f{i}': value for i, value in enumerate(
    ['\x1bOP', '\x1bOQ', '\x1bOR', '\x1bOS', '\x1b[15~', '\x1b[17~'], 1)})
COMMANDS = {'/help', '/copy', '/quit', '/switch', '/new', '/threads', '/status'}


def steps(value):
    if not isinstance(value, list) or len(value) > 128:
        raise ValueError('Steps must be an array of at most 128 objects')
    for step in value:
        if not isinstance(step, dict) or set(step) - {'command', 'paste', 'key', 'until', 'label'}:
            raise ValueError('Invalid step fields')
        actions = set(step) & {'command', 'paste', 'key'}
        if len(actions) != 1 or not isinstance(step.get('until'), str) or not step['until']:
            raise ValueError('Each step needs one action and nonempty until text')
        action = next(iter(actions))
        text = step[action]
        if not isinstance(text, str) or len(text) > 1024 * 1024:
            raise ValueError('Action text must be a string of at most 1 MiB')
        if action == 'command' and (not text.split() or text.split()[0] not in COMMANDS
                                   or text[0] != '/' or any(ord(char) < 32 for char in text)):
            raise ValueError('Only the documented non-image commands are allowed')
        if action == 'key' and text not in KEYS:
            raise ValueError('Use a documented named key, not raw terminal bytes')
        if action == 'paste' and '\x1b' in text:
            raise ValueError('Paste cannot contain terminal escapes')
        if 'label' in step and (not isinstance(step['label'], str) or len(step['label']) > 64):
            raise ValueError('Label must be a string of at most 64 characters')
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__, epilog=(
        'Standard library only. Live mode requires Unix and a trusted current '
        'Shellboy binary; creates config/data/HOME in a temporary root, fixes '
        'copy_backend=osc52, uses only unauthenticated loopback, and never '
        'forwards escape output to a GUI terminal. Each JSON step has command, '
        'paste, or key plus until (text on a fresh completed frame), optional label. '
        'Commands: ' + ', '.join(sorted(COMMANDS)) + '. Image-clipboard commands '
        'and raw key bytes are deliberately unavailable. F7/F8 and OS link-opening '
        'actions are not exposed. Non-command text uses '
        'bracketed paste. These are trusted test drivers, not a sandbox for '
        'arbitrary programs. 60s watchdog; 4 MiB output; 400x200 maximum. '
        'The decoder supports the VT subset emitted by this TUI, not a physical '
        'font/full terminal emulator. Unknown cell operations fail closed.'))
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--binary', type=Path, help='Existing trusted Shellboy binary (no build)')
    source.add_argument('--replay', type=Path, help='Read a VT capture instead of starting a process')
    parser.add_argument('--cols', type=int, default=80)
    parser.add_argument('--rows', type=int, default=36)
    parser.add_argument('--palette', choices=['dark', 'light'], default='dark')
    parser.add_argument('--term', default='xterm-256color')
    parser.add_argument('--no-color', action='store_true')
    parser.add_argument('--ready', default='F1 help', help='Live startup completed-frame marker')
    group = parser.add_mutually_exclusive_group()
    group.add_argument('--steps', default='[]', help='JSON action array (see epilog)')
    group.add_argument('--steps-stdin', action='store_true', help='Read action JSON from stdin (max 1 MiB)')
    parser.add_argument('--until', help='Replay: first completed frame containing this text')
    parser.add_argument('--out', type=Path, help='New JSON file; refuses existing paths, prints summary only')
    args = parser.parse_args()
    terminal = None
    try:
        if args.out and args.out.exists():
            raise ValueError('--out must not exist')
        if args.replay:
            if args.steps_stdin or args.steps != '[]':
                raise ValueError('Replay does not send keys')
            frames = Frames(args.cols, args.rows)
            selected = None
            with args.replay.open('rb') as capture:
                while chunk := capture.read(65536):
                    for text in frames.feed(chunk):
                        if args.until is None or args.until in text:
                            selected = text
                            if args.until is not None:
                                break
                    if selected is not None and args.until is not None:
                        break
            if selected is None:
                raise ValueError('No matching completed frame')
            screens = [{'label': 'replay', 'text': selected}]
        else:
            data = sys.stdin.buffer.read(1024 * 1024 + 1) if args.steps_stdin else args.steps.encode()
            if len(data) > 1024 * 1024:
                raise ValueError('Step JSON exceeds 1 MiB')
            actions = steps(json.loads(data))
            binary = args.binary.resolve(strict=True)
            terminal = Terminal(binary, cols=args.cols, rows=args.rows, palette=args.palette,
                                term=args.term, no_color=args.no_color)
            text = terminal.expect(lambda text: args.ready in text, 'ready')
            screens = [{'label': 'ready', 'text': text}]
            for index, step in enumerate(actions):
                before = terminal.frames.frame_count
                if 'paste' in step:
                    terminal.paste(step['paste'])
                elif 'key' in step:
                    terminal.send(KEYS[step['key']])
                else:
                    terminal.send('\x15' + step['command'] + '\r')
                text = terminal.expect(lambda text: terminal.frames.frame_count > before and step['until'] in text,
                                       step.get('label', str(index)))
                screens.append({'label': step.get('label', str(index)), 'text': text})
            terminal.close()
            terminal = None
        result = dict(ok=True, cols=args.cols, rows=args.rows, screens=screens)
        encoded = json.dumps(result, separators=(',', ':')) + '\n'
        if args.out:
            with args.out.open('x', encoding='utf-8') as output:
                output.write(encoded)
            print(json.dumps(dict(ok=True, out=str(args.out), screens=len(screens)), separators=(',', ':')))
        else:
            print(encoded, end='')
        return 0
    except (OSError, ValueError, RuntimeError, TimeoutError) as error:
        print(json.dumps(dict(ok=False, error=str(error).split('\n', 1)[0]), separators=(',', ':')))
        return 2
    finally:
        if terminal is not None:
            terminal.close(force=True)


if __name__ == '__main__':
    raise SystemExit(main())

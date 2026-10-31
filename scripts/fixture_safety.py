"""Preflight guards for synthetic terminal fixtures, not a product sandbox."""
from pathlib import Path
import re
import tomllib


def require_osc52(config):
    settings = tomllib.loads(Path(config).read_text())
    if settings.get('ui', {}).get('copy_backend') != 'osc52':
        raise ValueError('Fixture config must explicitly set [ui] copy_backend="osc52"')


def reject_gui_clipboard_input(commands, keys, draft):
    # GUI terminals may apply OSC52 to the host clipboard. Copy tests belong in
    # the pipe-drained PTY driver, never a GUI capture window.
    for command in commands:
        if command.split(maxsplit=1)[0:1] in [['/copy'], ['/image'], ['/paste-image']]:
            raise ValueError('Clipboard checks require a pipe-drained PTY, not GUI capture')
    for text in [*keys, draft]:
        literal = re.sub(r'\x1b(?:\[[0-9;?]*[A-DHF~]|O[A-DHFP-S])', '', text)
        if '\x1b' in literal or any(key in literal for key in ('c', 'C', '\x19', '\x03')) or '/image' in literal or '/paste-image' in literal:
            raise ValueError('Potential clipboard shortcut in GUI capture; use the PTY driver')

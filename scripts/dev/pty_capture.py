"""Disposable Shellboy PTY transport and completed VT frames (standard library)."""
import codecs
import errno
import json
import os
from pathlib import Path
import re
import select
import signal
import struct
import subprocess
import tempfile
import time
import tomllib
from urllib.parse import urlsplit

try:
    from .vt_screen import Screen
except ImportError:
    from vt_screen import Screen

WATCHDOG_SECONDS = 60
OUTPUT_LIMIT = 4 * 1024 * 1024
BOUNDARY = re.compile(rb'\x1b\[\?25l|\x1b\[\?25h\x1b\[\d+;\d+H')


class Frames:
    def __init__(self, cols, rows):
        self.screen = Screen(cols, rows)
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.pending = bytearray()
        self.completed_text = ''
        self.frame_count = 0
        self.total = 0

    def feed(self, data):
        self.total += len(data)
        if self.total > OUTPUT_LIMIT:
            raise ValueError('PTY output exceeds 4 MiB')
        self.pending.extend(data)
        while match := BOUNDARY.search(self.pending):
            self.screen.feed(self.decoder.decode(self.pending[:match.end()]))
            del self.pending[:match.end()]
            self.completed_text = self.screen.text()
            self.frame_count += 1
            yield self.completed_text


def check_config(root):
    config = tomllib.loads((root / 'config.toml').read_text(encoding='utf-8'))
    if set(config) - {'default_connection', 'default_model', 'connections', 'ui'}:
        raise ValueError('Only minimal synthetic fixture config is allowed')
    if config.get('ui', {}).get('copy_backend') != 'osc52':
        raise ValueError('Fixture requires explicit [ui] copy_backend="osc52"')
    if set(config.get('ui', {})) - {'copy_backend', 'palette', 'bell'}:
        raise ValueError('Unexpected fixture UI setting')
    for connection in config.get('connections', {}).values():
        url = urlsplit(connection.get('base_url', ''))
        if (url.scheme != 'http' or url.hostname not in ('127.0.0.1', '::1')
                or url.username or url.password or connection.get('api_key_env')
                or connection.get('headers')):
            raise ValueError('Only unauthenticated literal-loopback connections are allowed')


class Terminal:
    def __init__(self, binary, root=None, cols=80, palette='dark', term='xterm-256color', no_color=False,
                 rows=36, connection_url='http://127.0.0.1:9/v1'):
        if os.name != 'posix':
            raise OSError('Live PTYs require Unix; replay and VT tests are portable')
        import fcntl
        import pty
        import termios
        self.frames = Frames(cols, rows)
        self.screen = self.frames.screen
        self.raw = bytearray()
        self.master = None
        self.process = None
        self.scratch = None
        try:
            if root is None:
                self.scratch = tempfile.TemporaryDirectory(prefix='shellboy-pty-')
                root = Path(self.scratch.name)
                (root / 'config.toml').write_text(
                    'default_connection="fixture"\ndefault_model="offline-fixture"\n'
                    '[connections.fixture]\nbase_url=' + json.dumps(connection_url) + '\n'
                    '[ui]\ncopy_backend="osc52"\nbell=false\npalette=' + json.dumps(palette) + '\n',
                    encoding='utf-8')
            self.root = Path(root).resolve()
            # Existing-root support is for repository-owned synthetic fixtures,
            # never a live/user data directory. The public CLI only creates roots.
            if not self.root.is_relative_to(Path(tempfile.gettempdir()).resolve()):
                raise ValueError('Fixture root must be inside the OS temporary directory')
            if (self.root / 'data').is_symlink() or (self.root / 'config.toml').is_symlink():
                raise ValueError('Symlinked fixture config/data is forbidden')
            if (self.root / 'data/endpoint.json').exists():
                raise ValueError('Fixture owner must be stopped')
            check_config(self.root)
            allowed = ('PATH', 'LANG', 'LC_ALL', 'LC_CTYPE', 'TZ', 'SYSTEMROOT', 'WINDIR')
            env = {key: os.environ[key] for key in allowed if key in os.environ}
            home = self.root / 'isolated-home'
            (home / 'tmp').mkdir(parents=True, exist_ok=True)
            env.update(TMPDIR=str(home / 'tmp'), TMP=str(home / 'tmp'), TEMP=str(home / 'tmp'),
                       HOME=str(home), USERPROFILE=str(home), APPDATA=str(home / 'config'),
                       LOCALAPPDATA=str(home / 'data'), XDG_CONFIG_HOME=str(home / 'config'),
                       XDG_DATA_HOME=str(home / 'data'), TERM=term)
            env.pop('COLORTERM', None)
            env.pop('NO_COLOR', None)
            if no_color:
                env['NO_COLOR'] = '1'
            executable = [str(binary)] if isinstance(binary, (str, Path)) else list(binary)
            master, slave = pty.openpty()
            self.master = master
            try:
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
                self.process = subprocess.Popen(executable + ['--config', str(self.root / 'config.toml'),
                    '--data-dir', str(self.root / 'data')], cwd=self.root, env=env,
                    stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
            finally:
                os.close(slave)
            os.set_blocking(master, False)
        except BaseException:
            self.close(force=True)
            raise

    def text(self):
        return self.frames.completed_text

    def cells(self):
        return self.screen.text()

    def read_ready(self, render=True):
        try:
            chunk = os.read(self.master, 65536)
        except BlockingIOError:
            return
        except OSError as error:
            if error.errno != errno.EIO and self.process.poll() is None:
                raise
            return
        self.raw.extend(chunk)
        if len(self.raw) > OUTPUT_LIMIT:
            raise ValueError('PTY output exceeds 4 MiB')
        if render:
            for _ in self.frames.feed(chunk):
                pass

    def send(self, text):
        # Raw keys are for trusted repository drivers. The CLI has constrained
        # actions and never accepts image-clipboard commands or arbitrary bytes.
        pending = text.encode('utf-8')
        deadline = time.monotonic() + WATCHDOG_SECONDS
        while pending:
            if time.monotonic() >= deadline:
                raise TimeoutError('60s PTY input watchdog')
            readable, writable, _ = select.select([self.master], [self.master], [], .05)
            if readable:
                self.read_ready()
            if writable:
                try:
                    pending = pending[os.write(self.master, pending[:256]):]
                except BlockingIOError:
                    pass

    def paste(self, text):
        if '\x1b' in text:
            raise ValueError('Pasted text cannot contain terminal control escapes')
        self.send('\x1b[200~' + text + '\x1b[201~')

    def expect(self, predicate, label):
        deadline = time.monotonic() + WATCHDOG_SECONDS
        while not predicate(self.text()):
            if time.monotonic() >= deadline:
                raise TimeoutError('60s watchdog: ' + label + '\n' + self.text())
            if self.process.poll() is not None:
                raise RuntimeError('TUI exited before ' + label)
            if select.select([self.master], [], [], .05)[0]:
                self.read_ready()
        return self.text()

    def close(self, force=False):
        error = None
        try:
            if self.process and self.process.poll() is None:
                if not force:
                    try:
                        self.send('\x11')  # Global Ctrl+Q also works in an overlay.
                        deadline = time.monotonic() + WATCHDOG_SECONDS
                        while self.process.poll() is None and time.monotonic() < deadline:
                            if select.select([self.master], [], [], .05)[0]:
                                self.read_ready(render=False)
                    except BaseException as failure:
                        error = failure
                if self.process.poll() is None:
                    try:
                        os.killpg(self.process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    if error is None:
                        error = TimeoutError('60s shutdown watchdog')
                self.process.wait()
            if self.process and self.process.returncode and error is None and not force:
                error = RuntimeError('TUI exit status: ' + str(self.process.returncode))
        finally:
            if self.master is not None:
                os.close(self.master)
                self.master = None
            if self.scratch is not None:
                self.scratch.cleanup()
                self.scratch = None
        if error and not force:
            raise error

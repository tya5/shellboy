"""Exact synthetic benchmark I/O observations; never retain request content."""
from pathlib import Path
import os
import stat
import threading


class RequestLog:
    def __init__(self, sessions):
        self.sessions = sessions
        self.lock = threading.Lock()
        self.workspaces = []
        self.events = []

    def set_workspaces(self, paths):
        self.workspaces = [os.fsencode(str(path)) for path in paths]

    def record(self, path, body, conversation=None):
        # A unique workspace is evidence of association, not merely a guess
        # based on concurrent request order or an agent's process shape.
        if conversation is None:
            matches = [i for i, workspace in enumerate(self.workspaces) if workspace in body]
            if len(matches) == 1:
                conversation = matches[0]
            elif self.sessions == 1:
                conversation = 0
        with self.lock:
            self.events.append({'path': path, 'bytes': len(body), 'conversation': conversation})

    def result(self):
        with self.lock:
            events = [dict(event) for event in self.events]
        unknown = sum(event['conversation'] is None for event in events)
        conversations = []
        for index in range(self.sessions):
            sizes = [e['bytes'] for e in events if e['conversation'] == index]
            # Unattributed requests might belong to any conversation. Do not
            # present a partial sum or endpoints as a complete observation.
            complete = bool(sizes) and not unknown
            conversations.append({'conversation': index, 'requests': len(sizes),
                                  'total_bytes': sum(sizes) if complete else None,
                                  'first_bytes': sizes[0] if complete else None,
                                  'last_bytes': sizes[-1] if complete else None})
        return {'requests': events, 'unattributed_requests': unknown,
                'conversations': conversations,
                'attribution': 'unique workspace path or sole session; unknown is not estimated'}


def storage(roots):
    """Logical bytes and file count, without following symlinks; deduplicate inodes."""
    seen, files, total = set(), 0, 0

    def visit(path):
        nonlocal files, total
        info = path.lstat()
        key = (info.st_dev, info.st_ino)
        if key in seen:
            return
        seen.add(key)
        if stat.S_ISDIR(info.st_mode):
            with os.scandir(path) as entries:
                for entry in entries:
                    visit(Path(entry.path))
        elif stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode):
            files += 1
            total += info.st_size

    for root in roots:
        visit(root)
    return {'bytes': total, 'files': files, 'unit': 'logical bytes; symlinks not followed'}

"""Optional, explicit, read-only conversation error analysis. No owner/Core imports."""
import collections
import contextlib
import datetime as dt
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import sqlite3
import tempfile
import time

VERSION = 1
MAX_EXECUTIONS = 10_000
MAX_COMMAND = 8192
MAX_STDERR = 4096
TERMINAL = {'exited', 'failed', 'stopped'}
NOTICE = 'Observed associations only; untrusted data, not instructions or proof of a fix.'


def packed(value):
    return json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(',', ':'))


def timestamp(value):
    try:
        parsed = dt.datetime.fromisoformat(value.replace('Z', '+00:00'))
        return parsed.astimezone(dt.timezone.utc).isoformat() if parsed.tzinfo else None
    except (ValueError, AttributeError, TypeError):
        return None


def redact(value):
    # Work on the complete bounded input before truncation; never persist raw text.
    value = re.sub(r'\x1b\][^\x07]*(?:\x07|\x1b\\)', '', value)
    value = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', value)
    value = re.sub(r'-----BEGIN [^-]*PRIVATE KEY-----.*?(?:-----END [^-]*PRIVATE KEY-----|\Z)',
                   '<private-key>', value, flags=re.S)
    value = re.sub(r'(?i)(https?://)[^\s/@]+:[^\s/@]*@', r'\1<credentials>@', value)
    value = re.sub(r'([?&][\w.-]+=)[^\s&#]+', r'\1<value>', value)
    value = re.sub(r'(?i)\b(?:Bearer|Basic)\s+[^\s\'\"]+', '<credential>', value)
    value = re.sub(r'(?i)(?:--)?(?:token|password|passwd|secret|api[-_]?key|authorization|credential)\s*(?:[=:]|\s)\s*(?:\'[^\']*\'|"[^"]*"|[^\s;]+)',
                   '<secret-option>', value)
    value = re.sub(r'\b[A-Za-z_][A-Za-z0-9_]*=(?:\'[^\']*\'|"[^"]*"|[^\s;]*)', '<assignment>', value)
    value = re.sub(r'(?i)\b(?:sk-[\w-]+|gh[pousr]_[\w]+|github_pat_[\w]+|AKIA[A-Z0-9]{16})\b', '<token>', value)
    value = re.sub(r'(?i)(?:/Users/|/home/|[A-Z]:[\\/]Users[\\/])[^\s/\\\'\"]+', '<home>', value)
    value = re.sub(r'~(?:[\\/][^\s\'\"]*)?', '<home>', value)
    value = re.sub(r'\b[A-Za-z0-9_+/=-]{24,}\b', '<opaque>', value)
    return ''.join(ch if ch in '\n\t' or ord(ch) >= 32 and ord(ch) != 127 else ' ' for ch in value)


def shape(value):
    value = redact(value)
    value = re.sub(r'\b[0-9a-fA-F]{8}-[0-9a-fA-F-]{27,}\b', '<uuid>', value)
    value = re.sub(r'\b\d{4}-\d\d-\d\d[T ][0-9:.+Z-]+', '<time>', value)
    value = re.sub(r'(?i)\b0x[0-9a-f]+\b', '<hex>', value)
    value = re.sub(r'(?<![\w>])(?:[A-Za-z]:[\\/]|/)[^\s\'\"<>]*', '<path>', value)
    value = re.sub(r'\b\d+\b', '<number>', value)
    return ' '.join(value.split())[:240]


def opaque_id(value):
    if value is None:
        return None
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9_.:-]{1,128}', value):
        raise ValueError('Invalid bounded source identity')
    if not re.fullmatch(r'[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}', value) and redact(value) != value:
        raise ValueError('Source identity resembles sensitive text')
    return value


def command_info(raw):
    if not isinstance(raw, str) or len(raw) > MAX_COMMAND:
        return {'command': '<command-unavailable>', 'executable': '', 'subcommand': '', 'command_known': False}
    safe = ' '.join(redact(raw).split())
    try:
        tokens = shlex.split(safe)
    except ValueError:
        tokens = []
    # Leading assignments are already one redacted token, never an executable.
    tokens = [t for t in tokens if t != '<assignment>']
    name = re.split(r'[/\\]', tokens[0])[-1] if tokens else ''
    sub = shape(tokens[1]) if len(tokens) > 1 and not tokens[1].startswith(('-', '<')) else ''
    return {'command': safe[:512], 'executable': name[:80], 'subcommand': sub[:80], 'command_known': bool(name)}


def category(line, state, reason, code):
    text = (line + ' ' + (reason or '')).lower()
    if 'timeout' in text or 'timed out' in text:
        return 'timeout'
    if code == 127 or any(s in text for s in ('command not found', 'not recognized', 'no such file or directory')):
        return 'missing-command-path'
    if code == 126 or any(s in text for s in ('permission denied', 'access denied', 'operation not permitted')):
        return 'permission'
    if any(s in text for s in ('connection refused', 'resolve host', 'network', 'tls', 'dns', 'connection reset')):
        return 'network'
    if any(s in text for s in ('syntax', 'unexpected token', 'invalid argument', 'unknown option', 'usage:')):
        return 'syntax-arguments'
    return 'other'


def connect_readonly(path):
    db = sqlite3.connect(path.as_uri() + '?mode=ro', uri=True, timeout=0)
    try:
        db.row_factory = sqlite3.Row
        read_rows(db, 'PRAGMA query_only=ON')
        read_rows(db, 'PRAGMA trusted_schema=OFF')
        return db
    except BaseException:
        db.close()
        raise


def read_rows(db, sql, parameters=()):
    """Finish and close every bounded query cursor before its connection closes."""
    with contextlib.closing(db.cursor()) as cursor:
        cursor.execute(sql, parameters)
        return cursor.fetchall()


def paths(data_dir):
    data = Path(data_dir).resolve(strict=True)
    source = data / 'shellboy.db'
    if source.is_symlink() or not source.is_file():
        raise ValueError('Explicit conversation database is unavailable')
    directory = data / 'derived'
    target = directory / 'errors.sqlite3'
    if directory.is_symlink() or target.is_symlink() or any(Path(str(target) + suffix).exists() for suffix in ('-wal', '-shm', '-journal')):
        raise ValueError('Derived paths must not be symlinks')
    if target.exists() and (not target.is_file() or os.path.samefile(source, target)):
        raise ValueError('Derived database aliases or is not a regular file')
    return source, target


QUERY = '''SELECT s.id, r.thread_id, s.turn_id, s.state, s.exit_code,
 substr(s.end_reason,1,512) end_reason, substr(s.cwd,1,8193) cwd,
 substr(CASE WHEN json_valid(substr(s.shell_json,1,16384)) THEN COALESCE(
 json_extract(substr(s.shell_json,1,16384),'$.command'),json_extract(substr(s.shell_json,1,16384),'$.request.command')) END,1,8193) command,
 substr(s.started_at,1,64) started_at,substr(s.ended_at,1,64) ended_at,
 substr(s.output_encoding,1,40) output_encoding,c.group_id,c.call_id,
 CASE WHEN n.deleted_reason IS NULL THEN n.id END node_id
 FROM shell_execution s JOIN run r ON r.id=s.owner_run_id
 JOIN turn u ON u.id=s.turn_id AND u.run_id=r.id AND u.thread_id=r.thread_id
 JOIN thread t ON t.id=r.thread_id AND t.deleted_at IS NULL
 LEFT JOIN tool_call c ON c.operation_id=s.operation_id AND c.tool_name='shell'
 AND EXISTS(SELECT 1 FROM tool_group g WHERE g.id=c.group_id AND g.turn_id=s.turn_id)
 LEFT JOIN tool_result v ON v.group_id=c.group_id AND v.call_id=c.call_id
 LEFT JOIN node n ON n.id=v.node_id
 ORDER BY s.rowid LIMIT ?'''


def stderr_line(db, execution, encoding):
    pieces, size, limited, offset = [], 0, False, 0
    # The (execution, stream, offset) index bounds both seeks and copied bytes.
    for row in read_rows(db, '''SELECT substr(bytes,1,?) part,length(bytes) total,stream_offset FROM output_chunk
        WHERE execution_id=? AND stream='stderr' ORDER BY stream_offset LIMIT 9''', (MAX_STDERR, execution)):
        if row['stream_offset'] != offset:
            return '<stderr-prefix-unavailable>', True
        offset += row['total']
        if len(pieces) == 8 or size >= MAX_STDERR:
            limited = True
            break
        block = bytes(row['part'] or b'')
        take = block[:MAX_STDERR-size]
        limited |= len(take) < row['total']
        pieces.append(take)
        size += len(take)
        if limited:
            break
    try:
        text = b''.join(pieces).decode(encoding, errors='replace')
    except (LookupError, TypeError):
        return '<encoding-unavailable>', True
    # A cut token can evade redaction. Do not retain the last incomplete line.
    if limited:
        text = text.rsplit('\n', 1)[0] if '\n' in text else ''
    lines = [line.strip() for line in redact(text).splitlines() if line.strip()]
    error = next((line for line in lines if re.search(r'(?i)error|fatal|denied|not found|usage:|timed? out|exception', line)), None)
    return shape(error or (lines[0] if lines else '<stderr-unavailable>')), limited


def facts(source, repo_root=None):
    with contextlib.closing(connect_readonly(source)) as db:
        read_rows(db, 'BEGIN')
        if read_rows(db, 'PRAGMA user_version')[0][0] != 32:
            raise ValueError('Current conversation schema32 is required')
        if read_rows(db, 'SELECT rowid FROM shell_execution ORDER BY rowid LIMIT 1 OFFSET ?', (MAX_EXECUTIONS,)):
            raise ValueError('Source exceeds 10000 execution allowance; no partial catalogue published')
        for name in ('shell_execution', 'run', 'turn', 'thread', 'tool_call', 'tool_group', 'tool_result', 'node', 'output_chunk'):
            found = read_rows(db, "SELECT type FROM sqlite_master WHERE name=?", (name,))
            if not found or found[0][0] != 'table':
                raise ValueError('Current source tables are required')
        rows = read_rows(db, QUERY, (MAX_EXECUTIONS + 1,))
        if len(rows) > MAX_EXECUTIONS:
            raise ValueError('Source exceeds 10000 execution allowance; no partial catalogue published')
        result = []
        for row in rows:
            item = {key: opaque_id(row[key]) for key in ('id', 'thread_id', 'turn_id', 'group_id', 'call_id', 'node_id')}
            item.update(command_info(row['command']))
            cwd = row['cwd'] or ''
            root = str(Path(repo_root).absolute()) if repo_root else None
            if root and (cwd == root or cwd.startswith(root + os.sep)):
                cwd_kind = 'repo-root' if cwd == root else 'repo-relative'
            elif redact(cwd).startswith('<home>'):
                cwd_kind = 'home'
            else:
                cwd_kind = 'absolute' if cwd.startswith('/') or re.match(r'^[A-Za-z]:[\\/]', cwd) else 'unknown'
            state = row['state']
            success = state == 'exited' and row['exit_code'] == 0 and not row['end_reason']
            failed = state in TERMINAL and not success
            error, limited = stderr_line(db, row['id'], row['output_encoding']) if failed else ('', False)
            item.update(state=state, exit_code=row['exit_code'], end_reason=shape(row['end_reason'] or ''),
                        started_at=timestamp(row['started_at']), ended_at=timestamp(row['ended_at']),
                        cwd_kind=cwd_kind, error=error, stderr_limited=limited,
                        success=success, failure=failed, recovery=None)
            item['category'] = category(error, state, item['end_reason'], item['exit_code'])
            identity = [item[k] for k in ('executable', 'subcommand', 'exit_code', 'state', 'end_reason', 'error', 'cwd_kind')]
            item['signature'] = hashlib.sha256(packed(identity).encode()).hexdigest() if failed else None
            # Equality only in memory: raw cwd never crosses the derived boundary.
            result.append((item, cwd if len(cwd) <= MAX_COMMAND else None))
        return result


def associate(raw, steps):
    groups = collections.defaultdict(list)
    for item, cwd in raw:
        groups[item['thread_id'], item['turn_id']].append((item, cwd))
    for rows in groups.values():
        rows.sort(key=lambda pair: (pair[0]['started_at'] or '', pair[0]['id']))
        for index, (failure, cwd) in enumerate(rows):
            if not failure['failure'] or not failure['started_at'] or not failure['ended_at'] or failure['ended_at'] < failure['started_at'] or not failure['command_known']:
                continue
            for distance, (success, next_cwd) in enumerate(rows[index+1:index+1+steps], 1):
                if not success['success'] or not success['command_known'] or not success['started_at'] or success['started_at'] <= failure['ended_at']:
                    continue
                ratio = difflib.SequenceMatcher(None, failure['command'], success['command'], autojunk=False).ratio()
                same_executable = failure['executable'] == success['executable']
                if same_executable and ratio >= .5 or cwd is not None and cwd == next_cwd and ratio >= .8:
                    success_ref = {k: success[k] for k in ('id', 'thread_id', 'turn_id', 'group_id', 'call_id', 'node_id', 'command')}
                    failure['recovery'] = dict(success_ref, steps=distance, association='same-executable' if same_executable else 'same-cwd', similarity=round(ratio, 3))
                    break
    return sorted((item for item, _ in raw), key=lambda item: item['id'])


def read_catalog(target, options=None):
    with contextlib.closing(connect_readonly(target)) as db:
        meta = json.loads(read_rows(db, "SELECT value FROM meta WHERE key='format'")[0][0])
        if meta['version'] != VERSION or options is not None and meta['options'] != options:
            raise ValueError('Derived format/options changed; explicitly rebuild')
        rows = [json.loads(r[0]) for r in read_rows(db, 'SELECT fact FROM execution ORDER BY id LIMIT ?', (MAX_EXECUTIONS + 1,))]
        if len(rows) > MAX_EXECUTIONS:
            raise ValueError('Derived execution allowance exceeded')
        return meta, rows


def build(data_dir, incremental=False, steps=5, repo_root=None):
    if not 1 <= steps <= 20:
        raise ValueError('Recovery step allowance must be 1..20')
    source, target = paths(data_dir)
    started = time.monotonic()
    options = {'steps': steps, 'repo_root': bool(repo_root)}
    rows = associate(facts(source, repo_root), steps)
    old = {}
    if incremental and target.exists():
        _, prior = read_catalog(target, options)
        old = {row['id']: row for row in prior}
    unchanged = sum(old.get(row['id']) == row for row in rows)
    if not incremental or unchanged != len(rows) or len(old) != len(rows) or not target.exists():
        target.parent.mkdir(mode=0o700, exist_ok=True)
        fd, name = tempfile.mkstemp(prefix='.errors-', suffix='.sqlite3', dir=target.parent)
        os.close(fd)
        try:
            # Connection.__exit__ only commits/rolls back. Close statements, then
            # finish the transaction, then close the file before replace/unlink.
            with contextlib.closing(sqlite3.connect(name)) as db:
                with db, contextlib.closing(db.cursor()) as cursor:
                    cursor.execute('CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)')
                    cursor.execute('CREATE TABLE execution(id TEXT PRIMARY KEY,fact TEXT NOT NULL)')
                    cursor.execute('INSERT INTO meta VALUES(?,?)', ('format', packed({'version': VERSION, 'options': options})))
                    cursor.executemany('INSERT INTO execution VALUES(?,?)', ((r['id'], packed(r)) for r in rows))
            os.replace(name, target)
        except BaseException as original:
            try:
                if os.path.exists(name):
                    os.unlink(name)
            except OSError as cleanup_error:
                # Report both failures without replacing the original exception.
                raise original from cleanup_error
            raise
    return {'executions': len(rows), 'failures': sum(r['failure'] for r in rows), 'unchanged': unchanged,
            'changed': len(rows)-unchanged, 'elapsed_seconds': time.monotonic()-started, 'notice': NOTICE}


def top(data_dir, since=None, limit=10):
    _, target = paths(data_dir)
    _, rows = read_catalog(target)
    cutoff = timestamp(since) if since else None
    if since and not cutoff:
        raise ValueError('Since must be a timezone-qualified RFC3339 timestamp')
    return {'items': statistics(rows, cutoff)[:limit], 'notice': NOTICE}


def statistics(rows, cutoff=None):
    grouped = collections.defaultdict(list)
    for row in rows:
        if row['failure'] and (not cutoff or row['started_at'] and row['started_at'] >= cutoff):
            grouped[row['signature']].append(row)
    result = []
    for signature, group in grouped.items():
        first = group[0]
        dates = sorted(row['started_at'] for row in group if row['started_at'])
        distances = [row['recovery']['steps'] for row in group if row['recovery']]
        result.append({'signature': signature, 'category': first['category'],
                       'shape': {k: first[k] for k in ('executable', 'subcommand', 'exit_code', 'state', 'end_reason', 'error', 'cwd_kind')},
                       'count': len(group), 'threads': len({r['thread_id'] for r in group}),
                       'first_seen': dates[0] if dates else None, 'last_seen': dates[-1] if dates else None,
                       'paired': len(distances), 'paired_fraction': len(distances)/len(group),
                       'mean_steps': sum(distances)/len(distances) if distances else None,
                       'step_counts': dict(sorted(collections.Counter(distances).items())),
                       'examples': sorted(group, key=lambda row: (row['recovery'] is None, row['started_at'] or '', row['id']))[:3]})
    return sorted(result, key=lambda row: (-row['count'], row['signature']))


def similar(data_dir, execution, limit=10):
    _, target = paths(data_dir)
    _, rows = read_catalog(target)
    query = next((r for r in rows if r['id'] == execution and r['failure']), None)
    if query is None:
        raise ValueError('Failure is absent from built snapshot; explicitly build current facts')
    stats = statistics(rows)
    # Rank all signatures, not just the most frequent hundred.
    representatives = {}
    for row in rows:
        if row['failure']:
            representatives.setdefault(row['signature'], row)
    scores = {}
    for signature, row in representatives.items():
        scores[signature] = (4*(row['error'] == query['error']) + 2*(row['executable'] == query['executable'])
                             + (row['category'] == query['category']) + (row['exit_code'] == query['exit_code']))
    selected = sorted(scores, key=lambda key: (-scores[key], key))[:limit]
    by_signature = {row['signature']: row for row in stats}
    return {'execution_id': execution, 'items': [dict(signature=key, score=scores[key],
            examples=by_signature[key]['examples'], statistics=by_signature[key]) for key in selected], 'notice': NOTICE}

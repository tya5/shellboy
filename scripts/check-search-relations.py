#!/usr/bin/env python3
"""Opt-in real embeddings and native search/DAG CLI checks on a new synthetic fixture."""
import argparse
import fcntl
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import select
import sqlite3
import subprocess
import struct
import termios
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture-root', type=Path, required=True)
    parser.add_argument('--binary', type=Path, default=Path('target/release/shellboy'))
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--fixture-embeddings', action='store_true', help='Use explicit local HTTP embedding fixture instead of the real LiteLLM API')
    args = parser.parse_args()
    root = args.fixture_root.resolve()
    if not (root / '.shellboy-visual-fixture').is_file() or not (root / 'data/shellboy.db').is_file():
        parser.error('Create a dedicated visual_fixture first')
    endpoint = root / 'data/endpoint.json'
    if endpoint.exists():
        parser.error('Fixture owner must be stopped')
    summary = 'search-review-summary'
    user = 'visual-composer-node-0'
    answer = 'visual-composer-node-1'
    # Only the explicitly marked disposable fixture may be modified.
    with sqlite3.connect(root / 'data/shellboy.db') as db:
        if db.execute('SELECT 1 FROM node WHERE id=?', (summary,)).fetchone():
            parser.error('Use a newly created fixture; this check is not replayable')
        payload = json.dumps({'type': 'summary', 'text': '検索CLI検証用の合成要約'}, ensure_ascii=False)
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES(?,'summary','2026-10-24T12:00:00+09:00',?,?,?)", (summary, payload, len(payload.encode()), hashlib.sha256(payload.encode()).hexdigest()))
        db.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) VALUES('search-review-replacement',?,'synthetic-search-review','1','1','completed','2026-10-24T12:00:00+09:00')", (summary,))
        db.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision) VALUES('search-review-replacement',0,?,0)", (user,))
    server = None
    requests = []
    embedding_endpoint = 'http://127.0.0.1:4000/v1'
    if args.fixture_embeddings:
        class Embeddings(BaseHTTPRequestHandler):
            def do_POST(self):
                if self.path != '/v1/embeddings':
                    self.send_error(404)
                    return
                request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                requests.append(1)
                dimension = request.get('dimensions') or 1536
                vector = [1.0] + [0.0] * (dimension - 1)
                body = json.dumps({'data': [{'index': index, 'embedding': vector} for index, _ in enumerate(request['input'])]}).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args):
                pass

        server = ThreadingHTTPServer(('127.0.0.1', 0), Embeddings)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        embedding_endpoint = f'http://127.0.0.1:{server.server_port}/v1'
    (root / 'config.toml').write_text('default_connection="visual"\ndefault_model="offline-fixture"\n[connections.visual]\nbase_url="http://127.0.0.1:9/v1"\n[connections.embedding]\nbase_url="' + embedding_endpoint + '"\n[embedding]\nconnection="embedding"\nmodel="text-embedding-3-small"\n')
    binary = str(args.binary.resolve())
    env = {key: value for key, value in os.environ.items() if not key.startswith('SHELLBOY_')}
    env['TERM'] = 'xterm-256color'
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 100, 0, 0))
    process = subprocess.Popen([binary, '--data-dir', str(root / 'data'), '--config', str(root / 'config.toml')], stdin=slave, stdout=slave, stderr=slave, env=env)
    os.close(slave)
    stopped = threading.Event()

    def drain():
        while not stopped.is_set():
            if select.select([master], [], [], .1)[0]:
                try:
                    if not os.read(master, 65536):
                        break
                except OSError:
                    break

    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    evidence = {}

    def cli(label, *words):
        result = subprocess.run([binary, '--data-dir', str(root / 'data'), *words], env=env, capture_output=True, text=True, timeout=60)
        value = json.loads(result.stdout)
        assert value['ok'] and result.returncode == 0, value
        evidence[label] = value
        return value['result']

    try:
        deadline = time.monotonic() + 15
        while not endpoint.exists():
            assert process.poll() is None, 'Dedicated owner exited'
            assert time.monotonic() < deadline, 'Owner startup timed out'
            time.sleep(.05)
        time.sleep(.3)
        admitted = cli('backfill', 'index', 'backfill', '--source', user, '--source', answer)
        assert admitted['sources'] == 2, admitted
        deadline = time.monotonic() + 90
        while True:
            status = cli('index_status', 'index', 'status')
            if status['done'] == 2:
                break
            assert status['failed'] == 0 and time.monotonic() < deadline, status
            time.sleep(.2)
        queries = 0

        def search(label, *filters):
            nonlocal queries
            value = cli(label, 'history', 'search', '--query', 'H01 はじめの依頼', '--limit', '10', *filters)
            queries += 1
            return value

        hits = search('filtered', '--thread', 'visual-composer', '--kind', 'user', '--exclude-kind', 'tool_result', '--from', '2026-10-24T03:00:00Z', '--to', '2026-10-24T03:00:01Z')
        assert [hit['node_id'] for hit in hits] == [user], hits
        hit = hits[0]
        assert hit['thread_scope'] == {'thread_id': 'visual-composer', 'on_current_path': True}, hit
        assert hit['provenance'] == {'kind': 'user', 'created_at': '2026-10-24T12:00:00+09:00', 'source_revision': 0}, hit
        assert hit['spans'] and all(span['source_revision'] == 0 and len(span['content_hash']) == 64 for span in hit['spans']), hit
        for span in hit['spans']:
            segment = 'H01 はじめの依頼'[span['start']:span['end']]
            assert segment and hashlib.sha256(segment.encode()).hexdigest() == span['content_hash'], span
        assert [h['node_id'] for h in search('assistant_kind', '--kind', 'assistant_response')] == [answer]
        assert search('different_thread', '--thread', 'visual-markdown') == []
        assert search('excluded_kind', '--kind', 'user', '--exclude-kind', 'user') == []
        assert search('exclusive_to', '--to', '2026-10-24T12:00:00+09:00') == []
        original = cli('original', 'history', 'get', '--node', user)
        assert 'H01 はじめの依頼' in original['text'] and original['provenance'] == hit['provenance'], original
        next_page = cli('next', 'history', 'relations', '--node', user, '--relation', 'next', '--limit', '1')
        assert [entry['node_id'] for entry in next_page['items']] == [answer], next_page
        previous = cli('previous', 'history', 'relations', '--node', answer, '--relation', 'previous')
        assert [entry['node_id'] for entry in previous['items']] == [user], previous
        sources = cli('sources', 'history', 'relations', '--node', summary, '--relation', 'sources')
        assert [entry['node_id'] for entry in sources['items']] == [user], sources
        assert sources['items'][0]['source_revision'] == 0
        replacements = cli('replacements', 'history', 'relations', '--node', user, '--relation', 'replacements')
        assert [entry['node_id'] for entry in replacements['items']] == [summary], replacements
        with sqlite3.connect(root / 'data/shellboy.db') as db:
            jobs, attempts = db.execute('SELECT count(*),sum(attempts) FROM embedding_job').fetchone()
        assert jobs == 2, (jobs, attempts)
        verification = {'binary_sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest(), 'synthetic_data_only': True, 'embedding_mode': 'fixture' if args.fixture_embeddings else 'real_api', 'embedding_endpoint': embedding_endpoint, 'embedding_model': 'text-embedding-3-small', 'indexed_sources': 2, 'embedding_jobs': jobs, 'index_api_attempts': attempts, 'successful_query_embeddings': queries, 'fixture_http_requests': len(requests) if args.fixture_embeddings else None, 'generation_requests': 0, 'filtered_hit': hit, 'relation_checks': {'next': [answer], 'previous': [user], 'sources': [user], 'replacements': [summary]}, 'normal_shutdown': True}
    finally:
        if process.poll() is None:
            os.write(master, b'\x11')
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=5)
                raise
        stopped.set()
        reader.join(timeout=1)
        os.close(master)
        if server is not None:
            server.shutdown()
            server.server_close()
    assert process.returncode == 0, process.returncode
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(verification, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'indexed_sources': jobs, 'query_embeddings': queries, 'cli_checks': len(evidence), 'normal_shutdown': True}))


if __name__ == '__main__':
    main()

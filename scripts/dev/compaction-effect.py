#!/usr/bin/env python3
"""Measure provider-reported input around a compaction cut-over.

Request counts, distributions, and inter-compaction distances cover conversation
model_attempts. Summary-model calls are persisted only as per-compaction totals,
so they cannot be included in per-request distributions without estimation.

Usage: compaction-effect.py DB_PATH CUTOVER_ISO8601 [--threshold TOKENS]
The threshold defaults to Shellboy's 200000-token default. Supply the effective
configured value when it differs; per-connection overrides are not in the DB.
Only aggregate numeric data is emitted. No message or request bodies are read.
"""
import argparse
import datetime as dt
import json
import math
import sqlite3
import statistics
import tempfile
from contextlib import closing, contextmanager
from pathlib import Path

DEFAULT_THRESHOLD = 200_000
HIGH_CONTEXT = 272_000
OUTCOMES = ('adopted', 'rejected', 'no_safe_reduction')


def parse_timestamp(value):
    parsed = dt.datetime.fromisoformat(value.replace('Z', '+00:00'))
    if parsed.tzinfo is None:
        raise ValueError('cut-over timestamp must include a timezone')
    return parsed.astimezone(dt.timezone.utc)


def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(0, math.ceil(fraction * len(ordered)) - 1)]


def reported_int(value):
    return value if type(value) is int and 0 <= value <= (2**63 - 1) else None


@contextmanager
def snapshot_database(source):
    source = Path(source).expanduser().resolve(strict=True)
    if not source.is_file():
        raise ValueError('DB path must be a regular file')
    with tempfile.TemporaryDirectory(prefix='shellboy-compaction-effect-') as temp:
        snapshot = Path(temp) / 'snapshot.db'
        uri = source.as_uri() + '?mode=ro'
        with closing(sqlite3.connect(uri, uri=True)) as live, closing(sqlite3.connect(snapshot)) as copy:
            live.backup(copy)
        yield snapshot


def parse_object(raw):
    try:
        value = json.loads(raw) if raw else {}
    except (TypeError, ValueError):
        return {}
    return value if isinstance(value, dict) else {}


def read_attempts(path):
    with closing(sqlite3.connect(Path(path).as_uri() + '?mode=ro', uri=True)) as db:
        db.row_factory = sqlite3.Row
        version = db.execute('PRAGMA user_version').fetchone()[0]
        columns = {r['name'] for r in db.execute('PRAGMA table_info(model_attempt)')}
        required = {'id', 'turn_id', 'profile_id', 'model_id', 'usage_json', 'error_json', 'started_at'}
        if not required <= columns:
            raise ValueError('DB does not have the expected current model_attempt schema')
        # Extract only whitelisted numbers and the bounded compaction observation;
        # never select raw usage/error JSON, which may contain response text.
        rows = db.execute("""
            SELECT a.id,a.turn_id,t.thread_id,a.profile_id,a.model_id,a.started_at,
              CASE WHEN json_valid(a.usage_json) THEN CASE
                WHEN json_type(a.usage_json,'$.input_tokens')='integer'
                 AND typeof(json_extract(a.usage_json,'$.input_tokens'))='integer'
                 AND json_extract(a.usage_json,'$.input_tokens')>=0
                THEN json_extract(a.usage_json,'$.input_tokens') END END AS input_tokens,
              CASE WHEN json_valid(a.usage_json) THEN CASE
                WHEN json_type(a.usage_json,'$.input_tokens_details.cached_tokens')='integer'
                 AND typeof(json_extract(a.usage_json,'$.input_tokens_details.cached_tokens'))='integer'
                 AND json_extract(a.usage_json,'$.input_tokens_details.cached_tokens')>=0
                THEN json_extract(a.usage_json,'$.input_tokens_details.cached_tokens') END END AS cached_tokens,
              CASE WHEN json_valid(a.error_json) THEN CASE
                WHEN json_type(a.error_json,'$.automatic_trim_attempt')='object'
                THEN json_extract(a.error_json,'$.automatic_trim_attempt') END END AS compaction_json
            FROM model_attempt a JOIN turn t ON t.id=a.turn_id
            ORDER BY a.started_at,a.id
        """).fetchall()
    return version, rows


def normalize(row):
    observation = parse_object(row['compaction_json'])
    if not observation:
        observation = None
    return {
        'id': row['id'], 'turn': row['turn_id'], 'thread': row['thread_id'],
        'profile': row['profile_id'], 'model': row['model_id'], 'started': row['started_at'],
        'input': reported_int(row['input_tokens']),
        'cached': reported_int(row['cached_tokens']),
        'compaction': observation,
    }

def summarize(rows, side, threshold):
    requests = [r for r in rows if r['side'] == side]
    known = [r['input'] for r in requests if r['input'] is not None]
    ratio_pairs = [r for r in requests if r['input'] is not None and r['input'] > 0 and r['cached'] is not None]
    above_threshold = sum(n > threshold for n in known)
    above_high = sum(n > HIGH_CONTEXT for n in known)
    attempts = [r for r in requests if r['compaction'] is not None]
    outcomes = {key: sum(r['compaction'].get('outcome') == key for r in attempts) for key in OUTCOMES}
    outcomes['other_or_unknown'] = len(attempts) - sum(outcomes.values())

    # Count only complete intervals between consecutive compaction attempts.
    # Keep thread history across turns; a turn boundary does not reset context.
    gaps = []
    by_thread = {}
    for row in requests:
        by_thread.setdefault(row['thread'], []).append(row)
    for thread_rows in by_thread.values():
        thread_rows.sort(key=lambda r: (parse_timestamp(r['started']), r['id']))
        request_index = 0
        previous_compaction_index = None
        for row in thread_rows:
            request_index += 1
            if row['compaction'] is not None:
                if previous_compaction_index is not None:
                    gaps.append(request_index - previous_compaction_index - 1)
                previous_compaction_index = request_index

    return {
        'model_requests': len(requests),
        'provider_input_tokens': {
            'known_requests': len(known), 'unknown_requests': len(requests) - len(known),
            'median': statistics.median(known) if known else None, 'p90': percentile(known, .9),
            'above_compaction_threshold': {
                'threshold': threshold, 'requests': above_threshold,
                'share': above_threshold / len(known) if known else None},
            'above_272000': {
                'requests': above_high, 'share': above_high / len(known) if known else None}},
        'cached_over_input': {
            'known_pairs': len(ratio_pairs), 'unknown_pairs': len(requests) - len(ratio_pairs),
            'ratio': (sum(r['cached'] for r in ratio_pairs) / sum(r['input'] for r in ratio_pairs)) if ratio_pairs else None},
        'compaction_attempts': {'total': len(attempts), 'outcomes': outcomes},
        'requests_between_compactions': {
            'intervals': len(gaps), 'median': statistics.median(gaps) if gaps else None,
            'p90': percentile(gaps, .9)},
    }


def analyze(rows, cutover, threshold):
    cut = parse_timestamp(cutover)
    normalized = [normalize(row) for row in rows]
    for row in normalized:
        row['side'] = 'before' if parse_timestamp(row['started']) < cut else 'after'
    models = sorted({(row['profile'], row['model']) for row in normalized})
    return {
        side: {
            f'{profile}/{model}': summarize(
                [row for row in normalized if (row['profile'], row['model']) == (profile, model)],
                side, threshold)
            for profile, model in models
        }
        for side in ('before', 'after')
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('db', type=Path)
    parser.add_argument('cutover', help='timezone-qualified ISO-8601 timestamp')
    parser.add_argument('--threshold', type=int, default=DEFAULT_THRESHOLD,
                        help='effective auto_at_tokens threshold (default: 200000)')
    args = parser.parse_args(argv)
    if args.threshold < 0:
        parser.error('--threshold must be non-negative')
    parse_timestamp(args.cutover)
    with snapshot_database(args.db) as snapshot:
        version, rows = read_attempts(snapshot)
    report = {
        'schema_version': version, 'cutover': args.cutover,
        'compaction_threshold': args.threshold, 'high_context_threshold': HIGH_CONTEXT,
        'usage_source': 'provider-reported only',
        'request_scope': 'conversation model_attempt rows; compaction-summary requests are not individually stored',
        'periods': analyze(rows, args.cutover, args.threshold),
    }
    print(json.dumps(report, indent=2, sort_keys=True, allow_nan=False))


if __name__ == '__main__':
    main()

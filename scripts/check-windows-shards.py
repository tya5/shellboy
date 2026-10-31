#!/usr/bin/env python3
"""Audit the two native Windows results against their complete Cargo inventories."""
import argparse
import json
from pathlib import Path
from windows_shards import audit

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('results', type=Path, nargs=2, help='shard 1 and shard 2 results.json')
args = parser.parse_args()
try:
    print(json.dumps(audit([json.loads(path.read_text(encoding='utf-8')) for path in args.results])))
except (KeyError, ValueError) as error:
    parser.exit(1, f'Windows inventory audit failed: {error}\n')

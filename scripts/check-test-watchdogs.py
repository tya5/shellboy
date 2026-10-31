#!/usr/bin/env python3
"""Reject raw test timeouts and real-clock deadline assertions outside the ledger."""
import json
from pathlib import Path
from watchdog_policy import check

root = Path(__file__).resolve().parent.parent
errors = check(root, json.loads((root / 'scripts/test-watchdog-allowances.json').read_text()))
if errors:
    raise SystemExit('\n'.join(errors))
print('Test watchdog policy: no unapproved real-time guards')

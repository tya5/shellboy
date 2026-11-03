#!/usr/bin/env python3
"""Build a disposable database from current schema and test compaction-effect."""
import importlib.util
import json
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/dev/compaction-effect.py'
MIGRATIONS = ROOT / 'migrations'
spec = importlib.util.spec_from_file_location('compaction_effect', SCRIPT)
effect = importlib.util.module_from_spec(spec)
spec.loader.exec_module(effect)


class CompactionEffectTests(unittest.TestCase):
    def fixture(self, path):
        db = sqlite3.connect(path)
        for migration in sorted(MIGRATIONS.glob('*.sql')):
            db.executescript(migration.read_text())
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('thread','/','/AGENTS.md','profile','default','2026-11-03T00:00:00Z')")
        db.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('run','thread','completed','2026-11-03T00:00:00Z')")
        for i, started in enumerate((
            '2026-11-02T00:00:00Z','2026-11-02T00:01:00Z','2026-11-02T00:02:00Z',
            '2026-11-04T00:00:00Z','2026-11-04T00:01:00Z','2026-11-04T00:02:00Z'), 1):
            turn = 'before-turn' if i <= 3 else 'after-turn'
            if i in (1, 4):
                db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES(?,?,?,'completed',?)", (turn,'thread','run',started))
            usage = [
                {'input_tokens':100,'input_tokens_details':{'cached_tokens':25}},
                {'input_tokens':200000,'input_tokens_details':{'cached_tokens':100000}},
                {'input_tokens':300000},
                {'input_tokens':150,'input_tokens_details':{'cached_tokens':30}},
                {},
                {'input_tokens':400000,'input_tokens_details':{'cached_tokens':200000}},
            ][i-1]
            error = {}
            if i in (2, 3, 5):
                error['automatic_trim_attempt'] = {
                    'reason':'proactive_threshold' if i != 3 else 'overflow_retry',
                    'outcome':{2:'adopted',3:'rejected',5:'no_safe_reduction'}[i],
                }
            db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,error_json,started_at) VALUES(?,?,0,0,'profile',?,0,'completed','{}',?,?,?)", (f'a{i}',turn,'m1' if i < 4 else 'm2',json.dumps(usage),json.dumps(error),started))
        db.commit()
        return db

    def test_schema_fixture_aggregates_provider_values_and_outcomes(self):
        with tempfile.TemporaryDirectory() as temp:
            db_path = Path(temp) / 'fixture.db'
            db = self.fixture(db_path)
            before = db.execute('SELECT count(*) FROM model_attempt').fetchone()[0]
            version, rows = effect.read_attempts(db_path)
            result = effect.analyze(rows, '2026-11-03T00:00:00Z', 200000)
            self.assertEqual(version, 25)
            self.assertEqual(db.execute('SELECT count(*) FROM model_attempt').fetchone()[0], before)
            self.assertEqual(result['before']['profile/m1']['model_requests'], 3)
            self.assertEqual(result['before']['profile/m1']['provider_input_tokens']['median'], 200000)
            self.assertEqual(result['before']['profile/m1']['provider_input_tokens']['p90'], 300000)
            self.assertEqual(result['before']['profile/m1']['provider_input_tokens']['above_compaction_threshold']['share'], 1 / 3)
            self.assertEqual(result['before']['profile/m1']['provider_input_tokens']['above_272000']['requests'], 1)
            self.assertEqual(result['before']['profile/m1']['compaction_attempts']['outcomes']['adopted'], 1)
            self.assertEqual(result['before']['profile/m1']['compaction_attempts']['outcomes']['rejected'], 1)
            self.assertEqual(result['before']['profile/m1']['requests_between_compactions']['intervals'], 1)
            self.assertEqual(result['before']['profile/m1']['requests_between_compactions']['median'], 0)
            self.assertEqual(result['before']['profile/m1']['cached_over_input']['ratio'], 100025 / 200100)
            self.assertEqual(result['after']['profile/m2']['provider_input_tokens']['median'], 200075.0)
            self.assertEqual(result['after']['profile/m2']['provider_input_tokens']['unknown_requests'], 1)
            self.assertEqual(result['after']['profile/m2']['compaction_attempts']['outcomes']['no_safe_reduction'], 1)
            db.close()

    def test_cli_outputs_aggregates_without_mutating_fixture(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'fixture.db'
            db = self.fixture(path)
            db.close()
            before = path.read_bytes()
            result = subprocess.run([sys.executable, str(SCRIPT), str(path), '2026-11-03T00:00:00Z'], check=True, capture_output=True, text=True)
            output = json.loads(result.stdout)
            self.assertEqual(output['periods']['before']['profile/m1']['model_requests'], 3)
            self.assertEqual(path.read_bytes(), before)
            self.assertNotIn('AGENTS.md', result.stdout)

    def test_unknown_and_invalid_values_are_not_coerced(self):
        for value in (None, True, -1, 3.0, '3', 2**63):
            self.assertIsNone(effect.reported_int(value))
        self.assertIsNone(effect.percentile([], .9))
        self.assertEqual(effect.statistics.median([100, 200]), 150)
        with self.assertRaises(ValueError):
            effect.parse_timestamp('2026-11-03T00:00:00')


if __name__ == '__main__':
    unittest.main()

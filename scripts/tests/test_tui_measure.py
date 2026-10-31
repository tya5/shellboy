"""Fast stdlib-only measurement gates; never runs Cargo, Git, a TUI or a long window."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC=importlib.util.spec_from_file_location('tui_measure',Path(__file__).resolve().parents[1]/'tui-measure.py')
M=importlib.util.module_from_spec(SPEC);SPEC.loader.exec_module(M)

class MeasurementTests(unittest.TestCase):
    def test_multiple_background_calls_have_distinct_tool_pair_identities(self):
        import json
        calls=M.background_calls('fixed command',3)
        self.assertEqual(len({c['call_id'] for c in calls}),3)
        for call in calls:
            self.assertEqual(call['id'],call['call_id'])
            self.assertEqual(json.loads(call['arguments']),
                {'command':'fixed command','background':True,'stdin_open':True})
        for count in [0,5,True,1.5]:
            with self.assertRaisesRegex(ValueError,'producer count'):
                M.background_calls('fixed command',count)

    def test_readiness_requires_all_distinct_producers_in_same_completed_turn(self):
        rows=[(f'e{i}','run','turn','completed','live') for i in range(3)]
        self.assertEqual(M.saved_producers(rows,3)['executions'],['e0','e1','e2'])
        self.assertIsNone(M.saved_producers(rows[:2],3))
        self.assertIsNone(M.saved_producers(rows+[rows[0]],3))
        self.assertIsNone(M.saved_producers(rows[:2]+[rows[0]],3))
        for changed in [('e2','other','turn','completed','live'),
                        ('e2','run','other','completed','live'),
                        ('e2','run','turn','running','live'),
                        ('e2','run','turn','completed','stopped')]:
            self.assertIsNone(M.saved_producers(rows[:2]+[changed],3))

    def test_bounded_capture_retains_actual_tail(self):
        capture=M.BoundedCapture(5)
        capture.append(b'123');capture.append(b'4567');capture.append(b'89')
        self.assertEqual(capture.bytes(),b'56789');self.assertEqual(capture.total,9)
        self.assertEqual(capture.size,5)

    def test_clipped_wide_screen_text_never_uses_pyte_display(self):
        from types import SimpleNamespace
        screen=M.Screen.__new__(M.Screen)
        screen.screen=SimpleNamespace(lines=1,columns=3,
            buffer=[[SimpleNamespace(data=c) for c in ['a','b','界']]])
        # No pyte dependency is needed for the bounded grid export contract.
        with patch.dict('sys.modules',{'wcwidth':SimpleNamespace(wcswidth=lambda c:2 if c=='界' else 1)}):
            self.assertEqual(screen.text(),'ab界')

    def test_mask_keeps_offsets_and_excludes_test_module(self):
        s='// Connection::open_with_flags(x,y)\n#[cfg(test)] mod tests { fn a(){Connection::open_with_flags(x,y);} }\nfn actual(){Connection::open_with_flags(x,y); let x=r#"()"#;}'
        mask=M.production_mask(s)
        self.assertEqual(len(s),len(mask))
        self.assertEqual(mask.count('Connection::open_with_flags'),1)
        start=mask.index('open_with_flags(')+len('open_with_flags(')
        self.assertEqual(s[M.closing(mask,start)],')')

    def test_dirty_source_rejected_before_reading_files(self):
        with patch.object(M,'checked',side_effect=['a'*40,' M src/lib.rs']):
            with self.assertRaisesRegex(ValueError,'dirty'):
                M.source_witness(Path('.'))

    def test_fake_git_hash_is_rejected(self):
        with patch.object(M,'checked',return_value='caller-supplied-name'):
            with self.assertRaisesRegex(ValueError,'HEAD'):M.source_witness(Path('.'))

    def test_cpu_draw_reads_are_observations_not_gates(self):
        manifest={'source':{'head':'a'*40},'harness_sha256':'h'}
        result={'kind':'p2','exit':0,'measurement':{'window_model_requests':0},
          'native_metrics':{'source_witness':'a'*40,'harness_witness':'h','wall_seconds':600,
           'db_categories':{'ui':{'executed':100000}},'read_only_connections':{'executed':100000},
           'ui_read_tasks_admitted':100000,'draws':100000,'cpu_percent_one_core':999}}
        self.assertEqual(M.validate_result(result,manifest,600),(True,[]))
        result['native_metrics']['source_witness']='b'*40
        self.assertIn('embedded source mismatch',M.validate_result(result,manifest,600)[1])

    def test_truncated_window_and_missing_counter_fail(self):
        manifest={'source':{'head':'a'},'harness_sha256':'h'}
        result={'kind':'p5','exit':0,'measurement':{'window_model_requests':0},
          'native_metrics':{'source_witness':'a','harness_witness':'h','wall_seconds':1,'output_commits':0}}
        ok,errors=M.validate_result(result,manifest,60)
        self.assertFalse(ok);self.assertIn('truncated measurement window',errors)
        self.assertIn('hidden producer produced no committed output',errors)
        self.assertIn('missing counter draws',errors)

    def test_modified_source_fails_verification(self):
        with tempfile.TemporaryDirectory() as t:
            root=Path(t);(root/'x').write_text('changed')
            M.write_json(root/'manifest.json',{'source':{'head':'a','files':{'x':'wrong'}},'modified':{},'harness_sha256':M.harness_witness()})
            with patch.object(M,'checked',return_value='a'):
                with self.assertRaisesRegex(ValueError,'source witness'):M.verify(root,root)

    def test_unsupported_hook_fails_without_source_writes(self):
        with tempfile.TemporaryDirectory() as t:
            root=Path(t);(root/'src').mkdir()
            (root/'src/lib.rs').write_text('pub fn untouched() {}')
            (root/'src/ui').mkdir();(root/'src/ui/redraw.rs').write_text('unsupported draw API')
            with self.assertRaisesRegex(ValueError,'unsupported source hook'):
                M.instrument(root,{'head':'a'*40},'h')
            self.assertEqual((root/'src/lib.rs').read_text(),'pub fn untouched() {}')
            self.assertFalse((root/'src/ci_tui_measure.rs').exists())

    def test_summary_does_not_invent_missing_measurements(self):
        with tempfile.TemporaryDirectory() as t:
            root=Path(t);M.summary(root)
            import json
            self.assertEqual(json.loads((root/'summary.json').read_text())['measurements'],[])
            self.assertEqual(json.loads((root/'diff.json').read_text())['same_job_deltas'],[])

    def test_full_measure_is_ci_only(self):
        with patch.dict(M.os.environ,{},clear=True):
            with self.assertRaisesRegex(ValueError,'manual CI'):M.pair(Path('.'),Path('.'),Path('.'),80)

if __name__=='__main__':unittest.main()

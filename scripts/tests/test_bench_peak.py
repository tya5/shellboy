"""Observed maxima never hide unavailable or failed sampling."""
from pathlib import Path
import sys
import threading
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'bench'))
from peak import PeakSampler


class PeakTests(unittest.TestCase):
    def test_maximum_of_recorded_samples(self):
        values = iter([2, 9, 4])
        done = threading.Event()
        def sample():
            try:
                value = next(values)
                if value == 4:
                    done.set()
                return {'pss_kib': value}
            except StopIteration:
                return {'pss_kib': 4}
        with PeakSampler(sample, .002) as peak:
            self.assertTrue(done.wait(5))
        data = peak.result()
        self.assertEqual(data['observed_max_pss_kib'], 9)
        self.assertGreaterEqual(len(data['samples']), 3)
        self.assertTrue(all(s['end_ms'] >= s['start_ms'] >= 0 for s in data['samples']))

    def test_churn_is_counted_not_zero(self):
        observed = threading.Event()
        def sample():
            observed.set()
            raise RuntimeError('Process tree changed during snapshot')
        with PeakSampler(sample, .002) as peak:
            self.assertTrue(observed.wait(5))
        self.assertGreater(peak.result()['churn_failures'], 0)
        self.assertIsNone(peak.result()['observed_max_pss_kib'])

    def test_unexpected_failure_is_not_hidden(self):
        observed = threading.Event()
        def sample():
            observed.set()
            raise PermissionError('unreadable')
        with PeakSampler(sample, .002) as peak:
            self.assertTrue(observed.wait(5))
        with self.assertRaisesRegex(RuntimeError, 'PermissionError'):
            peak.result()


class RepeatSummaryTests(unittest.TestCase):
    def test_summary_keeps_counts_and_missing_values(self):
        from compare import summarize_repeats
        def case(n, base, peak):
            return {'n': n, 'baseline': {'pss_kib': base},
                    'in_flight_pss': {'observed_max_pss_kib': peak}}
        result = summarize_repeats([case(1, 9, 10), case(1, 1, 30), case(1, 5, 20),
                                    case(5, 7, None)])
        self.assertEqual(result[0]['baseline_pss_kib'], {'min': 1, 'median': 5, 'max': 9})
        self.assertEqual(result[0]['observed_in_flight_max_pss_kib'], {'min': 10, 'median': 20, 'max': 30})
        self.assertEqual(result[0]['repeats'], 3)
        self.assertIsNone(result[1]['observed_in_flight_max_pss_kib'])

    def test_tui_summary_and_unavailable_peak(self):
        from compare import summarize_repeats
        def case(value):
            return {'n': 1, 'baseline': {'pss_kib': 10},
                    'in_flight_pss': {'observed_max_pss_kib': 20},
                    'baseline_one_tui': {'pss_kib': 15, 'workload': {
                        'in_flight_pss': {'observed_max_pss_kib': value}}}}
        result = summarize_repeats([case(8), case(4), case(6)])[0]
        self.assertEqual(result['one_tui_observed_in_flight_max_pss_kib'],
                         {'min': 4, 'median': 6, 'max': 8})
        self.assertIsNone(summarize_repeats([case(8), case(None)])[0]
                          ['one_tui_observed_in_flight_max_pss_kib'])


if __name__ == '__main__':
    unittest.main()

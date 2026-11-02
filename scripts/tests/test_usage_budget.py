import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'bench'))
from usage_budget import Budget, StopExperiment, UsageStream, cache_ratios, normalize_usage, validate_request

H = {'session': True, 'trace': False, 'shellboy': False}


class BudgetTests(unittest.TestCase):
    def test_reserve_and_actual_usage(self):
        budget = Budget(limit=110_000)
        budget.admit()
        budget.accept('a', 6000, 3000, 4, H)
        self.assertEqual(budget.total, 6004)
        budget.admit()
        budget.accept('a', 100, None, 2, H)
        self.assertEqual(budget.total, 6106)

    def test_refuses_first_and_later_when_reserve_insufficient(self):
        with self.assertRaises(StopExperiment):
            Budget(limit=102_047).admit()
        budget = Budget(limit=110_000)
        budget.admit()
        budget.accept('a', 45_000, 0, 1, H)
        with self.assertRaises(StopExperiment):
            budget.admit()

    def test_usage_absence_and_concurrency_fail_closed(self):
        budget = Budget()
        budget.admit()
        with self.assertRaises(StopExperiment):
            budget.accept('a', None, None, 1, H)
        with self.assertRaises(StopExperiment):
            budget.admit()
        budget = Budget()
        budget.admit()
        with self.assertRaises(StopExperiment):
            budget.admit()

    def test_cached_missing_and_first_exclusion(self):
        rows = [{'agent': 'a', 'input': 10, 'cached': None},
                {'agent': 'a', 'input': 20, 'cached': 10},
                {'agent': 'a', 'input': 30, 'cached': 0},
                {'agent': 'b', 'input': 2, 'cached': 2}]
        self.assertEqual(cache_ratios(rows, 'a'), {'all': None, 'after_first': .2})
        self.assertEqual(cache_ratios(rows, 'b'), {'all': 1, 'after_first': None})

    def test_error_stops_and_overshoot_retains_reported_usage(self):
        budget = Budget(limit=102_048)
        budget.admit()
        with self.assertRaises(StopExperiment):
            budget.accept('a', 102_048, 0, 1, H)
        self.assertEqual(budget.total, 102_049)
        self.assertEqual(len(budget.rows), 1)
        budget = Budget()
        budget.admit()
        with self.assertRaisesRegex(StopExperiment, 'internal retry usage unknown'):
            budget.error()


class UsageNormalizationTests(unittest.TestCase):
    def test_cached_missing_is_unknown_not_zero(self):
        self.assertEqual(normalize_usage('responses', {'input_tokens': 10, 'output_tokens': 2}), (10, None, 2))
        self.assertEqual(normalize_usage('chat', {'prompt_tokens': 10, 'completion_tokens': 2,
                                                 'prompt_tokens_details': {'cached_tokens': 0}}), (10, 0, 2))

    def test_messages_sums_disjoint_input_counters(self):
        data = {'input_tokens': 10, 'cache_read_input_tokens': 20,
                'cache_creation_input_tokens': 3, 'output_tokens': 4}
        self.assertEqual(normalize_usage('messages', data), (33, 20, 4))
        del data['cache_creation_input_tokens']
        with self.assertRaises(StopExperiment):
            normalize_usage('messages', data)


class UsageStreamTests(unittest.TestCase):
    def test_responses_requires_completed_event(self):
        stream = UsageStream('responses')
        with self.assertRaises(StopExperiment):
            stream.done()
        stream.event({'type': 'response.completed', 'response': {'usage': {
            'input_tokens': 10, 'input_tokens_details': {'cached_tokens': 5}, 'output_tokens': 3}}})
        self.assertEqual(stream.done(), (10, 5, 3))

    def test_messages_stream_combines_start_and_delta(self):
        stream = UsageStream('messages')
        stream.event({'type': 'message_start', 'message': {'usage': {
            'input_tokens': 3, 'cache_read_input_tokens': 5, 'cache_creation_input_tokens': 0}}})
        stream.event({'type': 'message_delta', 'usage': {'output_tokens': 2}})
        with self.assertRaises(StopExperiment):
            stream.done()
        stream.event({'type': 'message_stop'})
        self.assertEqual(stream.done(), (8, 5, 2))

    def test_chat_needs_end_marker(self):
        stream = UsageStream('chat')
        stream.event({'usage': {'prompt_tokens': 10, 'completion_tokens': 2}})
        with self.assertRaises(StopExperiment):
            stream.done()
        stream.end_marker()
        self.assertEqual(stream.done(), (10, None, 2))

    def test_error_never_returns_usage(self):
        stream = UsageStream('chat')
        with self.assertRaises(StopExperiment):
            stream.event({'error': {'message': 'redacted'}})
        with self.assertRaises(StopExperiment):
            stream.done()


class RequestGuardTests(unittest.TestCase):
    def test_exact_path_model_and_bounded_output(self):
        body = {'model': 'gpt-6.1-terra', 'max_output_tokens': 2048, 'stream': True}
        self.assertEqual(validate_request('/v1/responses', body), 'responses')
        for patch in ({'model': 'other'}, {'max_output_tokens': 2049}, {'stream': False},
                      {'fallbacks': ['other']}):
            with self.subTest(patch=patch), self.assertRaises(StopExperiment):
                validate_request('/v1/responses', {**body, **patch})
        with self.assertRaises(StopExperiment):
            validate_request('/v1/unknown', body)

    def test_messages_and_chat_caps(self):
        body = {'model': 'gpt-6.1-terra', 'max_tokens': 10, 'stream': True}
        self.assertEqual(validate_request('/v1/messages', body), 'messages')
        self.assertEqual(validate_request('/v1/chat/completions', body), 'chat')


if __name__ == '__main__':
    unittest.main()

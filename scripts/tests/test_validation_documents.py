"""Keep status overviews and validation evidence in distinct document roles."""
from pathlib import Path
import unittest


class ValidationDocumentRoleTests(unittest.TestCase):
    def test_status_cannot_replace_the_validation_record(self):
        root = Path(__file__).resolve().parents[2]
        acceptance = (root / 'docs/validation/acceptance.md').read_text(encoding='utf-8')
        status = (root / 'docs/development/status.md').read_text(encoding='utf-8')
        self.assertTrue(acceptance.startswith('# Validation record\n'))
        self.assertTrue(status.startswith('# Development status\n'))
        self.assertNotEqual(acceptance, status)
        self.assertNotIn('\n# Development status\n', acceptance)
        self.assertNotIn('\n# Validation record\n', status)

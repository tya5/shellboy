"""Cell-level VT contracts, independent of a terminal, Rust, or third-party modules."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('vt_screen', Path(__file__).resolve().parents[1] / 'dev/vt_screen.py')
vt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vt)


class VtScreenTests(unittest.TestCase):
    def test_cursor_erase_and_split_control_sequences(self):
        screen = vt.Screen(12, 3)
        for chunk in ['abc', '\x1b[2;', '3Hxy', '\x1b[1G!', '\x1b[K']:
            screen.feed(chunk)
        self.assertEqual(screen.text(), 'abc         \n!           \n            ')
        screen.feed('\x1b[2J\x1b[Hreset')
        self.assertTrue(screen.text().startswith('reset'))

    def test_color_reset_and_indexed_gray(self):
        screen = vt.Screen(10, 2)
        screen.feed('\x1b[38;2;1;2;3m\x1b[48;5;235mA\x1b[0mB')
        self.assertEqual(screen.buffer[0][0], vt.Cell('A', '010203', '262626'))
        self.assertEqual(screen.buffer[0][1], vt.Cell('B'))
        self.assertEqual(vt.indexed_color(253), 'dadada')

    def test_wide_combining_and_wide_cell_overwrite(self):
        screen = vt.Screen(8, 2)
        screen.feed('\u65e5\u672ce\u0301')
        self.assertEqual([cell.data for cell in screen.buffer[0]][:5], ['\u65e5', '', '\u672c', '', 'e\u0301'])
        screen.feed('\x1b[1;2HX')
        self.assertTrue(screen.text().startswith(' X\u672ce\u0301'))
        screen.feed('\x1b[2;1H\U0001f469\u200d\U0001f4bb!')
        self.assertEqual(screen.buffer[1][0].data, '\U0001f469\u200d\U0001f4bb')
        self.assertEqual(screen.buffer[1][2].data, '!')

    def test_wrap_and_scroll(self):
        screen = vt.Screen(3, 2)
        screen.feed('abcdefg')
        self.assertEqual(screen.text(), 'def\ng  ')

    def test_osc_hyperlink_and_clipboard_bytes_are_not_cell_text(self):
        screen = vt.Screen(10, 2)
        for part in ['\x1b]8;;https://example.invalid', '\x1b', '\\link\x1b]8;;\x07', '\x1b]52;c;YWJj\x1b\\!']:
            screen.feed(part)
        self.assertTrue(screen.text().startswith('link!'))
        self.assertNotIn('YWJj', screen.text())

    def test_unknown_cell_operations_and_size_fail_closed(self):
        for size in [(0, 2), (401, 2), (2, 201)]:
            with self.assertRaises(ValueError):
                vt.Screen(*size)
        screen = vt.Screen(10, 2)
        with self.assertRaises(ValueError):
            screen.feed('\x1b[1P')
        with self.assertRaises(ValueError):
            vt.indexed_color(256)

    def test_alternate_screen_wrap_mode_and_cluster_limits(self):
        screen = vt.Screen(5, 2)
        screen.feed('main\x1b[?1049hALT')
        self.assertTrue(screen.text().startswith('ALT'))
        screen.feed('\x1b[?1049l')
        self.assertTrue(screen.text().startswith('main'))
        screen.feed('\x1b[?7lxy')
        self.assertTrue(screen.text().startswith('mainy'))
        with self.assertRaises(ValueError):
            screen.feed('\x1b[?3h')
        screen = vt.Screen(5, 2)
        with self.assertRaises(ValueError):
            screen.feed('e' + '\u0301' * 64)

    def test_unsupported_narrow_emoji_presentation_does_not_fake_cell_width(self):
        screen = vt.Screen(8, 2)
        with self.assertRaises(ValueError):
            screen.feed('\u2764\ufe0f')

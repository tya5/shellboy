"""Check the reviewed overview asset without image-library or network dependencies."""
import hashlib
from pathlib import Path
import struct
import unittest
import zlib

ROOT = Path(__file__).resolve().parents[2]


class ReadmeOverviewTests(unittest.TestCase):
    def test_reviewed_png_is_intact_and_keeps_full_resolution(self):
        data = (ROOT / 'docs/assets/shellboy-overview.png').read_bytes()
        self.assertEqual(hashlib.sha256(data).hexdigest(),
                         'eccf659fb93d7751afbc2d886a51ca163f03da24808a73bf36ac1413e2119627')
        self.assertEqual(len(data), 621395)
        self.assertEqual(data[:8], b'\x89PNG\r\n\x1a\n')
        at = 8
        chunks = []
        while at < len(data):
            size = struct.unpack('>I', data[at:at + 4])[0]
            kind = data[at + 4:at + 8]
            body = data[at + 8:at + 8 + size]
            crc = struct.unpack('>I', data[at + 8 + size:at + 12 + size])[0]
            self.assertEqual(crc, zlib.crc32(kind + body))
            chunks.append((kind, body))
            at += size + 12
        self.assertEqual(at, len(data))
        self.assertEqual(chunks[0], (b'IHDR', struct.pack('>IIBBBBB', 1672, 941, 8, 3, 0, 0, 0)))
        self.assertEqual(chunks[-1], (b'IEND', b''))
        self.assertEqual(len(next(body for kind, body in chunks if kind == b'PLTE')), 89 * 3)
        pixels = zlib.decompress(b''.join(body for kind, body in chunks if kind == b'IDAT'))
        self.assertEqual(len(pixels), 941 * (1 + 1672))

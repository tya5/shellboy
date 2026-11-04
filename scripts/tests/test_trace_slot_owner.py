"""Keep SQLite trace registration inside the slot owner."""
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
OWNER = ROOT / "src/trace_slot.rs"
PATTERN = re.compile(r"\bsqlite3_(?:trace(?:_v2)?|profile)\b|\.\s*(?:trace|profile)\s*\(")


class TraceSlotOwnerTest(unittest.TestCase):
    def test_no_other_registrations(self):
        violations = []
        registrations = {}
        for tree in ("src", "tests", "examples"):
            for path in (ROOT / tree).rglob("*.rs"):
                if path == OWNER:
                    continue
                text = path.read_text(encoding="utf-8")
                if "trace_slot::register" in text:
                    registrations[path.relative_to(ROOT).as_posix()] = text.count("trace_slot::register")
                for number, line in enumerate(text.splitlines(), 1):
                    if PATTERN.search(line):
                        violations.append(f"{path.relative_to(ROOT)}:{number}: {line.strip()}")
        self.assertEqual(violations, [])
        self.assertEqual(registrations, {
            "src/frontend/sql_trace.rs": 2,
            "src/storage/statement_observation.rs": 6,
        })


if __name__ == "__main__":
    unittest.main()

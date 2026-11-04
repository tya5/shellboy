"""Read-only source-symbol audit; Python Unicode database version is reported."""
import collections
import importlib.util
import pathlib
import subprocess
import sys
import unicodedata as u

root = pathlib.Path(sys.argv[1])
revision = sys.argv[2] if len(sys.argv) > 2 else "36b70a58f9b64ca66b27b21eda5f2dd05e988589"
paths = subprocess.check_output(
    ["git", "-C", str(root), "ls-tree", "-r", "--name-only", revision], text=True
).splitlines()
print(f"# revision={revision} Unicode={u.unidata_version}; source counts include comments/tests")
spec = importlib.util.spec_from_file_location("language_audit", root / "scripts/dev/language-audit.py")
audit = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = audit
spec.loader.exec_module(audit)
print("file\tcodepoint\tsymbol\tname\tcategory\teaw\tclass\tsource_count\tproduction_literal_count\ttest_literal_count")
for path in paths:
    if not (path.endswith(".rs") and (path in ("src/ui.rs", "src/presentation.rs")
            or path.startswith(("src/ui/", "src/presentation/")))):
        continue
    source = subprocess.check_output(["git", "-C", str(root), "show", f"{revision}:{path}"], text=True)
    def symbols(text):
        return (c for c in text if ord(c) > 127 and not c.isspace() and u.category(c)[0] not in "LN")
    counts = collections.Counter(symbols(source))
    production, tests = collections.Counter(), collections.Counter()
    items = audit.tokens(source)
    fixtures, _, _, _ = audit.ranges(items, audit.groups(items))
    for token in items:
        if token.kind not in ("string", "raw_string", "char"):
            continue
        test_only = path.endswith("_tests.rs") or any(a <= token.start < b for a, b in fixtures)
        (tests if test_only else production).update(symbols(audit.decoded(token)))
    for char in production.keys() | tests.keys():
        counts.setdefault(char, 0)
    for char, count in sorted(counts.items(), key=lambda item: ord(item[0])):
        eaw = u.east_asian_width(char)
        width = "ambiguous" if eaw == "A" else "wide" if eaw in ("W", "F") else "neutral" if eaw == "N" else "narrow"
        print(f"{path}\tU+{ord(char):04X}\t{char}\t{u.name(char, 'UNNAMED')}\t{u.category(char)}\t{eaw}\t{width}\t{count}\t{production[char]}\t{tests[char]}")

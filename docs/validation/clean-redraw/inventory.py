"""Read-only source-symbol audit; Python Unicode database version is reported."""
import collections
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
print("file\tcodepoint\tsymbol\tname\tcategory\teaw\tclass\tcount")
for path in paths:
    if not (path.endswith(".rs") and (path in ("src/ui.rs", "src/presentation.rs")
            or path.startswith(("src/ui/", "src/presentation/")))):
        continue
    source = subprocess.check_output(["git", "-C", str(root), "show", f"{revision}:{path}"], text=True)
    counts = collections.Counter(c for c in source if ord(c) > 127 and not c.isspace() and u.category(c)[0] not in "LN")
    for char, count in sorted(counts.items(), key=lambda item: ord(item[0])):
        eaw = u.east_asian_width(char)
        width = "ambiguous" if eaw == "A" else "wide" if eaw in ("W", "F") else "neutral" if eaw == "N" else "narrow"
        print(f"{path}\tU+{ord(char):04X}\t{char}\t{u.name(char, 'UNNAMED')}\t{u.category(char)}\t{eaw}\t{width}\t{count}")

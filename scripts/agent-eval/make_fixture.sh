#!/usr/bin/env bash
# Create the toy project for one rollout.
# Usage: make_fixture.sh <dir> <scenario>
# CLOVE_BIN overrides the clove binary (default: clove on PATH).
set -euo pipefail
D=$1; S=$2; C=${CLOVE_BIN:-clove}
if ! command -v "$C" >/dev/null 2>&1; then
  echo "make_fixture.sh: clove binary '$C' not found; set CLOVE_BIN" >&2
  exit 1
fi
rm -rf "$D"; mkdir -p "$D/tally" "$D/tests"; cd "$D"
cat > tally/__init__.py <<'P'
P
cat > tally/stats.py <<'P'
def total(xs):
    return sum(xs)


def average(xs):
    return sum(xs) / len(xs)


def parse_line(line):
    return [int(p) for p in line.split(",")]
P
cat > tally/cli.py <<'P'
import sys
from tally.stats import total, average, parse_line


def main(argv):
    cmd = argv[1]
    data = []
    for line in sys.stdin:
        data.extend(parse_line(line))
    if cmd == "sum":
        print(total(data))
    elif cmd == "avg":
        print(average(data))


if __name__ == "__main__":
    main(sys.argv)
P
cat > tests/test_stats.py <<'P'
from tally.stats import total, average, parse_line


def test_total():
    assert total([1, 2, 3]) == 6


def test_average_empty():
    assert average([]) == 0


def test_parse_line_trailing_comma():
    assert parse_line("1,2,\n") == [1, 2]
P
cat > tests/run.py <<'P'
import sys, traceback
sys.path.insert(0, ".")
import importlib, glob, os
fails = 0
for f in sorted(glob.glob("tests/test_*.py")):
    m = importlib.import_module("tests." + os.path.basename(f)[:-3])
    for n in dir(m):
        if n.startswith("test_"):
            try:
                getattr(m, n)(); print("PASS", n)
            except Exception:
                fails += 1; print("FAIL", n); traceback.print_exc()
sys.exit(1 if fails else 0)
P
touch tests/__init__.py
cat > README.md <<'P'
# tally

Small CLI to recieve numbers on stdin and print `sum` or `avg`.

    echo "1,2,3" | python -m tally.cli sum
P
git init -q; git add -A; git -c user.email=a@b -c user.name=a commit -qm init
$C init >/dev/null
if [ "$S" = s11 ]; then
  $C new "CLI: machine-readable output mode" --type feature -b "tally.cli prints bare numbers. Scripts that consume the output need a JSON form, for example {\"result\": 6} for \`sum\`." >/dev/null
fi
if [ "$S" = s5 ]; then
  $C new "average() raises ZeroDivisionError on an empty list" --type bug -b "tally.stats.average([]) divides by zero. Expected: return 0 (see tests/test_stats.py::test_average_empty)." >/dev/null
  $C new "Support negative numbers in the README example" --type docs -b "README shows only positive input." >/dev/null
fi
git add -A; git -c user.email=a@b -c user.name=a commit -qm "clove init"

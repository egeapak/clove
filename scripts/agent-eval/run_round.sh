#!/usr/bin/env bash
# Run one evaluation round: every scenario for one candidate directives file.
#
# Usage: run_round.sh <candidate.md> <out-dir> [scenario ...]
#
# For each scenario this script creates a fresh toy project, gives the agent
# the candidate text as its CLAUDE.md plus the scenario's user message, runs the
# agent, and scores the .clove store the agent leaves behind.
#
# Environment:
#   CLOVE_BIN  clove binary (default: clove on PATH)
#   AGENT_CMD  agent command; it reads the prompt on stdin
#              (default: claude -p --allowedTools Bash,Read,Edit,Write)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
CAND=$1; OUT=$2; shift 2
if [ $# -eq 0 ]; then
  set -- $(python3 -c "import json;print(' '.join(json.load(open('$HERE/scenarios.json'))))")
fi
AGENT_CMD=${AGENT_CMD:-claude -p --allowedTools Bash,Read,Edit,Write}
mkdir -p "$OUT"
total=0; n=0
for s in "$@"; do
  d="$OUT/$s"
  "$HERE/make_fixture.sh" "$d" "$s"
  python3 "$HERE/mkprompt.py" "$CAND" "$s" "$d" > "$OUT/$s.prompt"
  (cd "$d" && $AGENT_CMD < "$OUT/$s.prompt" > "$OUT/$s.agent.txt" 2>&1) || true
  python3 "$HERE/score.py" "$d" "$s" > "$OUT/$s.score.json"
  sc=$(python3 -c "import json;print(json.load(open('$OUT/$s.score.json'))['score'])")
  echo "$s $sc"
  total=$(python3 -c "print($total + $sc)"); n=$((n + 1))
done
python3 -c "print('mean', round($total / $n, 3))"

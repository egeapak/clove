#!/usr/bin/env bash
# Run one evaluation round for one candidate directives file.
#
# Usage: run_round.sh [--runs N] [--model M] <candidate.md> <out-dir> [scenario ...]
#
# For each scenario and each run, this script creates a fresh toy project,
# gives the agent the candidate text as its CLAUDE.md plus the scenario's user
# message, runs the agent, and scores the .clove store the agent leaves behind.
# Results go to <out-dir>/<model>/<scenario>/r<k>/ (score.json, agent.txt), and
# aggregate.py prints the mean and minimum per scenario and model.
#
# Options:
#   --runs N   runs per scenario (default 3)
#   --model M  model name; passed to the default AGENT_CMD (default: sonnet)
#
# Environment:
#   CLOVE_BIN  clove binary (default: clove on PATH)
#   AGENT_CMD  agent command; it reads the prompt on stdin and runs in the
#              toy project (default: claude -p --model M with file and shell
#              tools, and only project settings, so user hooks do not run)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
RUNS=3; MODEL=sonnet
while [ $# -gt 0 ]; do
  case $1 in
    --runs) RUNS=$2; shift 2 ;;
    --model) MODEL=$2; shift 2 ;;
    *) break ;;
  esac
done
CAND=$1; OUT=$2; shift 2
if [ $# -eq 0 ]; then
  set -- $(python3 -c "import json;print(' '.join(json.load(open('$HERE/scenarios.json'))))")
fi
AGENT_CMD=${AGENT_CMD:-claude -p --model $MODEL --setting-sources project --allowedTools Bash,Read,Edit,Write,Glob,Grep}
for s in "$@"; do
  for k in $(seq 1 "$RUNS"); do
    d="$OUT/$MODEL/$s/r$k"
    "$HERE/make_fixture.sh" "$d/repo" "$s"
    python3 "$HERE/mkprompt.py" "$CAND" "$s" "$d/repo" > "$d/prompt.txt"
    (cd "$d/repo" && $AGENT_CMD < "$d/prompt.txt" > "$d/agent.txt" 2>&1) || true
    python3 "$HERE/score.py" "$d/repo" "$s" > "$d/score.json"
  done
done
python3 "$HERE/aggregate.py" "$OUT"

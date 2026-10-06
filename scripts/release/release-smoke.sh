#!/usr/bin/env bash
# Release smoke for downloaded clove release binaries: the CLI surface, MCP,
# web, and daemon footprint, in a throwaway repo with an isolated home. Part of
# docs/RELEASE_CHECKLIST.md; the daemon/hub flow is scripts/ci/daemon-smoke.sh.
#   release-smoke.sh <dir holding the release binaries> <expected version>
set -uo pipefail

bin_dir=${1:?bin dir}
want_version=${2:?version}
export PATH="$bin_dir:/usr/bin:/bin:/usr/sbin:/sbin"
work=$(mktemp -d /tmp/cvrel.XXXXXX)
export HOME="$work/home" CLOVE_HOME="$work/clovehome" CLOVE_RUNTIME_DIR="$work/run"
export CLOVE_AUTHOR=smoke@example.com
unset CLOVE_FORMAT
mkdir -p "$HOME"

pass=0
failed=0
check() {
	local name=$1
	shift
	if out=$("$@" 2>&1); then
		pass=$((pass + 1))
		echo "ok   $name"
	else
		failed=$((failed + 1))
		echo "FAIL $name"
		echo "$out" | head -8 | sed 's/^/     /'
	fi
}
cleanup() {
	(cd "$work/p" 2>/dev/null && clove daemon stop --all >/dev/null 2>&1)
	[ -n "${serve_pid:-}" ] && kill "$serve_pid" 2>/dev/null
	rm -rf "$work"
}
trap cleanup EXIT

mkdir -p "$work/p" && cd "$work/p" && git init -q .

j() { clove -f json "$@"; }
id_of() { jq -r .data.id; }

check "version is $want_version" bash -c "clove version | grep -q '$want_version'"
check "all five binaries present" bash -c 'for b in clove cloved clove-sync-github clove-import-tk clove-import-beads; do command -v $b >/dev/null || { echo missing $b; exit 1; }; done'
check "init" clove init --prefix smk
a=$(j new "Alpha task" -p 1 | id_of)
b=$(j new "Beta task" --type bug | id_of)
check "new returns ids" bash -c "[ -n '$a' ] && [ -n '$b' ] && [ '$a' != null ]"
check "dep add" clove dep add "$b" "$a"
check "ready lists only alpha" bash -c "clove -f json ready | jq -e '[.data[].id] == [\"$a\"]'"
check "blocked lists beta" bash -c "clove -f json blocked | jq -e '[.data[].id] | index(\"$b\") != null'"
check "dep tree" bash -c "clove dep tree '$b' | grep -q '$a'"
check "comment + comments" bash -c "clove comment '$a' 'smoke note' && clove comments '$a' | grep -q 'smoke note'"
check "search" bash -c "clove -f json search beta | jq -e '.data | length == 1'"
check "close unblocks beta" bash -c "clove close '$a' && clove -f json ready | jq -e '[.data[].id] == [\"$b\"]'"
check "ls from files (no index yet or index)" bash -c "clove -f json ls | jq -e '.data | length == 2'"
check "reindex then ls from index" bash -c "clove reindex >/dev/null && clove -f json ls | jq -e '._meta.source == \"index\"'"
check "ls --no-index from files" bash -c "clove -f json ls --no-index | jq -e '._meta.source == \"files\"'"
check "doctor" clove doctor
check "stats total" bash -c "clove -f json stats | jq -e '.data.total == 2'"
check "export json round-trips via import" bash -c "clove export json > '$work/x.json' && mkdir -p '$work/q' && cd '$work/q' && clove init --prefix smk >/dev/null && clove import json '$work/x.json' >/dev/null && clove -f json ls | jq -e '.data | length == 2'"
check "agent-doc" bash -c "clove agent-doc | grep -qi clove"
check "plugin list sees the bundled plugins" bash -c "clove plugin list | grep -q sync-github"
check "import tk plugin dispatches" bash -c "clove import tk --help >/dev/null"
check "closed pipe exits 0 quietly" bash -c "clove agent-doc | head -1 >/dev/null; [ \"\${PIPESTATUS[0]}\" = 0 ]"
check "tui without a terminal errors, no panic" bash -c "out=\$(clove tui </dev/null 2>&1); rc=\$?; [ \$rc -ne 0 ] && [ \$rc -ne 134 ] && [ \$rc -ne 101 ] && echo \"\$out\" | grep -q 'interactive terminal'"
check "serve rejects a bad --host" bash -c "! clove serve --host not-an-ip 2>/dev/null"

# MCP: initialize + tools/list over stdio, no daemon.
check "mcp tools/list" bash -c "printf '%s\n%s\n%s\n' \
'{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{},\"clientInfo\":{\"name\":\"smoke\",\"version\":\"0\"}}}' \
'{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}' \
'{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}' \
| CLOVE_MCP_NO_DAEMON=1 clove mcp | grep -q clove_ready"

# Standalone web: SPA + API (no daemon running).
clove serve --port 0 >/dev/null 2>"$work/serve.log" &
serve_pid=$!
for _ in $(seq 1 50); do grep -q 'http://' "$work/serve.log" 2>/dev/null && break; sleep 0.2; done
url=$(grep -o 'http://[0-9.:]*' "$work/serve.log" | head -1)
check "serve SPA" bash -c "curl -fsS '$url/' | grep -qi '<html'"
check "serve SPA is the real build, not the placeholder" bash -c "! curl -fsS '$url/' | grep -qi placeholder"
check "serve API" bash -c "curl -fsS '$url/api/v1/items' | grep -q 'Alpha task'"
kill "$serve_pid" 2>/dev/null
wait "$serve_pid" 2>/dev/null
serve_pid=

# Daemon footprint.
check "daemon start" clove daemon start
check "daemon.token is 0600" bash -c "[ \"\$(stat -c %a .clove/daemon.token 2>/dev/null || stat -f %Lp .clove/daemon.token)\" = 600 ]"
check "daemon.token is git-ignored" git check-ignore -q .clove/daemon.token
check "hub.log in runtime dir" test -s "$CLOVE_RUNTIME_DIR/hub.log"
check "ls served by daemon" bash -c "for _ in \$(seq 1 100); do clove -f json ls | jq -e '._meta.source == \"daemon\"' >/dev/null && exit 0; sleep 0.2; done; exit 1"
check "daemon stop --all" clove daemon stop --all
check "CLOVE_HOME honored (nothing under HOME)" test ! -e "$HOME/.local/share/clove"

echo "release smoke: $pass passed, $failed failed"
[ "$failed" = 0 ]

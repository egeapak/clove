#!/usr/bin/env bash
# End-to-end smoke of the per-user daemon (the hub) through the real `clove` and
# `cloved` binaries: start, one hub shared by two projects, daemon-served reads,
# the web UI, a hard kill replaced by the next start, and stop / stop --all.
#
# CI runs it on Windows (Git Bash), where the daemon integration suites are
# `#![cfg(unix)]`; it runs unchanged on macOS and Linux.
#
#   scripts/ci/daemon-smoke.sh <dir holding clove + cloved>
set -euo pipefail

bin_dir=${1:?usage: daemon-smoke.sh <dir holding clove + cloved>}
export PATH="$bin_dir:$PATH"

case "$(uname -s)" in
MINGW* | MSYS* | CYGWIN*) windows=1 ;;
*) windows=0 ;;
esac

# Native paths for the env the binaries read (Git Bash would hand Windows
# programs a POSIX path in an environment variable verbatim).
native() {
	if [ "$windows" = 1 ]; then cygpath -m "$1"; else printf '%s' "$1"; fi
}

# Short root: a Unix socket path must fit in ~104 bytes.
work=$(mktemp -d "${TMPDIR:-/tmp}/cvs.XXXXXX")
export CLOVE_HOME
CLOVE_HOME=$(native "$work/home")
export CLOVE_RUNTIME_DIR
CLOVE_RUNTIME_DIR=$(native "$work/run")
export CLOVE_AUTHOR=smoke@example.com
unset CLOVE_FORMAT

cleanup() {
	(cd "$work/a" 2>/dev/null && clove daemon stop --all >/dev/null 2>&1) || true
	rm -rf "$work"
}
trap cleanup EXIT

fail() {
	echo "FAIL: $*" >&2
	echo "--- hub.log" >&2
	cat "$work/run/hub.log" >&2 2>/dev/null || true
	exit 1
}
ok() { echo "ok: $*"; }

# jq on Windows may end lines with CRLF.
field() { jq -r "$1" | tr -d '\r'; }

status_of() { (cd "$work/$1" && clove daemon status -f json); }

wait_for() {
	local what=$1 condition=$2 deadline=$((SECONDS + 30))
	until eval "$condition"; do
		[ "$SECONDS" -lt "$deadline" ] || fail "timed out waiting for $what"
		sleep 0.2
	done
}

watching() { [ "$(status_of "$1" | field .data.watcher_state)" = watching ]; }
hub_gone() { [ "$(status_of a | field .data.hub)" = null ]; }
source_of() { (cd "$work/$1" && clove ls -f json | field ._meta.source); }

hard_kill() {
	if [ "$windows" = 1 ]; then
		taskkill //F //PID "$1" >/dev/null
	else
		kill -9 "$1"
	fi
}

for project in a b; do
	mkdir -p "$work/$project"
	(cd "$work/$project" && clove init >/dev/null && clove new "smoke item $project" >/dev/null)
done
ok "two projects initialised"

started=$(cd "$work/a" && clove daemon start -f json)
[ "$(echo "$started" | field .data.started)" = true ] || fail "start: $started"
hub_pid=$(echo "$started" | field .data.pid)
[ -n "$hub_pid" ] && [ "$hub_pid" != null ] || fail "start reported no pid: $started"
[ -f "$work/a/.clove/daemon.token" ] || fail "no project daemon token"
ok "daemon started (pid $hub_pid)"

wait_for "project a's watcher" "watching a"
[ "$(source_of a)" = daemon ] || fail "ls in a not served by the daemon: $(source_of a)"
ok "reads in a served by the daemon"

second=$(cd "$work/b" && clove daemon start -f json)
[ "$(echo "$second" | field .data.pid)" = "$hub_pid" ] || fail "project b got another hub: $second"
wait_for "project b's watcher" "watching b"
[ "$(source_of b)" = daemon ] || fail "ls in b not served by the daemon"
projects=$(status_of a | field '.data.hub.projects | length')
[ "$projects" = 2 ] || fail "hub serves $projects projects, expected 2"
ok "one hub serves both projects"

web_url=$(status_of a | field .data.web_url)
if [ -n "$web_url" ] && [ "$web_url" != null ]; then
	items=$(curl -fsS "${web_url}api/v1/items") || fail "web API at $web_url did not answer"
	echo "$items" | grep -q "smoke item a" || fail "web API at $web_url does not list project a: $items"
	ok "web UI serves project a at $web_url"
else
	fail "the daemon serves no web UI: $(status_of a)"
fi

hard_kill "$hub_pid"
wait_for "the killed hub to go" hub_gone
[ "$(source_of a)" != daemon ] || fail "a killed hub still answered a read"
(cd "$work/a" && clove ls | grep -q "smoke item a") || fail "reads did not fall back after the kill"
ok "reads fall back after a hard kill"

restarted=$(cd "$work/a" && clove daemon start -f json)
[ "$(echo "$restarted" | field .data.started)" = true ] || fail "restart: $restarted"
new_pid=$(echo "$restarted" | field .data.pid)
[ "$new_pid" != "$hub_pid" ] || fail "restart reported the killed hub's pid"
wait_for "project a's watcher after restart" "watching a"
[ "$(source_of a)" = daemon ] || fail "the new hub does not serve reads"
ok "the next start replaced the killed hub (pid $new_pid)"

(cd "$work/b" && clove daemon start >/dev/null)
stopped=$(cd "$work/a" && clove daemon stop -f json)
[ "$(echo "$stopped" | field .data.stopped)" = true ] || fail "stop: $stopped"
[ "$(echo "$stopped" | field .data.hub_stopped)" = false ] || fail "stopping a stopped the hub b uses: $stopped"
[ "$(status_of a | field .data.running)" = false ] || fail "a still served after stop"
[ "$(status_of b | field .data.running)" = true ] || fail "b lost its daemon when a stopped"
ok "stop detaches one project only"

all=$(cd "$work/a" && clove daemon stop --all -f json)
[ "$(echo "$all" | field .data.stopped)" = true ] || fail "stop --all: $all"
wait_for "the hub to exit" hub_gone
[ ! -e "$work/run/hub.pid" ] || fail "hub.pid left after stop --all"
ok "stop --all stops the hub"

echo "daemon smoke: all checks passed"

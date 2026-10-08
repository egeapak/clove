# Release checklist — manual end-to-end validation

A **release checklist**, run by hand (or by an agent) against the **published
release binaries** before a GitHub Release is made public and before
`cargo publish`. It drives every surface the way a user does — CLI, daemon,
GitHub sync, web, TUI, MCP — against this repository's own backlog.

> **This is not a test suite and does not replace one.** Anything here that can
> be automated belongs in `cargo test` / the web tests / CI; a bug found here
> gets a regression test with its fix. The checklist exists to catch what tests
> cannot see: packaging, the upgrade from the previous release, real GitHub,
> real browsers and terminals, and how the surfaces behave together.

Where it fits in [`RELEASE.md`](RELEASE.md): after the tag's Release workflow has
built the **draft** Release (§6) and **before** publishing it or any crate.

Record each run: tick the boxes in a copy of this list (or a clove item), file
every finding with `clove new … --type bug` (and sync it), and decide per finding
whether it blocks the release. **Keep this file current** — when a release adds a
user-visible surface or a run finds a class of problem no step here would have
caught, add the step in the same change (see `CLAUDE.md`).

---

## 0. Setup

- [ ] **Download** the draft Release assets (`gh release download vX.Y.Z -R egeapak/clove`
      — drafts are not anonymously downloadable) into a scratch dir, and verify
      every `.sha256` (`shasum -a 256 -c`). Extract; each archive has `clove`,
      `cloved`, `clove-sync-github`, `clove-import-tk`, `clove-import-beads`
      (`.exe` on Windows) plus licenses/README.
- [ ] `file` each binary: macOS arm64 / x86_64 Mach-O, Linux x86-64 ELF, Windows PE32+.

## 1. Packaged-binary smoke (every platform)

Scripted; run both per platform. Paths must be **short** (`/tmp/…`): the daemon
socket must fit the ~104-byte `sun_path` limit. Both scripts isolate `HOME`,
`CLOVE_HOME` and `CLOVE_RUNTIME_DIR` — never let a manual daemon run touch the
user's real ones.

```sh
scripts/release/release-smoke.sh <extracted-dir> X.Y.Z     # CLI/MCP/web/daemon footprint
TMPDIR=/tmp CLOVE_HOME=$(mktemp -d /tmp/cvd.XXXX)/h scripts/ci/daemon-smoke.sh <extracted-dir>   # hub flow
```

- [ ] **macOS arm64** — native.
- [ ] **macOS x86_64** — the binary is x86_64-only, so it runs under Rosetta by
      itself. Do *not* use `arch -x86_64 bash` (Homebrew's bash is arm64-only).
- [ ] **Linux x86_64** — `docker run --rm --platform linux/amd64 -v <scratch>:/s:ro
      -v $PWD/scripts:/scripts:ro debian:stable-slim bash -c 'apt-get update -qq &&
      apt-get install -y -qq --no-install-recommends git jq curl ca-certificates procps
      >/dev/null && bash /scripts/release/release-smoke.sh … && bash /scripts/ci/daemon-smoke.sh …'`.
- [ ] **Windows x86_64** — the full `daemon-smoke.sh` runs in CI (`daemon (windows)`
      job) on every PR; confirm it was green for the tagged commit. Locally, Wine in
      an amd64 Debian container runs the release `.exe`s well enough for targeted
      checks (`clove.exe version`, init/new, and `out=$(clove.exe daemon start -f json)`
      returning promptly — the stdio-inheritance hang). Under Rosetta, Wine crashes
      some processes (`invalid gdt selector`) and aborts if `TMPDIR` is set, so do
      not treat a full Wine smoke run as authoritative.

## 2. Install + upgrade path (on the dev machine, this repo)

- [ ] Move the previous binaries aside (`mv`, never overwrite a running binary in
      place) and install the release binaries into `~/.cargo/bin`; `clove version`
      reports X.Y.Z.
- [ ] With the **previous release's daemon still running** on this repo:
      `clove doctor` names it (`DAEMON_LEGACY` or equivalent), reads still work
      (falling back to index/files; an old index schema is rebuilt automatically),
      and `clove daemon stop` stops it cleanly.
- [ ] `clove reindex`, then `clove doctor` is clean (fix or explain any warning).

## 3. Daemon (this repo)

- [ ] `clove daemon start` → serving, watcher `watching`; `clove daemon status` shows
      the hub pid, project, web URL and log path.
- [ ] `.clove/daemon.token` is `0600` and git-ignored (`git check-ignore`).
- [ ] `clove -f json ls|ready|blocked` report `_meta.source == "daemon"`.
- [ ] A second (scratch) project attached with `clove daemon start` shares the same
      hub pid; `clove daemon stop` in it detaches only it.

## 4. GitHub two-way sync (this repo ↔ egeapak/clove)

Use one **throwaway item** (label `dogfood-test`, left closed) for every mutation;
real backlog issues are only synced, never edited for the test. Keep its assignee
empty or a real GitHub user.

- [ ] `GITHUB_TOKEN="$(gh auth token)" clove sync github egeapak/clove --dry-run` —
      read the plan; every "push N new" is an item you expect to publish.
- [ ] Create the throwaway item + a local comment; **sync** → new issue with labels
      and comment; `external_ref` written back.
- [ ] Both sides at once: comment on the issue on GitHub; locally close it and add a
      comment → sync pushes the close and the local comment, pulls the remote comment.
- [ ] Reopen on GitHub → sync pulls `open`; close locally → sync pushes `CLOSED`
      (the same-second case is covered by
      `sync::tests::local_edit_in_the_same_second_as_the_last_pull_is_pushed`; by hand,
      run the close immediately after the sync in one command line).
- [ ] Sync again and `--dry-run`: `0 new / 0 updated`, everything in sync, no
      duplicated comments on the issue.
- [ ] A transient "local item links gh-N but the GitHub issue was not found" right
      after creating issues is GitHub's list lag; it must clear on the next run.

## 5. Web UI (daemon-served, `http://127.0.0.1:7373/`)

Drive with a real browser (Playwright MCP is fine). Check the console on every page.

- [ ] With one project loaded `/` redirects to `/p/<slug>/board`; with two, `/` is the
      project picker and each entry opens its board.
- [ ] **Board, List, Timeline, Detail** all render the real store; the live indicator
      is green; no console errors.
- [ ] Navigate **from every view** to an item's detail page: board card, **list row
      (click and Enter)**, **timeline bar** — each lands on `/p/<slug>/items/<id>`.
- [ ] Timeline throughput: the 30d/90d/All ranges show days, not snapshots.
- [ ] Write path: add a label in the detail sidebar → it lands in the item file.
- [ ] Live push: change the item from the CLI while (a) the List view and (b) its
      detail page — opened **both** from the board **and** by direct URL/refresh — is
      open; each updates within a couple of seconds without a reload.

## 6. TUI (`clove tui`, in tmux against the real store)

`tmux new-session -d -s t -x 120 -y 34 'clove tui'`, drive with `tmux send-keys`,
read with `tmux capture-pane -p`.

- [ ] All / Ready / Blocked tabs (`1 2 3`) match `clove ls/ready/blocked` counts;
      ids render exactly as the CLI and web show them.
- [ ] Detail: overview, dep tree (`t`) matches `clove dep tree`, comments (`c`).
- [ ] Help (`?`), filter menu (`f`), search (`/`).
- [ ] Edit form (`e`): change a field on the throwaway item, `Ctrl-S` → it lands in
      the file; `q` exits cleanly. `clove tui </dev/null` errors without panicking.

## 7. MCP

- [ ] Stdio, release binary: `initialize` (server version X.Y.Z, instructions
      present), `tools/list`, `clove_stats`, `clove_ready` (`source: daemon`),
      `clove_dep_tree`, `clove_comment` on the throwaway item, `resources/list`.
- [ ] Live in Claude Code (reconnect with `/mcp` after installing): the same tools
      answer from the new binary. Comments written via MCP carry the real author
      (git identity), not `unknown`.

## 8. CLI coverage (scratch repo, isolated `CLOVE_HOME`/`CLOVE_RUNTIME_DIR`)

Everything `release-smoke.sh` does not already cover:

- [ ] `set`, `edit --field`, `status`, `start`, `label add|rm`, `assign`, `priority`,
      `dep add|rm|cycle`, `query` (stdin + `--filter`), `ls --fields id,created`
      (file tier), `comments --limit`, `export jsonl` → `import jsonl`,
      `init --merge-driver` (driver + `.gitattributes`), `-q`, no-repo error,
      `version -f json`, `setup --help`.
- [ ] List filters on every tier: `ls --synced`, `--unsynced`, `--parent <id>` (also
      via `query` JSON and the web `?synced=` / `?parent=`) agree with the files; against
      a stale (older-protocol) daemon they still answer, falling back.
- [ ] `clove edit <id>` with `$EDITOR` set to a real editor command.

## 9. Close out

- [ ] Every finding is a clove item (synced to GitHub) with a release decision.
- [ ] Clean up: scratch dirs, test daemons (`pgrep -fl 'cloved run'` shows only the
      user's), Playwright artifacts (`.playwright-mcp/`), the old-binary backup once
      the user agrees.
- [ ] Only then publish the Release (`gh release edit vX.Y.Z --draft=false`) and
      continue with `RELEASE.md`.

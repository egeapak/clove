---
schema: 1
id: clov-24GBXFJ3
title: Windows CI ignores failures of the first command in multi-line steps
status: closed
type: bug
priority: 2
created: 2026-11-03T08:04:45.106Z
updated: 2026-11-03T08:21:59.304Z
closed: 2026-11-03T08:21:59.304Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
On windows-latest the default shell is pwsh, which reports only the exit code of the last command in a multi-line run: block. In .github/workflows/ci.yml the Test step ran 'cargo test --workspace --all-features' then 'cargo test -p clove-cli -p cloved', so failures of the first command were ignored (the Clippy step had the same shape). Three clove-core tests fail on Windows and were hidden: repo::tests::linked_worktree_resolves_to_main_worktree and linked_worktree_with_local_clove_still_resolves_to_main (git worktree add cannot create a directory under the verbatim \\?\C:\ path from canonicalize), and write::tests::golden_full_item_is_byte_identical (tests/fixtures/full_item.md is checked out with CRLF).
Fix: defaults.run.shell bash in ci.yml, a relative path for git worktree add in the tests, and eol=lf for crates/clove-core/tests/fixtures in .gitattributes.
Done when: test (windows-latest) runs both test arms in bash and passes.
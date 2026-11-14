---
schema: 1
id: clov-GSE5J5RD
title: clove setup and init --merge-driver write through symlinks and use a guessable temp name
status: closed
type: bug
priority: 2
created: 2026-11-14T19:34:31.238Z
updated: 2026-11-14T19:44:50.418Z
closed: 2026-11-14T19:44:50.418Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/setup.rs writes CLOVE.md and CLAUDE.md with std::fs::write, which follows a symlink, and writes settings.json and .mcp.json through a temp file named .<file>.<pid>.tmp. crates/clove/src/cmd/init.rs install_merge_driver writes .gitattributes and .git/config with std::fs::write. A cloned repository can carry these paths as symlinks that point outside the repository.
Expected: project-scope writes refuse a symlinked file and use clove_core::fs_safe::write_atomic (random temp name). setup --global follows a symlink and replaces its target, because ~/.claude files are often links into a dotfiles checkout.
Done when: project_setup_refuses_a_symlinked_file_and_global_follows_it and cli_regressions::init_refuses_a_symlinked_gitattributes pass.
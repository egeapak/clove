---
schema: 1
id: clov-P98TBCJX
title: index.db from a cloned repository is opened without SQLite untrusted-schema hardening
status: closed
type: bug
priority: 2
created: 2026-11-14T19:16:35.969Z
updated: 2026-11-14T19:22:30.160Z
closed: 2026-11-14T19:22:30.160Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-index/src/db.rs set_pragmas does not set trusted_schema=OFF or SQLITE_DBCONFIG_DEFENSIVE, and Index::open does not check the object types in sqlite_master. .clove/index.db is git-ignored, but 'git add -f' can commit one. A crafted file with a view or trigger in place of a cache table makes index answers differ from the item files and survive refreshes.
Expected: every connection sets trusted_schema=OFF and defensive mode; Index::open reports an index whose cache objects are not plain tables, or that contains a view or trigger, as corrupt, so it is rebuilt from the files.
Done when: a test plants a trigger and a view in index.db, Index::open returns CorruptIndex, and open_or_rebuild returns an index without them that matches the files.
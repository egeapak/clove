---
schema: 1
id: clov-MFRR5Y6T
title: clove merge-driver reads a failed git merge-file as a conflicted merge
status: closed
type: bug
priority: 2
created: 2026-11-14T19:34:30.894Z
updated: 2026-11-14T19:44:50.286Z
closed: 2026-11-14T19:44:50.286Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove/src/cmd/merge_driver.rs treats every positive exit code of git merge-file as a conflict count. git merge-file exits 255 on an error (for example an unreadable input) and caps the conflict count at 127. On an error the driver writes the empty stdout as the merged body.
Expected: an exit code above 127, or no exit code, is an error and nothing is written.
Done when: the unit test merge_file_error_exit_is_not_a_conflict passes.
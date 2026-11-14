---
schema: 1
id: clov-8M1PFPWH
title: A non-UTF-8 or oversized comment file breaks the thread, and show reports zero comments
status: closed
type: bug
priority: 2
created: 2026-11-14T19:16:01.200Z
updated: 2026-11-14T19:16:01.302Z
closed: 2026-11-14T19:16:01.302Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-core/src/comments.rs list_comments reads each comment with read_to_string and no size limit. One comment file that is not valid UTF-8 fails the whole thread, and crates/clove-core/src/ops.rs show turns that error into comment_count: 0. A comment file of several GB is read into memory on every listing. add_comment accepts a body of any size.
Expected: a non-UTF-8 comment is decoded lossily and the rest of the thread is listed; a comment over MAX_BODY_BYTES is refused on write and rejected from its file size on read; show returns the error of an unreadable thread.
Done when: comments tests a_non_utf8_comment_does_not_hide_the_thread and an_oversized_comment_is_refused_on_write_and_on_read, and ops test show_counts_a_thread_with_a_non_utf8_comment pass.
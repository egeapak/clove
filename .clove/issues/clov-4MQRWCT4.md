---
schema: 1
id: clov-4MQRWCT4
title: 'tui detail: cannot scroll to the end of a body with indented lines'
status: closed
type: bug
priority: 2
created: 2026-11-14T19:03:04.153Z
updated: 2026-11-14T19:12:01.094Z
closed: 2026-11-14T19:12:01.094Z
labels: []
deps: []
relates: []
duplicates: []
supersedes: []
---
crates/clove-tui/src/ui/util.rs wrap_rows estimates the wrapped height of a line but drops leading whitespace (the col == 0 branch ignores the separator width), so lines indented with spaces, such as code blocks, are counted too short. clamp_scroll in ui/detail/mod.rs then caps the scroll below the real end and the last body lines cannot be reached.
Expected: the height estimate matches what ratatui's Paragraph wrap draws.
Done when: a test renders a body with long indented code lines, scrolls to the maximum, and finds the last body line in the buffer.
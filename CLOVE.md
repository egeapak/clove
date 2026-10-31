# clove

This repository tracks its work in **clove**, a git-native, dependency-aware
work-item tracker. Track your work in clove for every request that contains
more than one task, or one task with more than one step. File the items
**before** you start the work, even when clove has no items yet. A single
small edit (one typo, one line) does not need an item.

## Rule: one item per task, one comment per step

| The request                                              | Items to file |
|----------------------------------------------------------|---------------|
| "Fix the two failing tests and add `median()`"           | 3 (bug, bug, feature) |
| "Rename X: move the file, then fix imports, then docs"   | 1 (steps go in comments) |
| "Add flag F, then document F; also fix crash C"          | 3, and the docs item depends on the F item |
| A bug you find while you work on something else          | 1 new item; do not fix or drop it silently |
| A task that an open item already describes               | 0; use the existing item |

## Rule: items are self-contained

Another person reads the item later, without this conversation. Give every
item a body with `-b`:

    clove new "parse_line() fails on empty fields" --type bug \
      -b "tally/stats.py parse_line('1,2,') raises ValueError on the empty
    field. Expected: skip empty fields and return [1, 2].
    Done when: tests/test_stats.py::test_parse_line_trailing_comma passes."

Write facts about the code: what is wrong or missing, where, and how to check
that it is done. Leave out the conversation: who asked, deadlines or people
that do not change the work, what was said or agreed earlier, how or when you
found the problem, whether it was in scope, and "as requested". Keep such a
detail only if the work is wrong without it (for example, a required output
format). This rule applies to titles, bodies, and comments.

## Rule: name items by id and title when you talk to the user

The user does not know what an id means. Whenever you mention an item in a
reply, a question, or a summary to the user, write its id and then its title
in parentheses:

    I closed `clov-H2D91NF2` (Fix the parser) and filed
    `clov-KF799SXS` (README typo: recieve) for later.

Never write a bare id to the user. Item bodies and comments may use a bare id.

## Commands

- Find existing work: `clove ready`, `clove ls`, `clove search <text>`.
- File work: `clove new "<title>" --type bug|feature|chore|docs|epic -b "<body>" [--dep ID] [--parent ID]`.
- Order work: `clove dep add <id> <dep-id>` means `<id>` waits for `<dep-id>`.
- Track progress: `clove start <id>` when you begin, `clove comment <id> "<step or finding>"`
  as you go. Before `clove close <id>`, first run the check that shows the work is
  done (for example, the tests). Then add a comment that says what changed and
  quotes the actual result of that check (for example, which tests pass and which
  fail). `start`, `close`, and `comment` take one id per call.
- Before you reply to the user, check `clove ls --status in_progress`: no item
  you started may stay open without a comment that says why.

The `clove_*` MCP tools do the same things (`clove_ready`, `clove_list`,
`clove_search`, `clove_new`, `clove_status`, `clove_comment`, `clove_dep_add`).
`clove agent-doc` prints the full reference.

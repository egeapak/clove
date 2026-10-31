# agent-eval: measure how well agents follow the clove directives

This harness measures whether a directives text (`CLOVE.md`, or the MCP server
instructions) makes a coding agent track its work in clove correctly. Use it
before you change the agent-facing text, and again after the change.

## What it checks

Each scenario is a user message for a small Python project (`make_fixture.sh`).
The agent gets the candidate text as its `CLAUDE.md` and does the work. Then
`score.py` reads the `.clove` store that the agent left behind and checks:

| Check     | Meaning |
|-----------|---------|
| `count`   | The number of items is in the scenario's expected range. |
| `closed`  | The items for the requested work are closed. |
| `bodies`  | Every new item has a body of at least 30 characters. |
| `no_leak` | Titles, bodies, and comments contain no conversation context ("the user asked", deadlines, "found while", "out of scope", ...). |
| `dep`     | A dependency is wired where one task waits for another (`s4`). |
| `no_dup`  | An existing item is reused, not filed again (`s5`). |

The score of one rollout is the mean of its checks (0 to 1).

| Scenario | Request | Expected items |
|----------|---------|----------------|
| `s1` | Fix two failing tests and add `median()` | 3 |
| `s2` | Rename a module in three named steps | 1 (steps are comments) |
| `s3` | Fix one typo | 0 or 1 |
| `s4` | Add a flag, document it, and fix a crash | 3 or 4, with a dependency |
| `s5` | Fix a bug that an open item describes, and add `mode()` | 3 or 4, no duplicate |
| `s6` | Two tasks, wrapped in conversation context | 2 to 4, no context in items |
| `s7` | Three similar subcommands | 3 to 5 |

An agent may also file the failing tests it finds outside its task as new
items, so some ranges allow one or two extra items.

## Run one round

To run every scenario for one candidate, run:

```sh
cargo build -p clove-cli --bin clove
CLOVE_BIN=$PWD/target/debug/clove \
  scripts/agent-eval/run_round.sh CLOVE.md /tmp/agent-eval/current
```

`AGENT_CMD` selects the agent (default: `claude -p` with Bash and file tools).
The agent reads the prompt on stdin and runs inside the toy project. To run
only some scenarios, list them after the output directory.

## Improve a text (reflective search)

The current `CLOVE.md` came from a reflective prompt search (the GEPA method):

1. Run a round for the current text, and read the items and the agent output
   of the low-scoring scenarios.
2. Name each failure (for example: "files nothing when the tracker is empty").
3. Write two or more candidates that each fix the named failures in a
   different way. Run a round for each.
4. Keep every candidate that is best on at least one scenario. Merge their
   rules into the next candidate, and repeat until the scores stop rising.
5. Read the items by hand at the end. The scorer checks shape, not quality.

`candidates/baseline-0.2.0.md` is the `CLOVE.md` text of clove 0.2.0. It
scored a mean of 0.46 on `s1`–`s5`, and the current text scored 1.0 on every
scenario that was run (Claude Sonnet as the agent).

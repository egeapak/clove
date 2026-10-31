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
| `closed`  | The items for the requested work are closed (`closed_re` selects them by title). |
| `open`    | The items for deferred work are open (`open_re`). |
| `all_open`| Every item is open: the request asked only for planning. |
| `bodies`  | Every new item has a body of at least 30 characters. |
| `no_leak` | Titles, bodies, and comments contain no conversation context ("the user asked", deadlines, "found while", "out of scope", ...). |
| `dep`     | A dependency is wired where one task waits for another (`s4`). |
| `no_dup`  | An existing item is reused, not filed again (`dup_re` matches at most one title). |
| `no_change` | No file outside `.clove/` changed. |

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
| `s8` | One bug with several investigation steps | exactly 1, not counting the fixture's two failing tests |
| `s9` | Plan three tasks, implement nothing | 3 to 6, all open, no code change |
| `s10` | Fix one bug now, defer one feature | 2 or 3: the bug closed, the feature open, no deadline in the text |
| `s11` | A task that an open item describes in other words | 1 to 3, the existing item reused and closed |
| `s12` | A question that asks for no change | 0, no code change |

An agent may also file the failing tests it finds outside its task as new
items, so some ranges allow one or two extra items. `count_ignore_re` leaves
such items out of the count instead, so a range can stay exact.

To add a scenario, add an entry to `scenarios.json` (a `prompt`, an
`expect_items` range, and the optional check keys above), and add any
pre-existing items to `make_fixture.sh`. Then build a correct store and a wrong
store by hand and confirm that only the correct one scores 1.0.

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

`candidates/baseline-0.2.0.md` is the `CLOVE.md` text of clove 0.2.0.

## Results so far

One run per scenario, Claude Sonnet as the agent:

| Text | s1–s7 | s8–s12 | Mean of s1–s12 |
|------|-------|--------|----------------|
| Current `CLOVE.md` | 1.0 on each | 1.0 on each | 1.0 |
| 0.2.0 baseline | 1.0, 0.75, 0.667, 1.0, 0.8, 0.83, 1.0 | 1.0 on each | 0.921 |

The baseline loses points for items without a body (`s2`, `s3`, `s5`) and for
conversation context in comments (`s6`). Single runs vary: the first baseline
runs of `s1` and `s4` scored 0.0 and 0.083 (the agent hit the nonexistent
`clove list` command, or filed nothing), and the reruns scored 1.0. So compare
texts over several runs per scenario before you trust a small difference.

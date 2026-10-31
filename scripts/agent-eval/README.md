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
| `dep_order` | No item is closed while one of its hard dependencies is open. |
| `children` | At least `min_children` items have a parent (an epic was set up). |
| `close_comment` | Every closed item has a comment that records the result. |

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
| `s13` | Implement an item that an open item blocks | 2 to 4; no item closed before its dependency |
| `s14` | A feature tracked as an epic with one child per part | 6 to 8; at least 5 children, all closed |
| `s15` | Three review comments | 3 to 5; one closed item per comment |
| `s16` | Fix one of two open bugs | 2; that bug closed, the other open, no duplicate |
| `s17` | Close an item whose work is already done | 1; closed with a result comment, no code change |

The fixture has defects outside each request: two failing tests, a README
typo, and a crash when no command is given. `CLOVE.md` tells an agent to file
such a defect as its own item. So `side_re` names them per scenario: an item
whose title matches is neither counted nor required to be closed (pre-existing
items always count), and each range is the exact number of requested items.

To add a scenario, add an entry to `scenarios.json` (a `prompt`, an
`expect_items` range, and the optional check keys above), and add any
pre-existing items to `make_fixture.sh`. Then build a correct store and a wrong
store by hand and confirm that only the correct one scores 1.0.

## Run one round

To run every scenario 3 times for one candidate and one model, run:

```sh
cargo build -p clove-cli --bin clove
CLOVE_BIN=$PWD/target/debug/clove \
  scripts/agent-eval/run_round.sh --runs 3 --model haiku CLOVE.md /tmp/agent-eval
```

- `--runs N` sets the runs per scenario (default 3). One run is not enough to
  compare two texts: the same text and scenario can score 0.0 in one run and
  1.0 in the next.
- `--model M` names the model. The default `AGENT_CMD` passes it to
  `claude -p`, and it is the directory name under the output directory, so
  rounds for several models can share one output directory.
- `AGENT_CMD` replaces the agent. It reads the prompt on stdin and runs inside
  the toy project. The default loads only project settings
  (`--setting-sources project`), so user hooks do not act on the rollout.
- To run only some scenarios, list them after the output directory.

Results go to `<out>/<model>/<scenario>/r<k>/` (`prompt.txt`, `agent.txt`,
`score.json`, and the toy project in `repo/`). To print the mean and minimum
score per scenario and model, run `aggregate.py <out>`; add `--markdown` for
a table.

If your environment does not allow a script to start an agent, then run each
rollout as a subagent: create the toy project with `make_fixture.sh`, give the
agent the output of `mkprompt.py`, and score it with `score.py`. Put the
results in the same layout so that `aggregate.py` reads them.

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

Current `CLOVE.md`, 3 runs per scenario and model (153 runs). Each cell is the
mean score, the minimum, and the run count:

| Scenario | haiku | opus | sonnet |
|---|---|---|---|
| s1 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s2 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s3 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s4 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s5 | 0.98 (min 0.93, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s6 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s7 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s8 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s9 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s10 | 0.93 (min 0.86, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s11 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s12 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s13 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s14 | 0.98 (min 0.93, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s15 | 0.94 (min 0.83, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s16 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| s17 | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) | 1.00 (min 1.00, n=3) |
| **mean** | 0.990 | 1.000 | 1.000 |

- Sonnet and Opus score 1.0 on every run.
- Haiku loses points only on `no_leak`: in 5 of 51 runs it writes
  conversation context into an item ("next week", "per the request",
  "Found while testing").
- Single runs vary. With the 0.2.0 text, Sonnet scored 0.0 and 0.083 on the
  first runs of `s1` and `s4`, and 1.0 on reruns. Compare texts over several
  runs per scenario.
- A rollout agent can see every tool of the session that starts it. Haiku
  rollouts called session and message tools in several runs, even with the
  prompt line that forbids it. Run rollouts where the agent has only shell and
  file tools, as the default `AGENT_CMD` does.

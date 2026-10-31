"""Print the rollout prompt for one candidate and one scenario.

Usage: mkprompt.py <candidate.md> <scenario-id> <rollout-dir>
CLOVE_BIN names the clove binary the agent is told to call (default: clove).
"""
import json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
cand_path, s, d = sys.argv[1], sys.argv[2], os.path.abspath(sys.argv[3])
cand = open(cand_path).read()
sc = json.load(open(os.path.join(HERE, "scenarios.json")))[s]
clove = os.environ.get("CLOVE_BIN", "clove")
reply = os.path.join(os.path.dirname(d), "reply.txt")
print(f"""You are a coding agent working in the repository at {d}. Do all work inside that directory: start each shell command with `cd {d} &&`. The `clove` command is installed; call it as `{clove}`. Python is `python3` (no pytest; tests run with `python3 tests/run.py`). Do not touch any other directory. Do not commit. Use only shell and file tools: do not start agents or sessions, send messages, or call any other tool.

The repository's CLAUDE.md contains these project instructions. Treat them exactly as you would treat a CLAUDE.md:

<claude_md>
{cand}</claude_md>

The user's message:

<user>
{sc['prompt']}
</user>

Complete the user's request. When you are done, reply with a two-sentence summary of what you did. Also write that reply, word for word, to the file {reply}.""")

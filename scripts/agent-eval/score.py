"""Score one rollout: read the .clove store a rollout left behind.

Usage: score.py <rollout-dir> <scenario-id>
CLOVE_BIN overrides the clove binary (default: clove on PATH).
"""
import json, os, re, subprocess, sys
HERE = os.path.dirname(os.path.abspath(__file__))
C = os.environ.get("CLOVE_BIN", "clove")
LEAK = re.compile(r"\b(the user|user asked|user wants|you asked|as requested|requested by|this (conversation|session|chat)|as discussed|we discussed|while (i'm|you're|i was) at it|per (your|the) request|colleague|i'd like|the prompt|asked me|next week|don't forget|so we don't|someone can pick|pick it up|manager|friday|deadline|we talked|talked about|you suggested|i agree|agreed|found while|while fixing|outside (this|the) (request|scope)|not fixed here)\b", re.I)
def j(args, cwd):
    out = subprocess.run([C] + args + ["--format", "json"], cwd=cwd, capture_output=True, text=True).stdout
    return json.loads(out)["data"]
ID_RE = re.compile(r"[a-z][a-z0-9]{0,7}-[0-9A-Z]{8}")
def norm(text):
    # Compare titles without the quoting an agent may add or drop.
    return re.sub(r"\s+", " ", re.sub(r"[`'\"*]", "", text)).lower()
def changed_files(d):
    out = subprocess.run(["git", "status", "--porcelain"], cwd=d, capture_output=True, text=True).stdout
    # .clove/ is the tracker; __pycache__ and .pyc files appear whenever an
    # agent runs the tests, so neither counts as a change.
    return [l for l in out.splitlines() if ".clove/" not in l and "__pycache__" not in l and not l.endswith(".pyc")]
def matching(det, pattern):
    return [x for x in det if re.search(pattern, x[0]["title"], re.I)]
def score(d, sc):
    """Each optional check runs only when the scenario declares its key."""
    items = j(["ls", "--limit", "200"], d)
    det = []
    for it in items:
        s = j(["show", it["id"]], d)
        cs = j(["comments", it["id"]], d) if s.get("comment_count") else []
        det.append((s, cs))
    n = len(items)
    lo, hi = sc["expect_items"]
    pre = set(sc.get("preexisting_titles", []))
    # The fixture has known defects outside each request (two failing tests,
    # a README typo, a crash on a missing command). CLOVE.md tells an agent to
    # file such a defect as its own item, so an item that `side_re` names is
    # neither counted nor required to be closed. Pre-existing items always count.
    side_re = sc.get("side_re") or sc.get("count_ignore_re")
    side = [x for x in det if side_re and x[0]["title"] not in pre and re.search(side_re, x[0]["title"], re.I)]
    side_ids = {x[0]["id"] for x in side}
    counted = n - len(side)
    checks = {}
    checks["count"] = 1.0 if lo <= counted <= hi else max(0.0, 1 - abs(counted - (lo if counted < lo else hi)) / max(hi, 1))
    unrelated = set(sc.get("unrelated_titles", []))
    new = [x for x in det if x[0]["title"] not in pre]
    if sc.get("need_closed"):
        # The requested work must be closed. Items filed for problems outside
        # the request may stay open, so `closed_re` narrows the set.
        work = matching(det, sc["closed_re"]) if sc.get("closed_re") else [x for x in det if x[0]["title"] not in unrelated]
        work = [x for x in work if x[0]["id"] not in side_ids]
        checks["closed"] = (sum(1 for s, _ in work if s["status"] == "closed") / len(work)) if work else 0.0
    if sc.get("open_re"):
        # Deferred work must be tracked and left open.
        deferred = matching(det, sc["open_re"])
        checks["open"] = (sum(1 for s, _ in deferred if s["status"] == "open") / len(deferred)) if deferred else 0.0
    if sc.get("all_open"):
        checks["all_open"] = (sum(1 for s, _ in det if s["status"] == "open") / n) if n else 0.0
    if new:
        checks["bodies"] = sum(1 for s, _ in new if len((s.get("body") or "").strip()) >= 30) / len(new)
        text = " ".join([s["title"] + " " + (s.get("body") or "") + " " + " ".join(c["body"] for c in cs) for s, cs in det])
        hits = LEAK.findall(text)
        checks["no_leak"] = 1.0 if not hits else max(0.0, 1 - 0.34 * len(hits))
    if sc.get("need_dep"):
        checks["dep"] = 1.0 if any(s["deps"] for s, _ in det) else 0.0
    if sc.get("dup_re"):
        checks["no_dup"] = 0.0 if len(matching(det, sc["dup_re"])) > 1 else 1.0
    if sc.get("dep_order"):
        # No item is closed while one of its hard dependencies is still open.
        status = {x[0]["id"]: x[0]["status"] for x in det}
        closed = [x for x in det if x[0]["status"] == "closed"]
        bad = [x for x in closed if any(status.get(dep) != "closed" for dep in x[0]["deps"])]
        checks["dep_order"] = 0.0 if bad else 1.0
    if sc.get("min_children"):
        # A request for an epic: at least this many items have a parent.
        children = [x for x in det if x[0].get("parent")]
        checks["children"] = min(1.0, len(children) / sc["min_children"])
    if sc.get("close_comment"):
        # Every closed item carries a comment that records the result.
        closed = [x for x in det if x[0]["status"] == "closed"]
        checks["close_comment"] = (sum(1 for _, cs in closed if cs) / len(closed)) if closed else 0.0
    if sc.get("no_change"):
        checks["no_change"] = 0.0 if changed_files(d) else 1.0
    # The reply to the user must name each item as `<id>` (<title>): every
    # mention of an id that exists in the store has the title right after it.
    reply_path = os.path.join(os.path.dirname(os.path.abspath(d)), "reply.txt")
    reply_ids = None
    if os.path.exists(reply_path):
        reply = open(reply_path).read()
        titles = {x[0]["id"]: norm(x[0]["title"]) for x in det}
        mentions = [m for m in ID_RE.finditer(reply) if m.group(0) in titles]
        reply_ids = len(mentions)
        if mentions:
            # The title must start within a few characters of the id, as in
            # `id` (title); the window allows the quoting and the parenthesis.
            def titled(m):
                t = titles[m.group(0)]
                return t in norm(reply[m.end():m.end() + len(t) + 12])
            checks["id_titles"] = sum(1 for m in mentions if titled(m)) / len(mentions)
    total = sum(checks.values()) / len(checks)
    return {"score": round(total, 3), "checks": {k: round(v, 2) for k, v in checks.items()}, "n_items": n, "reply_ids": reply_ids,
            "items": [{"title": s["title"], "type": s["type"], "status": s["status"], "deps": s["deps"], "body": (s.get("body") or "")[:300], "comments": [c["body"][:200] for c in cs]} for s, cs in det]}
if __name__ == "__main__":
    scs = json.load(open(os.path.join(HERE, "scenarios.json")))
    print(json.dumps(score(sys.argv[1], scs[sys.argv[2]]), indent=1))

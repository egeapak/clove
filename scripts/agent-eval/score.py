"""Score one rollout: read the .clove store a rollout left behind.

Usage: score.py <rollout-dir> <scenario-id>
CLOVE_BIN overrides the clove binary (default: clove on PATH).
"""
import json, os, re, subprocess, sys
HERE = os.path.dirname(os.path.abspath(__file__))
C = os.environ.get("CLOVE_BIN", "clove")
LEAK = re.compile(r"\b(the user|user asked|user wants|you asked|as requested|requested by|this (conversation|session|chat)|as discussed|we discussed|while (i'm|you're|i was) at it|per (your|the) request|colleague|i'd like|the prompt|asked me|manager|friday|deadline|we talked|talked about|you suggested|i agree|agreed|found while|while fixing|outside (this|the) (request|scope)|not fixed here)\b", re.I)
def j(args, cwd):
    out = subprocess.run([C] + args + ["--format", "json"], cwd=cwd, capture_output=True, text=True).stdout
    return json.loads(out)["data"]
def score(d, sc):
    items = j(["ls", "--limit", "200"], d)
    pre = sc.get("preexisting", 0)
    det = []
    for it in items:
        s = j(["show", it["id"]], d)
        cs = j(["comments", it["id"]], d) if s.get("comment_count") else []
        det.append((s, cs))
    n = len(items)
    lo, hi = sc["expect_items"]
    checks = {}
    checks["count"] = 1.0 if lo <= n <= hi else max(0.0, 1 - abs(n - (lo if n < lo else hi)) / max(hi, 1))
    PRE = ("average() raises ZeroDivisionError on an empty list", "Support negative numbers in the README example")
    new = [x for x in det if not (pre and x[0]["title"] in PRE)]
    work = [x for x in det if x[0]["title"] != PRE[1] or not pre]
    if sc.get("work_re"):
        work = [x for x in det if re.search(sc["work_re"], x[0]["title"], re.I)]
    if sc["need_closed"]:
        checks["closed"] = (sum(1 for s, _ in work if s["status"] == "closed") / len(work)) if work else 0.0
    if new:
        checks["bodies"] = sum(1 for s, _ in new if len((s.get("body") or "").strip()) >= 30) / len(new)
        text = " ".join([s["title"] + " " + (s.get("body") or "") + " " + " ".join(c["body"] for c in cs) for s, cs in det])
        hits = LEAK.findall(text)
        checks["no_leak"] = 1.0 if not hits else max(0.0, 1 - 0.34 * len(hits))
    if sc.get("need_dep"):
        checks["dep"] = 1.0 if any(s["deps"] for s, _ in det) else 0.0
    if sc.get("reuse_title"):
        checks["no_dup"] = 0.0 if sum(1 for s, _ in det if re.search(r"average", s["title"], re.I)) > 1 else 1.0
    total = sum(checks.values()) / len(checks)
    return {"score": round(total, 3), "checks": {k: round(v, 2) for k, v in checks.items()}, "n_items": n,
            "items": [{"title": s["title"], "type": s["type"], "status": s["status"], "deps": s["deps"], "body": (s.get("body") or "")[:300], "comments": [c["body"][:200] for c in cs]} for s, cs in det]}
if __name__ == "__main__":
    scs = json.load(open(os.path.join(HERE, "scenarios.json")))
    print(json.dumps(score(sys.argv[1], scs[sys.argv[2]]), indent=1))

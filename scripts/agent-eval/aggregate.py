"""Print the mean and minimum score per scenario and model.

Usage: aggregate.py <out-dir> [--markdown]

Reads <out-dir>/<model>/<scenario>/r<k>/score.json, as run_round.sh writes it.
A run without a score.json is reported as missing, not as a zero.
"""
import glob, json, os, sys

def natural(s):
    return (len(s), s)

def collect(out):
    table = {}
    for path in glob.glob(os.path.join(out, "*", "*", "r*", "score.json")):
        run_dir = os.path.dirname(path)
        scenario_dir = os.path.dirname(run_dir)
        model = os.path.basename(os.path.dirname(scenario_dir))
        scenario = os.path.basename(scenario_dir)
        with open(path) as f:
            score = json.load(f)["score"]
        table.setdefault(scenario, {}).setdefault(model, []).append(score)
    return table

def main():
    out = sys.argv[1]
    md = "--markdown" in sys.argv
    table = collect(out)
    models = sorted({m for row in table.values() for m in row})
    scenarios = sorted(table, key=natural)
    if md:
        print("| Scenario | " + " | ".join(models) + " |")
        print("|---|" + "---|" * len(models))
    else:
        print("scenario  " + "  ".join(f"{m:>18}" for m in models))
    totals = {m: [] for m in models}
    for s in scenarios:
        cells = []
        for m in models:
            runs = table[s].get(m, [])
            totals[m].extend(runs)
            cells.append(f"{sum(runs)/len(runs):.2f} (min {min(runs):.2f}, n={len(runs)})" if runs else "missing")
        print((f"| {s} | " + " | ".join(cells) + " |") if md else f"{s:<9} " + "  ".join(f"{c:>18}" for c in cells))
    cells = [f"{sum(v)/len(v):.3f}" if v else "missing" for v in (totals[m] for m in models)]
    print((f"| **mean** | " + " | ".join(cells) + " |") if md else f"{'mean':<9} " + "  ".join(f"{c:>18}" for c in cells))

if __name__ == "__main__":
    main()

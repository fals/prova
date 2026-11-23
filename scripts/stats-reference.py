#!/usr/bin/env python3
"""Reference vectors for crates/prova-core/src/stats.rs, from SciPy.

Usage: python3 scripts/stats-reference.py > crates/prova-core/tests/stats_reference.json
Hodges-Lehmann intervals follow R's wilcox.test(conf.int=TRUE) formula,
with the exact null distribution of U enumerated by brute force.
"""
import itertools, json, random
from scipy import stats

def u_cdf_bruteforce(n, m):
    counts = {}
    for xs in itertools.combinations(range(n + m), n):
        xs = set(xs)
        # U of x: pairs where x ranks above y
        u = sum(1 for i in xs for j in range(n + m) if j not in xs and i > j)
        counts[u] = counts.get(u, 0) + 1
    total = sum(counts.values())
    acc, cdf = 0, []
    for u in range(n * m + 1):
        acc += counts.get(u, 0)
        cdf.append(acc / total)
    return cdf

def hl(x, y, alpha):
    n, m = len(x), len(y)
    diffs = sorted(b - a for b in y for a in x)
    cdf = u_cdf_bruteforce(n, m)
    qu = next(u for u, p in enumerate(cdf) if p >= alpha / 2)
    qu = max(qu, 1)
    est = float(stats.scoreatpercentile(diffs, 50)) if len(diffs) % 2 else (diffs[len(diffs)//2 - 1] + diffs[len(diffs)//2]) / 2
    return {"estimate": est, "lower": diffs[qu - 1], "upper": diffs[n * m - qu]}

rng = random.Random(7)
cases = []
for n, m in [(3, 3), (4, 5), (5, 5), (6, 4), (8, 8), (10, 10)]:
    for shift in [0.0, 0.3, 1.5]:
        x = [round(rng.lognormvariate(0, 0.4), 6) for _ in range(n)]
        y = [round(rng.lognormvariate(shift, 0.4), 6) for _ in range(m)]
        r = stats.mannwhitneyu(x, y, alternative="two-sided", method="exact")
        case = {"x": x, "y": y, "u": float(r.statistic), "p": float(r.pvalue), "method": "exact"}
        if n * m <= 64:
            case["hl"] = hl(x, y, 0.05)
        cases.append(case)
# ties: asymptotic with continuity correction
for x, y in [([1, 2, 2, 3, 4], [2, 3, 3, 5, 6, 6]), ([10, 10, 11, 12, 12, 12], [12, 13, 13, 14, 10, 15])]:
    r = stats.mannwhitneyu(x, y, alternative="two-sided", method="asymptotic", use_continuity=True)
    cases.append({"x": [float(v) for v in x], "y": [float(v) for v in y], "u": float(r.statistic), "p": float(r.pvalue), "method": "asymptotic"})

ps = [0.001, 0.04, 0.03, 0.2, 0.012]
# Holm by definition (statsmodels not required)
order = sorted(range(len(ps)), key=lambda i: ps[i])
adj, running = [0.0] * len(ps), 0.0
for rank, i in enumerate(order):
    running = max(running, min(1.0, (len(ps) - rank) * ps[i]))
    adj[i] = running

print(json.dumps({"mann_whitney": cases, "holm": {"p": ps, "adjusted": adj}}, indent=1))

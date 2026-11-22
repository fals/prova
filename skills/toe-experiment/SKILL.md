---
name: toe-experiment
description: Measure whether a change improves performance (latency, throughput, memory, any numeric metric) with a pre-registered protocol and a statistical verdict. Use for optimisation, benchmarking, load testing, or any "is it faster/better" question, instead of timing commands by hand.
---

# Experiments with toe

One run proves nothing. toe runs baseline and candidate interleaved,
several times, and decides with a rank test and a confidence interval.

## 1. Declare before measuring

```
toe exp new <name> --hypothesis "<what you expect and why>" \
  --metric wall_ms --better lower --min-effect 2% \
  --trials 10 --max-trials 20 -- <workload command>
```

- Baseline is `HEAD` (committed code). Make changes uncommitted.
- The workload must be deterministic in its inputs. It may print metrics
  as JSON lines: `{"metric": "p99_ms", "value": 12.3, "unit": "ms"}`.
  `wall_ms` of the whole command is always recorded.
- `--guardrail mem_kb` (or `NAME:higher`) rejects a change that makes
  that metric worse.
- The protocol freezes at the first trial. To change it, start a new
  experiment. Do not try to edit it after seeing data.

## 2. Measure a candidate

```
<edit code>
toe exp candidate <name>          # records the working tree as c1, c2, ...
toe exp run <name> --detach
toe exp wait <name> --max-wait 30s
```

Do not edit files while trials run; toe refuses a candidate whose files
changed since it was recorded.

## 3. Act on the verdict

- `ACCEPT`: the improvement interval clears the minimum effect. Keep it;
  it becomes the new best. Next ideas are compared against it.
- `REJECT`: worse, no practical effect, or a guardrail regressed. Revert.
- `INVALID`: trials ran on different machines; rerun on one machine.
- Failed comparison: a trial crashed; read `.toe/experiments/<id>/`.

Before reporting a final result: `toe exp revalidate <name>` compares the
best against the original baseline with fresh trials.
`toe exp status <name>` shows the decision log.

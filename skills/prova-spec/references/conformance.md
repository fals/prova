# Trace conformance: replay the specification against the code

## Generate traces

```sh
quint run spec/ipam.qnt --mbt --max-steps 20 --n-traces 50 --max-samples 50 \
  --out-itf 'traces/ipam_{seq}.itf.json'
```

`--n-traces` is the number of files written. Commit a handful of traces as
regression fixtures; generate fresh ones in the oracle so new paths are
explored on every run.

## ITF format (what the driver reads)

One JSON file per trace, `states` is the list of states, in order:

```json
{"#meta": {"index": 2},
 "assigned": {"#map": [["c", {"#bigint": "1"}]]},
 "mbt::actionTaken": "add",
 "mbt::nondetPicks": {"pod": {"tag": "Some", "value": "c"},
                      "ip":  {"tag": "Some", "value": {"#bigint": "1"}}}}
```

- State variables appear by name. Integers are `{"#bigint": "1"}`, maps
  `{"#map": [[k, v], ...]}`, sets `{"#set": [...]}`, tuples `{"#tup": [...]}`,
  sum types `{"tag": "Name", "value": ...}`.
- `mbt::actionTaken` is the action that produced this state (`init` first).
- `mbt::nondetPicks` holds every `nondet` value; `tag: "None"` when the
  action did not use it.

## Driver shape (any language)

```
for each trace:
    impl = fresh implementation
    for each state s:
        apply s["mbt::actionTaken"] to impl with the values in mbt::nondetPicks
        compare impl's observable state with s; on mismatch print
            trace, step, action, spec state, implementation state; fail
exit non-zero if any trace diverged
```

Where the spec is nondeterministic (`nondet ip = free.oneOf()`), the
implementation may choose differently. Either force the choice (pass the
picked value into the implementation, if it has such an input) or compare
only what the spec fixes (which pods have an address) and check the
invariants on the rest (addresses unique, in the pool). Errors the spec
models as "state unchanged" are expected exceptions, not failures.

## Python driver (tested against `examples/ipam.qnt`)

```python
"""Replays ITF traces of spec/ipam.qnt against Ipam; exits 1 on divergence."""
import glob, json, sys
from ipam import Ipam

def val(v):  # ITF value -> Python
    if isinstance(v, dict) and "#bigint" in v: return int(v["#bigint"])
    if isinstance(v, dict) and "#map" in v: return {val(k): val(x) for k, x in v["#map"]}
    if isinstance(v, dict) and "#set" in v: return {val(x) for x in v["#set"]}
    return v

def pick(state, name):
    p = state["mbt::nondetPicks"][name]
    return val(p["value"]) if p["tag"] == "Some" else None

failed = 0
for path in sorted(glob.glob(sys.argv[1] + "/*.itf.json")):
    impl = Ipam({1, 2, 3})
    for i, s in enumerate(json.load(open(path))["states"]):
        action, pod = s["mbt::actionTaken"], pick(s, "pod")
        if action == "add":
            try: impl.add(pod)
            except RuntimeError: pass          # spec: exhausted pool leaves state unchanged
        elif action == "del":
            impl.delete(pod)
        want = val(s["assigned"])
        ok = set(impl.assigned) == set(want) \
            and len(set(impl.assigned.values())) == len(impl.assigned) \
            and set(impl.assigned.values()) <= {1, 2, 3}
        if not ok:
            print(f"{path} step {i} {action}({pod}): spec {want}, implementation {impl.assigned}")
            failed += 1
            break
sys.exit(1 if failed else 0)
```

An implementation that forgets to release addresses on DEL fails at the
first ADD after the pool should have had room:
`step 10 add(a): spec {'a': 2, 'd': 3}, implementation {'d': 1}`.

## Other languages

The same loop, with the project's JSON library:

- Rust: `serde_json::Value`; run as an integration test so `prova run` sees
  one test per trace. `quint-connect` exists but is young.
- Go: `encoding/json` into `map[string]any`; one `t.Run(trace)` per file.
- TypeScript: `JSON.parse`; one Vitest `test.each` case per file.
- Java / C#: Jackson / System.Text.Json; one parameterized test per file.

Making each trace a test case means prova reports the diverging trace by name,
remembers it, and flags it as flaky if the implementation is
nondeterministic.

## The oracle command

```make
conformance:
	quint run spec/ipam.qnt --invariant safety --max-steps 20 | grep -q "No violation"
	rm -rf traces/gen && mkdir -p traces/gen
	quint run spec/ipam.qnt --mbt --max-steps 20 --n-traces 50 --max-samples 50 \
	  --out-itf 'traces/gen/ipam_{seq}.itf.json' > /dev/null
	python3 conformance.py traces/gen
```

It checks the spec still holds, then the implementation against it. Pass it
to `prova exp new --oracle "make conformance"`.

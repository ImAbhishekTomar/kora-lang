# Benchmarks

Twelve programs that measure what the Kora runtime costs today, plus a harness
that can tell you whether a change made it worse.

The point is not a number to put on a slide. The point is that the tree-walking
interpreter is a known, temporary stage ([DECISIONS.md](../DECISIONS.md),
"Execution strategy"), and a stage you cannot measure is a stage you cannot
leave. These numbers are also the honest answer to "is Kora fast?", which the
docs answer with "not yet, and here is exactly where".

## Running them

```bash
cargo build --release -p kora-cli
python3 scripts/bench.py
```

That prints a table and nothing else. Three flags change what it does:

| | |
|---|---|
| `--save-baseline` | record this run as `benches/baseline.json` |
| `--compare` | measure, then diff against that baseline |
| `--against <ref>` | build `<ref>` too, and A/B the two on this machine |

`--against` is the one to trust:

```bash
python3 scripts/bench.py --against main
```

It builds the other revision in a detached worktree, then alternates the two
binaries repetition by repetition. Both see the same CPU, the same neighbours,
and the same thermal state, so the ratio means something even on a machine
that is busy doing other work. It fails above **1.25x**.

`--compare` is looser (**1.5x**) because it compares against a file, and a file
does not know what machine it was recorded on. Useful locally, meaningless
across machines.

The baseline revision is built in `target/bench-worktree` with its own
`target/bench-baseline` directory, which is kept on purpose: sharing one target
directory would make the two builds evict each other and the second one would
be measured against a cold cache. The worktree is removed after the run; the
build cache is not.

Other flags: `--filter arith` for one benchmark, `--reps N` for more samples,
`--json out.json` to keep the raw numbers.

## What each one measures

Every benchmark checks its own output before timing it. A program that got
faster by doing less work is a regression that reports as an improvement.

| benchmark | measures | why it is in the set |
| --- | --- | --- |
| `startup` | process start, parse, name resolution | the floor under every other row; subtract it before reading them |
| `arith` | integer and float arithmetic in a loop | the closest thing to pure evaluator overhead, and the first thing a bytecode VM should move |
| `calls` | frame setup, argument binding, return | recursion plus a flat call loop, which stress different paths |
| `collections` | list growth, indexing, dict work, sorting | where value representation (`Rc`, boxing, hashing) shows up |
| `strings` | f-strings, concatenation, slicing | agent programs build prompts; this path is hot in real code |
| `json` | `json.parse` and `json.get` over 4000 elements | the stdlib is native Rust, so this should stay flat as the evaluator changes |
| `regex` | `re.find_all` and `re.replace` over 200KB | same, and it guards the compile cache |
| `csv` | 20000 quoted rows, parsed ten times | the largest realistic input an agent program hands the stdlib |
| `sequential` | eight units of CPU work on one thread | the baseline for the row below |
| `parallel` | the same eight units through `parallel for` | branch spawn, heap isolation, copying values in and results out |
| `durable_off` | 500 effects, not journaled | the baseline for the row below |
| `durable_on` | the same 500 effects with `--durable` | what "survives being killed" costs |

The last two pairs exist because a single number cannot answer the question.
`parallel` alone says nothing without `sequential`; `durable_on` alone says
nothing without `durable_off`. The harness prints the parallel speedup for you.

## What the numbers say today

Recorded with `--save-baseline` on an Apple M-series laptop, 10 CPUs, Kora
0.0.2. Your machine will differ; the shape will not.

| benchmark | best of 7 |
| --- | ---: |
| `startup` | 4.3 ms |
| `arith` | 220 ms |
| `calls` | 162 ms |
| `collections` | 200 ms |
| `strings` | 309 ms |
| `json` | 234 ms |
| `regex` | 104 ms |
| `csv` | 172 ms |
| `sequential` | 426 ms |
| `parallel` | 118 ms |
| `durable_off` | 6.4 ms |
| `durable_on` | 200 ms |

Three things worth saying out loud:

1. **`parallel for` gives 3.6x on 10 CPUs** for this workload. Not 10x: each
   branch is a fresh agent with its own heap, and the values it needs are
   copied in. That overhead is real and it is the price of having no locks and
   no GIL.
2. **The evaluator is the slow part, not the stdlib.** `arith` runs 400,000
   iterations of a three-operation expression in 220 ms; `csv` parses 200,000
   rows in 172 ms. Work that reaches Rust is fast, work that stays in the tree
   walker is not. This is the exact gap a bytecode VM closes.
3. **Durability costs about 0.4 ms per effect here**, and it grows with the
   run: the journal is rewritten whole on each effect, so a long run pays more
   per effect than a short one. Known, measured, and now visible when it is
   fixed.

## Breaking points, not just throughput

`scripts/bench.py` answers "how much does this cost". `scripts/stress.py`
answers a different question: "at what size does this stop working" —
deepest recursion before the interpreter gives up, largest string
accumulation, most `--durable` effects, widest `parallel for` fan-out. Each
probe runs under a watchdog (RSS cap, timeout, and a system-memory abort
switch) so finding a real limit never means also hanging the machine it runs
on.

```bash
python3 scripts/stress.py                  # every probe
python3 scripts/stress.py --filter recur    # one probe
python3 scripts/stress.py --history         # append to benches/stress_history.jsonl
python3 scripts/stress.py --against-history # flag a probe that broke at a smaller N than last time
```

`scripts/bench.py --history` / `--against-history` do the same thing for the
throughput numbers, appending to `benches/history.jsonl` — a running record
to diff a change against, independent of the single committed
`baseline.json` snapshot.

## In CI

The `benchmarks` job runs on every pull request. It builds the base commit,
A/Bs it against the branch on the same runner, and posts the table to the job
summary. A benchmark more than 1.25x slower fails the job.

A shared runner is noisy, so if a failure looks like noise, re-run the job
before believing it. If a change is *meant* to cost time (a new check on a hot
path, say), say so in the pull request and raise `--tolerance` in the workflow
in the same commit, rather than deleting the benchmark.

On pushes to `main` the job only checks that every benchmark still runs, which
keeps these programs from rotting when the language changes.

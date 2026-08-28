#!/usr/bin/env python3
"""Kora's benchmark set: what the runtime costs today, and whether a change
made it worse.

Three ways to run it, in increasing order of trust:

    python3 scripts/bench.py                      # measure this build
    python3 scripts/bench.py --compare            # against benches/baseline.json
    python3 scripts/bench.py --against origin/main  # A/B on this machine

The last one is the only measurement that is safe to gate CI on. Wall time on
a shared runner drifts by tens of percent between jobs, so a number recorded
on one machine says very little about a number recorded on another. Building
both revisions and running them back to back, interleaved, cancels almost all
of that: the two binaries see the same CPU, the same neighbours, and the same
thermal state.

`--compare` against the committed baseline is still useful locally, where the
machine does not change between runs. It is deliberately loose.

Every benchmark checks its own output. A program that got faster by doing
less work is a regression that reports as an improvement, which is worse than
no benchmark at all.
"""

import argparse
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH_DIR = ROOT / "benches"
BASELINE = BENCH_DIR / "baseline.json"
DEFAULT_BIN = ROOT / "target" / "release" / "kora"

# name, file, extra flags, a string the output must contain, and what the
# benchmark is actually measuring.
BENCHMARKS = [
    ("startup", "startup.ko", [], "ok",
     "process start, parse, name resolution: the floor under every other row"),
    ("arith", "arith.ko", [], "arith: 333000000",
     "integer and float arithmetic in a tight loop: raw evaluator overhead"),
    ("calls", "calls.ko", [], "fib: 28657",
     "function call overhead: frame setup, argument binding, return"),
    ("collections", "collections.ko", [], "collections: 7199940000 512",
     "list growth, indexing, dict insert and lookup, sorting"),
    ("strings", "strings.ko", [], "strings: 480000",
     "f-string formatting, concatenation, slicing, membership"),
    ("json", "json_bench.ko", [], "json: 400000 ada",
     "the json module: parse and path walk over a 4000-element document"),
    ("regex", "regex_bench.ko", [], "re: 800000",
     "the re module: find_all and replace over 200KB of text"),
    ("csv", "csv_bench.ko", [], "csv: 200000",
     "the csv module: 20000 rows with quoted fields, parsed ten times"),
    ("sequential", "sequential.ko", [], "sequential:",
     "eight units of CPU work on one thread: the parallel baseline"),
    ("parallel", "parallel.ko", [], "parallel:",
     "the same eight units through `parallel for`: fan-out and copy cost"),
    ("durable_off", "durable.ko", [], "step 499",
     "500 effects, not journaled"),
    ("durable_on", "durable.ko", ["--durable"], "step 499",
     "the same 500 effects, journaled to disk: what durability costs"),
]


def clean_state():
    """Durable runs leave a journal behind; a growing directory would show up
    as a slow drift in the numbers."""
    shutil.rmtree(BENCH_DIR / ".kora", ignore_errors=True)


def measure(binary, bench, reps, warmup):
    name, filename, flags, expect, _ = bench
    cmd = [str(binary), "run", *flags, str(BENCH_DIR / filename)]
    samples = []
    for i in range(warmup + reps):
        clean_state()
        start = time.perf_counter()
        done = subprocess.run(cmd, capture_output=True, text=True)
        elapsed = (time.perf_counter() - start) * 1000.0
        if done.returncode != 0:
            raise SystemExit(
                f"{name}: `{' '.join(cmd)}` exited {done.returncode}\n{done.stderr.strip()}"
            )
        if expect not in done.stdout:
            raise SystemExit(
                f"{name}: output check failed. Expected to find {expect!r}.\n"
                f"Got: {done.stdout.strip()[:400]}"
            )
        if i >= warmup:
            samples.append(elapsed)
    clean_state()
    return {
        "min_ms": round(min(samples), 2),
        "median_ms": round(statistics.median(samples), 2),
        "samples": len(samples),
    }


def run_set(binary, benchmarks, reps, warmup, label):
    print(f"# measuring {label}", file=sys.stderr)
    results = {}
    for bench in benchmarks:
        results[bench[0]] = measure(binary, bench, reps, warmup)
        print(f"  {bench[0]:<12} {results[bench[0]]['min_ms']:>8.2f} ms", file=sys.stderr)
    return results


def machine():
    return {
        "os": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
        "cpus": os.cpu_count(),
        "python": platform.python_version(),
    }


def build_ref(ref, verbose):
    """Build `ref` in a detached worktree so both binaries exist at once.

    The worktree gets its own target directory. Sharing one with the working
    tree would make the two builds evict each other's artifacts, and the
    second build would be measured against a cold cache."""
    work = ROOT / "target" / "bench-worktree"
    target = ROOT / "target" / "bench-baseline"
    subprocess.run(["git", "worktree", "remove", "--force", str(work)],
                   cwd=ROOT, capture_output=True)
    subprocess.run(["git", "worktree", "add", "--detach", str(work), ref],
                   cwd=ROOT, check=True, capture_output=not verbose)
    env = dict(os.environ, CARGO_TARGET_DIR=str(target))
    subprocess.run(["cargo", "build", "--release", "-p", "kora-cli"],
                   cwd=work, check=True, env=env, capture_output=not verbose)
    return work, target / "release" / "kora"


def drop_worktree(work):
    subprocess.run(["git", "worktree", "remove", "--force", str(work)],
                   cwd=ROOT, capture_output=True)


def interleaved(baseline_bin, candidate_bin, benchmarks, reps, warmup):
    """Alternate the two binaries per repetition rather than running one set
    and then the other, so a machine that slows down halfway through slows
    both by the same amount."""
    out = {name: {"baseline": [], "candidate": []} for name, *_ in benchmarks}
    for bench in benchmarks:
        name, filename, flags, expect, _ = bench
        for i in range(warmup + reps):
            for side, binary in (("baseline", baseline_bin), ("candidate", candidate_bin)):
                clean_state()
                cmd = [str(binary), "run", *flags, str(BENCH_DIR / filename)]
                start = time.perf_counter()
                done = subprocess.run(cmd, capture_output=True, text=True)
                elapsed = (time.perf_counter() - start) * 1000.0
                if done.returncode != 0 or expect not in done.stdout:
                    raise SystemExit(f"{name} ({side}): run failed\n{done.stderr.strip()[:400]}")
                if i >= warmup:
                    out[name][side].append(elapsed)
        b = min(out[name]["baseline"])
        c = min(out[name]["candidate"])
        print(f"  {name:<12} {b:>8.2f} -> {c:>8.2f} ms  ({c / b:.2f}x)", file=sys.stderr)
    clean_state()
    return {n: {"baseline_ms": round(min(v["baseline"]), 2),
                "candidate_ms": round(min(v["candidate"]), 2)}
            for n, v in out.items()}


def speedup_row(results, key="min_ms"):
    if "parallel" in results and "sequential" in results:
        seq = results["sequential"][key]
        par = results["parallel"][key]
        if par > 0:
            return seq / par
    return None


def table(results, notes):
    rows = ["| benchmark | min | median | measures |",
            "| --- | ---: | ---: | --- |"]
    for name, r in results.items():
        rows.append(f"| `{name}` | {r['min_ms']:.2f} ms | {r['median_ms']:.2f} ms | {notes[name]} |")
    return "\n".join(rows)


def comparison_table(results, tolerance):
    rows = ["| benchmark | before | after | change |", "| --- | ---: | ---: | ---: |"]
    regressions = []
    for name, r in results.items():
        before, after = r["baseline_ms"], r["candidate_ms"]
        ratio = after / before if before else 1.0
        delta = after - before
        # A ratio on a 4 ms benchmark is mostly process-start jitter, so a
        # regression has to be visible in absolute time as well.
        bad = ratio > tolerance and delta > 3.0
        mark = " ⚠️" if bad else ""
        rows.append(f"| `{name}` | {before:.2f} ms | {after:.2f} ms | {ratio:.2f}x{mark} |")
        if bad:
            regressions.append((name, before, after, ratio))
    return "\n".join(rows), regressions


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--bin", default=str(DEFAULT_BIN), help="the kora binary to measure")
    p.add_argument("--reps", type=int, default=7, help="timed repetitions per benchmark")
    p.add_argument("--warmup", type=int, default=2, help="untimed repetitions first")
    p.add_argument("--filter", default=None, help="only benchmarks whose name contains this")
    p.add_argument("--json", default=None, help="write results to this file")
    p.add_argument("--save-baseline", action="store_true",
                   help="record this run as benches/baseline.json")
    p.add_argument("--compare", action="store_true",
                   help="compare against benches/baseline.json (same machine only)")
    p.add_argument("--against", default=None, metavar="REF",
                   help="build REF and A/B it against this working tree")
    p.add_argument("--tolerance", type=float, default=None,
                   help="fail above this ratio (default 1.25 for --against, 1.5 for --compare)")
    p.add_argument("--verbose", action="store_true", help="show the baseline build")
    args = p.parse_args()

    benchmarks = [b for b in BENCHMARKS if not args.filter or args.filter in b[0]]
    if not benchmarks:
        raise SystemExit(f"no benchmark matches {args.filter!r}")
    notes = {b[0]: b[4] for b in BENCHMARKS}

    binary = Path(args.bin)
    if not binary.exists():
        raise SystemExit(f"{binary} does not exist. Build it first:\n"
                         f"  cargo build --release -p kora-cli")

    if args.against:
        tolerance = args.tolerance if args.tolerance is not None else 1.25
        work, baseline_bin = build_ref(args.against, args.verbose)
        try:
            results = interleaved(baseline_bin, binary, benchmarks, args.reps, args.warmup)
        finally:
            drop_worktree(work)
        rendered, regressions = comparison_table(results, tolerance)
        print(f"## Benchmarks vs `{args.against}`\n")
        print(rendered)
        print(f"\nSame machine, interleaved, {args.reps} timed repetitions, "
              f"best of each. Flagged above {tolerance:.2f}x.")
        if args.json:
            Path(args.json).write_text(json.dumps(
                {"against": args.against, "machine": machine(), "results": results}, indent=2) + "\n")
        if regressions:
            print("\nRegressions:")
            for name, before, after, ratio in regressions:
                print(f"  {name}: {before:.2f} ms -> {after:.2f} ms ({ratio:.2f}x)")
            return 1
        return 0

    results = run_set(binary, benchmarks, args.reps, args.warmup, str(binary))
    payload = {"version": subprocess.run([str(binary), "--version"], capture_output=True,
                                         text=True).stdout.strip(),
               "machine": machine(), "reps": args.reps, "results": results}

    print("## Benchmarks\n")
    print(table(results, notes))
    speedup = speedup_row(results)
    if speedup:
        print(f"\n`parallel for` speedup on {os.cpu_count()} CPUs: **{speedup:.2f}x** "
              f"(sequential / parallel).")
    print(f"\nBest of {args.reps} timed repetitions after {args.warmup} warmups, "
          f"whole-process wall time including start-up.")

    if args.json:
        Path(args.json).write_text(json.dumps(payload, indent=2) + "\n")
    if args.save_baseline:
        BASELINE.write_text(json.dumps(payload, indent=2) + "\n")
        print(f"\nWrote {BASELINE.relative_to(ROOT)}.")

    if args.compare:
        tolerance = args.tolerance if args.tolerance is not None else 1.5
        if not BASELINE.exists():
            raise SystemExit("no baseline yet: run with --save-baseline first")
        old = json.loads(BASELINE.read_text())
        if old["machine"]["machine"] != machine()["machine"]:
            print(f"\nNote: the baseline was recorded on {old['machine']['machine']}, "
                  f"this is {machine()['machine']}. Compare with suspicion.")
        merged = {name: {"baseline_ms": old["results"][name]["min_ms"],
                         "candidate_ms": r["min_ms"]}
                  for name, r in results.items() if name in old["results"]}
        rendered, regressions = comparison_table(merged, tolerance)
        print(f"\n## Against the committed baseline\n")
        print(rendered)
        if regressions:
            print("\nRegressions:")
            for name, before, after, ratio in regressions:
                print(f"  {name}: {before:.2f} ms -> {after:.2f} ms ({ratio:.2f}x)")
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

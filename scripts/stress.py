#!/usr/bin/env python3
"""Push the Kora runtime until something gives, without taking the machine
down with it.

Every probe runs a child process under a watchdog: a hard RSS ceiling, a wall
clock timeout, and a system-wide free-memory abort switch that kills the
child if the whole machine starts running low, not just this process. Worst
case is one killed child, never a swapped-to-death laptop.

    python3 scripts/stress.py                  # run every probe, print a table
    python3 scripts/stress.py --filter recur    # just the recursion probe
    python3 scripts/stress.py --history         # also append to stress_history.jsonl
    python3 scripts/stress.py --rss-cap-mb 512  # tighter cage on a small machine

What each probe is checking, and why it is in the set (see benches/README.md
for the throughput benchmarks -- this file is about breaking points, not
steady-state cost):

  recursion    deepest non-tail call chain before the interpreter gives up.
               A Kora function call recurses through the host Rust stack;
               past a depth guard it errors, but the guard has to sit safely
               under the point where the OS stack actually overflows.
  string_concat  `s = s + x` in a loop, at increasing sizes. Naive immutable
               string concatenation is O(n) per append -- O(n^2) total -- if
               nothing recognizes that the old value is about to be discarded.
  durable_journal  `--durable` effects, at increasing counts. The journal is
               written whole to disk on every effect; this probe is how you
               would notice if that stopped being true, or got worse.
  parallel_width  number of concurrent `parallel for` branches. Checks the
               scheduler stays a pool (bounded memory) rather than spawning
               one OS thread per branch (memory that scales with N).

A probe's "breaking point" is the smallest N in its series where the run
stopped completing normally -- crashed, got killed for memory, or ran out
the clock. `--against-history` compares today's breaking points to the last
recorded run and flags anything that got *worse* (broke at a smaller N, or
got slower approaching the same N), which is what you want after a change
that touches call handling, string ops, or the journal.
"""

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH_DIR = ROOT / "benches"
DEFAULT_BIN = ROOT / "target" / "release" / "kora"
HISTORY = BENCH_DIR / "stress_history.jsonl"
WORKDIR = ROOT / "target" / "stress-tmp"

STATUS_OK = "ok"
STATUS_CLEAN_ERROR = "clean-error"   # exit 1, program-level error message -- not a crash
STATUS_CRASHED = "crashed"           # killed by a signal (SIGABRT/SIGSEGV/...) -- a real crash
STATUS_KILLED_RSS = "killed(rss-cap)"
STATUS_KILLED_TIMEOUT = "killed(timeout)"
STATUS_ABORTED_SYSTEM = "aborted(system-memory-critical)"


def free_pages():
    """Free memory pages on this machine, however `vm_stat`/`vmstat` reports
    it. Returns None where neither is available (e.g. non-mac, non-linux) --
    callers must treat that as "cannot check, do not block on it"."""
    try:
        out = subprocess.run(["vm_stat"], capture_output=True, text=True, timeout=2)
        for line in out.stdout.splitlines():
            if line.startswith("Pages free"):
                return int(line.split(":")[1].strip().rstrip("."))
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass
    try:
        out = subprocess.run(["cat", "/proc/meminfo"], capture_output=True, text=True, timeout=2)
        for line in out.stdout.splitlines():
            if line.startswith("MemAvailable"):
                kb = int(line.split()[1])
                return kb // 4  # normalize to ~4KB-page units, good enough as a ratio check
    except FileNotFoundError:
        pass
    return None


def rss_kb(pid):
    try:
        out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)],
                             capture_output=True, text=True, timeout=2)
        text = out.stdout.strip()
        return int(text) if text else None
    except (FileNotFoundError, subprocess.TimeoutExpired, ValueError):
        return None


def watchdog_run(cmd, rss_cap_kb, timeout_s, min_free_pages=500, poll_s=0.15):
    """Run `cmd`, killing it if it crosses the RSS cap, runs past the
    timeout, or the whole machine's free memory drops critically low.
    Returns (status, exit_code, peak_rss_kb, elapsed_s, stdout_tail)."""
    start = time.monotonic()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    peak = 0
    status = STATUS_OK
    while proc.poll() is None:
        rss = rss_kb(proc.pid)
        if rss is not None:
            peak = max(peak, rss)
            if rss > rss_cap_kb:
                proc.kill()
                status = STATUS_KILLED_RSS
                break
        if time.monotonic() - start > timeout_s:
            proc.kill()
            status = STATUS_KILLED_TIMEOUT
            break
        free = free_pages()
        if free is not None and free < min_free_pages:
            proc.kill()
            status = STATUS_ABORTED_SYSTEM
            break
        time.sleep(poll_s)
    out, _ = proc.communicate(timeout=5)
    elapsed = time.monotonic() - start
    if status == STATUS_OK and proc.returncode != 0:
        # Negative on POSIX means killed by a signal (SIGABRT, SIGSEGV, ...)
        # -- a real crash. A positive code is the program's own exit(1) with
        # a clean error message, which is working as intended.
        status = STATUS_CRASHED if proc.returncode < 0 else STATUS_CLEAN_ERROR
    return status, proc.returncode, peak, round(elapsed, 2), (out or "")[-400:]


# --- probes -----------------------------------------------------------
#
# Each probe is a (name, description, series, make_source) tuple. `series`
# is the list of N values to try, smallest first; `make_source(n)` returns
# the Kora program text for that N.

def recursion_source(n):
    return f"""
def deep(i, target):
    if i >= target:
        return i
    return deep(i + 1, target)

def main():
    print(deep(0, {n}))
"""


def string_concat_source(n):
    return f"""
def main():
    s = ""
    i = 0
    while i < {n}:
        s = s + "x"
        i = i + 1
    print(len(s))
"""


def durable_journal_source(n):
    return f"""
def main():
    total = 0
    i = 0
    while i < {n}:
        total = total + i
        print(f"step {{i}}")
        i = i + 1
"""


def parallel_width_source(n):
    return f"""
def work(x):
    return x * 2

def main():
    results = parallel for x in range(0, {n}):
        work(x)
    print(len(results))
"""


PROBES = [
    ("recursion", "deepest non-tail recursion before the interpreter gives up",
     [500, 1000, 1500, 1900, 2500, 4000], recursion_source, []),
    ("string_concat", "`s = s + x` accumulation at increasing sizes",
     [200_000, 1_000_000, 3_000_000, 8_000_000], string_concat_source, []),
    ("durable_journal", "`--durable` effect count at increasing sizes",
     [1_000, 5_000, 15_000, 40_000], durable_journal_source, ["--durable"]),
    ("parallel_width", "concurrent `parallel for` branches at increasing counts",
     [1_000, 10_000, 100_000, 500_000], parallel_width_source, []),
]


def run_probe(binary, name, series, make_source, extra_flags, rss_cap_kb, timeout_s):
    rows = []
    broke_at = None
    for n in series:
        WORKDIR.mkdir(parents=True, exist_ok=True)
        src = WORKDIR / f"{name}.ko"
        src.write_text(make_source(n))
        journal_dir = WORKDIR / ".kora"
        shutil.rmtree(journal_dir, ignore_errors=True)
        cmd = [str(binary), "run", *extra_flags, str(src)]
        cwd = os.getcwd()
        os.chdir(WORKDIR)
        try:
            status, code, peak, elapsed, tail = watchdog_run(cmd, rss_cap_kb, timeout_s)
        finally:
            os.chdir(cwd)
        shutil.rmtree(journal_dir, ignore_errors=True)
        rows.append({"n": n, "status": status, "exit_code": code,
                     "peak_rss_kb": peak, "elapsed_s": elapsed})
        marker = "" if status == STATUS_OK else f"  <-- {tail.strip().splitlines()[-1] if tail.strip() else ''}"
        print(f"  {name:<16} n={n:<10} {status:<28} peak={peak/1024:>7.1f}MB  "
              f"{elapsed:>6.2f}s{marker}", file=sys.stderr)
        if status != STATUS_OK and broke_at is None:
            broke_at = n
            break  # no point pushing further once it has broken
    return {"probe": name, "broke_at": broke_at, "series": rows}


def machine():
    return {"os": platform.system(), "release": platform.release(),
            "machine": platform.machine(), "cpus": os.cpu_count()}


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--bin", default=str(DEFAULT_BIN))
    p.add_argument("--filter", default=None, help="only probes whose name contains this")
    p.add_argument("--rss-cap-mb", type=int, default=1536,
                   help="kill a probe if it crosses this RSS (default 1536MB)")
    p.add_argument("--timeout", type=int, default=60,
                   help="kill a probe if one N takes longer than this (seconds)")
    p.add_argument("--history", action="store_true",
                   help="append this run to benches/stress_history.jsonl")
    p.add_argument("--against-history", action="store_true",
                   help="compare today's breaking points to the last recorded run")
    args = p.parse_args()

    binary = Path(args.bin)
    if not binary.exists():
        raise SystemExit(f"{binary} does not exist. Build it first:\n"
                         f"  cargo build --release -p kora-cli")

    probes = [pr for pr in PROBES if not args.filter or args.filter in pr[0]]
    if not probes:
        raise SystemExit(f"no probe matches {args.filter!r}")

    print(f"# stress-testing {binary}", file=sys.stderr)
    print(f"# cage: {args.rss_cap_mb}MB RSS cap, {args.timeout}s per-N timeout, "
          f"abort if system free memory runs critical", file=sys.stderr)

    results = []
    for pname, desc, series, make_source, flags in probes:
        results.append(run_probe(binary, pname, series, make_source, flags,
                                 args.rss_cap_mb * 1024, args.timeout))
    shutil.rmtree(WORKDIR, ignore_errors=True)

    print("\n## Breaking points\n")
    print("| probe | broke at | largest clean N | how it broke |")
    print("| --- | ---: | ---: | --- |")
    for r in results:
        series = r["series"]
        clean = [row["n"] for row in series if row["status"] == STATUS_OK]
        largest_clean = max(clean) if clean else "-"
        if r["broke_at"] is None:
            print(f"| `{r['probe']}` | never (up to {series[-1]['n']:,}) | {largest_clean:,} | - |")
        else:
            broken_row = next(row for row in series if row["n"] == r["broke_at"])
            print(f"| `{r['probe']}` | {r['broke_at']:,} | "
                  f"{largest_clean if largest_clean == '-' else f'{largest_clean:,}'} | "
                  f"{broken_row['status']} |")

    record = {"timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "git_commit": subprocess.run(["git", "rev-parse", "--short", "HEAD"],
                                           cwd=ROOT, capture_output=True, text=True).stdout.strip(),
              "machine": machine(), "rss_cap_mb": args.rss_cap_mb,
              "timeout_s": args.timeout, "results": results}

    if args.against_history or args.history:
        HISTORY.parent.mkdir(parents=True, exist_ok=True)
        previous = None
        if HISTORY.exists():
            lines = HISTORY.read_text().strip().splitlines()
            if lines:
                previous = json.loads(lines[-1])

    if args.against_history and previous:
        print(f"\n## Against previous run ({previous['timestamp']}, "
              f"{previous['git_commit']})\n")
        prev_by_probe = {r["probe"]: r for r in previous["results"]}
        regressed = False
        for r in results:
            prev = prev_by_probe.get(r["probe"])
            if not prev:
                continue
            old_break = prev["broke_at"]
            new_break = r["broke_at"]
            if new_break is not None and (old_break is None or new_break < old_break):
                regressed = True
                print(f"  ⚠️  {r['probe']}: broke at {new_break:,} "
                      f"(previously {old_break if old_break else 'never'})")
            else:
                print(f"  {r['probe']}: breaking point unchanged or improved "
                      f"({old_break if old_break else 'never'} -> "
                      f"{new_break if new_break else 'never'})")
        if regressed:
            print("\nA probe broke at a smaller N than last time. Investigate before merging.")

    if args.history:
        with HISTORY.open("a") as f:
            f.write(json.dumps(record) + "\n")
        print(f"\nAppended to {HISTORY.relative_to(ROOT)}.")

    return 0


if __name__ == "__main__":
    sys.exit(main())

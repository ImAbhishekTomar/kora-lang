# Kora — Design Decisions

Frozen outcomes of the design brainstorm (2026-08-28). Changes to this file are
design changes and should be deliberate.

## Identity

- **Name:** Kora
- **File extension:** `.ko`
- **Config file:** `kora.toml`
- **CLI:** `kora`
- **Implementation language:** Rust, single-language monorepo

## Thesis

**The agent is the unit of execution.** Everything derives from this:

1. **Agents are isolated, durable, resumable processes.** Per-agent private
   heaps, share-nothing, message passing. Durability is **replay-based**, not
   stack-snapshot based: a tree-walking interpreter keeps its state in the
   Rust call stack, which cannot be serialized, so every effect (model call,
   tool result, human answer, output line) is journaled and a resumed run
   re-executes from the top with those effects served from the journal. This
   is Temporal's and Restate's approach, and it works here precisely because
   agents share nothing — each replays independently, with no interleaving to
   reproduce. The contract: code between effects must be deterministic.
   Output is journaled too, so a resumed run continues rather than retells.
2. **Real parallelism.** OS threads + work-stealing scheduler, no GIL, no
   async/await coloring. `parallel for` for fan-out. Safe because heaps are
   isolated.
3. **Information flow control.** `classified` values cannot reach a model
   without a scoped `declassify ... for <sink>:` block. Checked at compile
   time; sinks and policies declared in `kora.toml`. `kora audit` lists every
   declassify site.
4. **Budgets are native.** Token-denominated (`max_tokens`, `max_calls`,
   `max_steps`, `max_time`), lexically scoped, nested (child may tighten,
   never loosen), shared across `parallel for`. Exhaustion is a value
   (`Exhausted`), not an exception; partial work survives. Money is an
   optional display layer via `[budget.pricing]` in config — never enforcement.

## Language surface

- Python-like: indentation blocks, `def`, `if/elif/else`, `for x in xs`,
  f-strings, list/dict literals, comprehensions. Zero learning curve is a goal.
- Static types, checked. Type declarations on the left: `e: Expense = ...`.
- New words (whole list): `analyze`, `tool`, `agent`, `budget`, `parallel for`,
  `classified` / `declassify`, `ask_human`, `test`, `mock`.
- `analyze(data, "prompt")` returns a typed result via schema-constrained
  output. Result is `Ok(T)` / `Uncertain(reason)` / `Exhausted(meter)`,
  handled with `match`.
- No confidence float. Model must refuse explicitly (`Uncertain`).
- Fixes over Python: no GIL, no async coloring, mandatory-at-boundary types,
  no bare except, one packaging story, no mutable-default-arg footgun.

## Memory model

- Per-agent isolated heaps (Erlang-style). Whole-heap free on agent exit.
- Within-agent: small per-agent GC for v1 (swappable implementation detail).
- No user-facing memory syntax at all. No Rust-style ownership for users.
- Message passing: copy-only for v1; immutable shared buffers for big
  read-only data later if needed.

## Models

- Providers: OpenAI (API key) + Ollama (localhost HTTP) from Phase 2.
- `local_model` sink = Ollama. In-process GPU inference (llama.cpp/candle) is
  parked (Phase 7, optional — measure first).
- Model choice/config: call-site > block > agent > main > kora.toml.

## Security labels

- `classified` (confidentiality, transitive through all operations,
  field-level granularity). Public by default; IO boundaries declare labels.
- `declassify <expr> for <sink>:` scoped block only — no permanent
  declassified values. Sink-aware policy from config.
- Redaction (`redact()`) is the blessed easy path: placeholders out, real
  values re-substituted, nothing secret leaves.
- Integrity direction (`unverified` model output vs trusted sinks) is
  designed but parked; slot after Phase 4.
- Telemetry export is a labeled sink: classified values cannot reach spans.

## Testing & observability

- Record/replay cassettes native, from Phase 2. Replay is CI default;
  `(live)` tests opt-in. Cassette format: human-readable JSON on disk.
- `mock analyze -> ...` is a typed language construct, checked at compile time.
- Runtime is an OTel producer: agents/calls = spans (GenAI semantic
  conventions verbatim), budgets = metrics, declassify = events.
  Zero-config: local file + `kora trace last`.
- One internal event stream feeds cassettes, OTel, and `--report` cost output.
- LLM eval (DeepEval-style metrics: answer relevancy, faithfulness,
  hallucination, G-Eval judge) ships as **native stdlib primitives**, not a
  Python bridge to the real DeepEval lib. Full DeepEval too big to
  reimplement; a subset of core metrics as Rust-backed builtins fits Kora's
  no-Python-required packaging story and avoids the sidecar's per-call
  serialization cost for what is likely a hot path in `test`. Same rationale
  as native stdlib layer in Ecosystem strategy below. Sequencing: alongside
  `test`/`mock` work, Phase 6.

## Execution strategy

- Stage 1: tree-walking interpreter. Stage 2: bytecode VM. Stage 3 (maybe
  never): cranelift JIT. Native codegen is NOT on the critical path.

## Ecosystem strategy

An agent program's real imports are HTTP, JSON, CSV/Excel, SQL, dates, regex,
file IO, and vendor SDKs that are themselves HTTP wrappers. The reasoning
lives in the model, not in a library. That is ~20 libraries, not 500,000, so
Kora does not need to replicate PyPI. Three layers, in this order:

### What the stdlib fixes

Rewriting these libraries is only worth it if the rewrite fixes what everyone
already knows is broken. Existing ecosystems cannot fix these because too much
code depends on the current behaviour; a new language has exactly one chance.

Cross-cutting, enforced by the language rather than by discipline:

- **Every value that enters the program from outside is `unverified`.** HTTP
  bodies, file contents, model output, tool results. An unverified value
  cannot reach a dangerous sink (SQL, shell, file path, HTTP URL) until it is
  narrowed by a parse or an allowlist. This is the integrity direction of the
  label system, and it makes SQL injection, SSRF, and path traversal *type
  errors* rather than review findings.
- **Nondeterminism goes through the journal.** `time.now()` and `random()` are
  effects. Otherwise a durable replay silently produces different answers —
  a correctness requirement, not a nicety.
- **Failure is a value.** No silent `None`, no exception that a caller forgets
  to catch. Same `Ok` / `Err` shape as `analyze`.
- **Timeouts and retries are mandatory with defaults**, never optional extras.
- **Classified data never reaches logs or telemetry.**

Per module, the specific defect being fixed:

| module | what everyone gets wrong | what Kora does |
|---|---|---|
| `http` | no default timeout (hangs forever); non-2xx looks like success until you call `raise_for_status()`; retries are a separate library | timeout always set; non-2xx is `Err`, not a success object; retry with backoff built in; response body is `unverified` |
| `json` | parses to untyped `Any`; errors say "line 1 col 4318" | parses into a declared type; errors name the path: `$.users[2].email: expected str, got int` |
| `csv` | everything is a string, or types are guessed and zip codes lose leading zeros; ragged rows pass silently | declared schema, no guessing; ragged or mistyped rows are errors naming row and column; BOM handled |
| `sql` | string interpolation, therefore injection | parameters only; an `unverified` value cannot become query text at all |
| `fs` | path traversal from untrusted input; silent overwrite; partial writes on crash | paths from unverified data are refused; writes are atomic (temp + rename); overwrite is explicit |
| `time` | naive datetimes with no zone, then DST arithmetic bugs | every instant is zone-aware; there is no naive type; `now()` is journaled |
| `re` | catastrophic backtracking (ReDoS) | linear-time engine, no backtracking to exploit |

**1. Native stdlib, Rust-backed.** Users cannot write `use serde::Deserialize`
in a `.ko` file, but the interpreter is Rust, so a crate becomes a Kora module
through a thin binding — the way Python's `json` is C underneath. Planned
modules and their backing crates:

| module | crate | note |
|---|---|---|
| `http` | `reqwest` / `ureq` | equal to `requests` |
| `json` | `serde_json` | faster |
| `csv`, `excel` | `csv`, `calamine` | equal |
| `data` | `polars` | better than pandas |
| `sql` | `sqlx`, `rusqlite` | equal |
| `time` | `chrono` / `jiff` | better |
| `re` | `regex` | no catastrophic backtracking |
| `s3`, `aws` | `aws-sdk-rust` | official |
| `pdf` | `lopdf`, `pdf-extract` | weaker than PyPDF |
| `search` | `tantivy` | Lucene-class |

Known gaps: scipy, sklearn, matplotlib, torch/transformers, and niche SaaS
SDKs. Only the last matters for agents, and layer 2 covers it.

**2. MCP integration — borrow an existing ecosystem.** MCP is already the
standard for "tools an agent can call," with hundreds of maintained servers
(GitHub, Slack, Postgres, Sentry, Linear, filesystem, Stripe...). Kora already
has `tool` as a first-class construct, and an MCP server is a bag of tools, so
the mapping is mechanical — one implementation, whole ecosystem inherited:

```python
use mcp "github" as gh
r: Report = analyze(issue, "triage this", tools=gh.tools)
```

MCP servers are separate processes, so each is a labeled sink:
`declassify x for github` is checkable exactly like a model sink. Highest
leverage per line of code in the project.

**3. Python via sidecar worker — the long-tail escape hatch.** A separate
Python process, data in / data out over IPC. No live object handles, no
Python callbacks into Kora.

```python
use python "pandas" as pd
df = pd.read_csv("sales.csv")
summary = pd.to_dict(pd.describe(df))
```

Chosen over embedded CPython (PyO3) because embedding would break three of the
four thesis pillars:
- **Threading**: embedding reintroduces the GIL into Kora. A sidecar keeps
  Kora GIL-free, and N workers give real parallelism that embedding cannot.
- **Durability**: a CPython call stack cannot be checkpointed. As an RPC, a
  Python call is atomic — checkpoint before and after, never mid-frame.
- **Labels**: an explicit boundary is a declassification site the compiler
  sees; embedded objects would be opaque and labels would vanish.
- **Packaging**: no `use python` means no Python needed; the binary stays a
  single download.

Cost accepted: per-call serialization, and no live-object interop
(`df.groupby().apply(lambda ...)`). Negligible next to real work per call.

**Kora's own packages** come later: `kora.toml` dependencies, a lockfile,
`kora add`. One tool, lockfile by default, no global installs, reproducible.
A new registry is a ghost town for years, so layers 1 and 2 carry the weight.
For third-party *native* packages, the destination is WASM components rather
than dynamic libraries: sandboxed by construction, language-agnostic, and a
sandboxed package cannot exfiltrate classified data. Immature today.

Sequencing: native stdlib after Phase 5, MCP around Phase 6, Python sidecar
when something actually demands it (designed, not built speculatively).

## Parked / non-goals

- Auto-parallelization (explicit `parallel for` only)
- Embedded CPython (PyO3). Python support ships as a sidecar worker instead
  — see Ecosystem strategy.
- GPU tensor compiler (that is Mojo's war, not ours)
- Native/JIT compilation, semantic-assert judging, label lattice beyond
  binary, `unverified` labels (designed, waiting)
- Public release: personal-use first; polish/marketing gloss lowest priority

## Phases

0. Freeze + skeleton (`kora --version`) — **done**
1. Core Python-like language + types + good errors + VS Code basics
   (highlighting, icon, run command) — **done**
2. `analyze` (OpenAI + Ollama), typed results, cassettes — **done**
3. `agent`, `tool`, `parallel for`, budgets — **done**
4. `classified` / `declassify` + `kora audit` — **done**
5. Durability (journal/replay, `ask_human`) — **done**
6. `test`/`mock`, OTel, LSP (squiggles, hover, go-to-def)
7. (parked) in-process GPU inference

Ecosystem work, sequenced alongside the phases above:
- Native stdlib: `json`, `fs`, `time`, `re`, `http`, `csv`, `sql`, `env` —
  **done**
- MCP integration (`use mcp "..."`) — around Phase 6
- Python sidecar (`use python "..."`) — on demand
- Kora package manager + WASM components — later

Each phase ends with a runnable demo program. Demo programs live in
`examples/`.

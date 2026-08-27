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
   heaps, share-nothing, message passing. Checkpoint at every suspension point
   (model call, tool call, `ask_human`). Kill the process, restart, it resumes.
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

## Execution strategy

- Stage 1: tree-walking interpreter. Stage 2: bytecode VM. Stage 3 (maybe
  never): cranelift JIT. Native codegen is NOT on the critical path.

## Parked / non-goals

- Auto-parallelization (explicit `parallel for` only)
- Python interop (all forms — decided against ecosystems A/B; own stdlib)
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
4. `classified` / `declassify` + `kora audit`
5. Durability (checkpoint/resume, `ask_human`)
6. `test`/`mock`, OTel, LSP (squiggles, hover, go-to-def)
7. (parked) in-process GPU inference

Each phase ends with a runnable demo program. Demo programs live in
`examples/`.

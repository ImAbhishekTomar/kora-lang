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
  `classified` / `declassify`, `ask_human`, `test`, `mock`. `use` covers
  stdlib modules, Python, MCP servers, and other `.ko` files. Guards and the
  `else` binding reuse `if` and `else` rather than adding a word.
- `analyze(data, "prompt")` returns a typed result via schema-constrained
  output. Result is `Ok(T)` / `Uncertain(reason)` / `Exhausted(meter)` /
  `Failed(reason)`, handled with `match`.
- No confidence float. Model must refuse explicitly (`Uncertain`).
- **A provider that does not answer is `Failed(reason)`, not a crash.** A
  refused connection, a timeout, a 429, a 5xx: the most common failure in a
  real agent program used to be the one failure the language had no value for,
  which made "failure is a value" true of everything except the part that
  fails most. The runtime decides nothing about what it is worth; the program
  does.
  - **`Failed` is not `Uncertain`.** One is the model answering no, the other
    is nobody answering. They are read by different people and fixed in
    different places -- a prompt versus a provider -- and a program that
    retries an `Uncertain` differently from a `Failed` is the normal case, not
    an exotic one. Collapsing them would hide an outage inside a refusal.
  - **It is a breaking change, and deliberately not softened.** Every existing
    three-arm `match` over a model call becomes non-exhaustive the first time a
    provider blinks. Mapping `Failed` onto `Uncertain` would have avoided that
    by lying. The unmatched-value error names the arm to add, and `else` was
    added first so the migration is one line rather than a fourth branch
    everywhere.
  - **Journaled, never recorded to a cassette.** A durable run that took the
    failure branch must take it again on resume, or the replay diverges from
    the history it is replaying. A cassette is the opposite: it is a fixture,
    and freezing one afternoon's outage into it would fail every later run for
    a reason that no longer exists.
  - **Tokens already spent stay spent.** A tool loop that completed four turns
    and then lost the provider charged those turns as they happened. A budget
    that forgave them would let a failing call retry itself into a real bill.
- **Model calls retry, with backoff, and there is no way to mean "never".**
  `http` already retries a GET; the model transport was the one place the rule
  "timeouts and retries are mandatory with defaults" was not applied, and a
  hosted provider at its rate limit is the ordinary case rather than the
  exceptional one. `[models] max_retries` tunes it and `0` disables it -- which
  is honest for a local model on the same machine, where a refused connection
  means the server is not running and waiting will not start it.
  - Retried: refused connections, timeouts, 408, 409, 429, and every 5xx.
    Not retried: the rest of the 4xx range, which is the request being wrong.
    The classification is made where the failure is observed, not recovered
    from the error message afterwards.
  - A provider's `Retry-After` beats any locally computed backoff, capped so
    that a provider asking for an hour does not hold a thread for one.
  - The backoff is jittered, because `parallel for` fans out across every core
    and a shared rate limit hits every branch at once. Unjittered, they all
    march back into the provider together.
  - Retrying wraps one HTTP request, not the tool loop around it: a call that
    has already run three turns does not start over because the fourth request
    was rate limited.
- **A `parallel for` branch that raises still fails the loop.** An expected
  failure is an outcome and comes back as a value in its own slot; a raise is a
  bug, and a bug in one branch is a bug. What changed is that the error now
  says how many branches had already finished, because "this is broken" and
  "this is broken on one input in two hundred" are different problems.
- **`case` arms take a guard: `case Ok(a) if a.risk == "low":`.** A guard is
  evaluated with the pattern's binders in scope, and a false one falls through
  to the next arm. Binders stay bound on a rejected arm, because Kora scopes by
  function like Python and an arm that had its own scope would be the single
  exception in the language.
- **A guard cannot call a model.** Arms are tried in order, so a guard may run
  for an arm that never executes, and a model call there would make the token
  cost of a `match` depend on arm order — invisible spending is what budgets
  exist to prevent. The checker rejects a literal `analyze` in a guard; the
  runtime refuses one reached through a helper, because whether an arbitrary
  `def` eventually calls a model is not decidable at check time. Checker for
  the good message, runtime for the guarantee.
- **"Every matching arm was guarded off" is its own error**, distinct from "no
  arm matched". They have different fixes, and sending the reader to the wrong
  half of the `match` costs more than the branch that tells them apart.
- **`x: T = <outcome> else:` binds the payload or leaves.** Every model call
  returns three ways, so chaining them with `match` costs one indentation level
  per call and buries the path that matters. The `else` form keeps the
  successful path at its own level and makes failure the exception.
  - The block **must** diverge (`return` / `break` / `continue`), checked, so
    the bound name is always defined afterwards. A name that might not exist is
    worse than a required keyword.
  - The right-hand side must be an outcome. Treating a plain value as success
    would mean the failure path could never run, which is a silently dead
    branch — the thing this language most wants not to have.
  - `else (why, kind):` preserves the lower-case outcome tag for the flat
    "record and leave" case. It avoids forcing four identical `match` arms
    merely to write a trace, without making `else` a disguised branch table.
    When categories need different behavior, `match` is still there.
- **Pattern alternatives bind the same names or do not parse.** `case
  Uncertain(reason) | Failed(reason):` captures the ordinary shared-recovery
  branch without hiding which names a body may read. Alternatives with
  different binders are rejected instead of making a name conditionally exist.
- **`stream` is the standard-output shortcut, not a second result type.**
  `answer: str = analyze(...) stream` means "show the prose as it arrives"
  while preserving the same outcome that an ordinary call returns. A custom
  `on token(piece):` handler remains for work beyond printing; a loop over
  pieces still cannot replace matching the terminal outcome.
- **Destructuring reads through a label and re-applies it.** `match` on a
  classified outcome used to skip every `Ok(...)` arm, so a classified value
  silently took a different branch than the same value unclassified. Reading
  the structure through the wrapper fixes that; re-applying the label to every
  binding is what keeps the fix from becoming a laundering hole.
- **A mock is part of the test, so it crosses a `parallel for` boundary.**
  Workers are fresh interpreters seeded with what they need, and the mock stack
  was the one thing not seeded — so a test that fanned out silently reached for
  a real model. The fan-out is the path most worth testing; leaving it the one
  path that could not be tested was a hole, not a design.
- Fixes over Python: no GIL, no async coloring, mandatory-at-boundary types,
  no bare except, one packaging story, no mutable-default-arg footgun.

### File modules

- A program may span several `.ko` files: `use "./lib/tax.ko" as tax`.
- A **quoted path** is a file; a **bare word** is a stdlib module. Two
  syntaxes rather than one namespace, so an import can never be ambiguous and
  adding a stdlib module can never shadow somebody's file.
- Paths resolve against the **importing file**, never the working directory.
  A program is a directory, and it moves or vendors whole. A package is
  named rather than pathed (see Ecosystem strategy); paths need no
  infrastructure to work, so they came first.
- `as <name>` is **required**. A path has no natural bare name, and inventing
  one from the file stem would bind a name the source never mentions.
- **Everything top-level is exported.** No `export` keyword yet: adding
  privacy later only removes names, which is a change a checker can report,
  whereas guessing a privacy rule now would be one we could not take back.
- **Each file reads its own top level.** A function resolves free names in
  the file it was written in, so importing a module cannot change what its
  code means. This is why functions carry their home module at runtime.
- **Types are global across the files of one package.** A `Money` is one type
  everywhere inside it, so values cross file boundaries without conversion;
  declaring the same name differently in two of its files is an error, not
  two types. The sharing stops at the package boundary: two dependencies may
  each declare `Config` and they are different types, because the alternative
  is a hard error the consumer cannot fix, owning neither package. A type is
  identified internally by its package and name, and displayed by its name
  alone — except where two short names collide, which is the one case a
  reader cannot otherwise resolve.
- **A file's top level runs once per run.** Imports are cached by canonical
  path, so a diamond is one module, not two with separate state.
- **Cycles are an error**, reported with the chain. A half-initialized module
  is the failure mode Python accepted; we would rather refuse it.
- Budgets, labels, the journal, and `kora audit` all cross file boundaries
  unchanged. A module boundary is an organizing device, not a security one.

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
- Model choice/config: call-site > block > agent > main > kora.toml. The
  call-site level is `analyze(..., model="vision")`, and it takes a *name*
  from `[models]`, never a provider spec — a vendor's model name in a source
  file is how a program ends up routing on an environment variable.
- **Images are values, not an integration.** `fs.image` loads one, `analyze`
  takes it, and it crosses a `parallel for` boundary like anything else. An
  agent-first language that can only read text cannot look at a receipt, a
  screenshot, or a scanned form, which is most of the work people actually
  hand a model. Kept deliberately narrow: raster images only (PNG, JPEG, GIF,
  WebP), no audio, no video, and no PDF — a document needs page extraction,
  which is a different problem wearing the same coat.
- The pixels ride beside the JSON, not inside it. The data argument keeps an
  `<image>` marker in each image's place, so a cassette key does not move when
  a file is renamed, and a base64 blob never reaches a log line.
- A model call's timeout is `[models] timeout_secs`, not a constant: a local
  vision model reading a 900px receipt runs minutes past what a text call
  needs, and a timeout that fires on ordinary work teaches people to disable
  it.

## Security labels

- `classified` (confidentiality, transitive through all operations,
  field-level granularity). Public by default; IO boundaries declare labels.
- `declassify <expr> for <sink>:` scoped block only — no permanent
  declassified values. Sink-aware policy from config.
- Redaction (`redact()`) is the blessed easy path: placeholders out, real
  values re-substituted, nothing secret leaves.
- **Terminal output is a redacting boundary.** `print` and `write` render
  public structure but replace classified leaves with `__CLASSIFIED__` by
  default, configurable per project under `[output]`. A `declassify` release
  names a sink such as `local_model`; it never doubles as permission to copy a
  secret into a terminal, test log, debugger console, or redirected output.
- Integrity direction: data entering from outside is `unverified` and cannot
  reach a dangerous sink until narrowed. Shipped with the stdlib, since that
  is where the sinks are.
- A release names one sink. `declassify x for local_model:` does not make x
  plain inside the block — it records what x was approved for, so a secret
  released to a model still cannot be written to a file three lines later.
- Telemetry export is a labeled sink: classified values cannot reach spans.

## Testing & observability

- Record/replay cassettes native, from Phase 2. Replay is CI default;
  `(live)` tests opt-in. Cassette format: human-readable JSON on disk.
- `mock analyze -> ...` is a typed language construct, checked at compile time.
- Runtime is an OTel producer: agents and model calls are spans following the
  GenAI semantic conventions, declassifications are spans of their own, and
  budget spend rides on the agent span. Zero-config: a local file plus
  `kora trace`. OTLP JSON is emitted directly rather than through the SDK,
  which keeps an async runtime out of a synchronous interpreter.
  A metrics pipeline (as opposed to span attributes) is not built yet.
- One internal event stream feeds cassettes, OTel, and `--report` cost output.
- LLM eval (DeepEval-style metrics: answer relevancy, faithfulness,
  hallucination, G-Eval judge) ships as **native stdlib primitives**, not a
  Python bridge to the real DeepEval lib. Full DeepEval too big to
  reimplement; a subset of core metrics as Rust-backed builtins fits Kora's
  no-Python-required packaging story and avoids the sidecar's per-call
  serialization cost for what is likely a hot path in `test`. Same rationale
  as native stdlib layer in Ecosystem strategy below. Sequencing: alongside
  `test`/`mock` work, Phase 6.

### Debugging

- Debugging is a **first-class tool, not a print statement**: `kora dap` is a
  Debug Adapter Protocol server, so every DAP-capable editor gets breakpoints,
  stepping, a call stack, and a variables pane from one implementation.
- The interpreter knows nothing about the protocol. It keeps a frame stack and
  asks a `Debugger` trait whether to stop; the translation lives in
  `kora-dap`. With none attached the cost is one `Option` check per statement.
- **A paused program is inspected from a snapshot, never from the live
  interpreter.** When execution stops it pushes a complete copy of the stack
  and blocks; the adapter answers the editor out of that copy. Nothing reaches
  into a running interpreter, so inspecting a program cannot perturb it and
  there is no lock to get wrong.
- Each frame snapshots its names *before every statement it runs*, so a frame
  that has called into another shows its names as they stood at the call. This
  is what makes an outer frame inspectable without unsafe pointers into a live
  stack.
- **Watch expressions are lookups, not evaluation**: names and field paths
  only. A watch that could call a model, spend budget, or write a file is not
  inspecting the program, it is changing it — and a debugger that changes what
  it observes is worse than none.
- A breakpoint on a blank line moves forward to the next statement, and the
  editor is told where it landed. A breakpoint that silently never fires is
  the worst outcome available.
- `parallel for` bodies are **not** debuggable: branches are separate agents on
  worker threads with their own interpreters. Stopping one would mean stopping
  a thread the user cannot see, and the DAP thread model would have to grow to
  match. Deferred rather than faked.

## Execution strategy

- Stage 1: tree-walking interpreter. Stage 2: bytecode VM. Stage 3 (maybe
  never): cranelift JIT. Native codegen is NOT on the critical path.
- **Every stage is measured before it is left.** `benches/` holds twelve
  programs and `scripts/bench.py` runs them; CI A/Bs a pull request against
  its base commit on one machine and fails above 1.25x. The set exists so the
  cost of the current stage is a published number rather than an opinion, and
  so a feature that quietly taxes the hot path is caught by the branch that
  added it.
- The pairs in that set (`sequential`/`parallel`, `durable_off`/`durable_on`)
  are deliberate: the price of a guarantee is only legible next to a run
  without it.

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
| `fs` | path traversal from untrusted input; silent overwrite; partial writes on crash; `listdir`/`glob` return filesystem order, which differs per machine; `mimetypes` trusts the extension | paths from unverified data are refused; writes are atomic (temp + rename); overwrite is explicit; listings are sorted and return full paths; `fs.image` reads the type from the magic bytes |
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
| `pdf` | `lopdf`, `pdf-extract` | weaker than PyPDF; would make documents values the way images already are |
| `search` | `tantivy` | Lucene-class |

Known gaps: scipy, sklearn, matplotlib, torch/transformers, and niche SaaS
SDKs. Only the last matters for agents, and layer 2 covers it.

**2. MCP integration — borrow an existing ecosystem.** MCP is already the
standard for "tools an agent can call," with hundreds of maintained servers
(GitHub, Slack, Postgres, Sentry, Linear, filesystem, Stripe...). Kora already
has `tool` as a first-class construct, and an MCP server is a bag of tools, so
the mapping is mechanical — one implementation, whole ecosystem inherited:

```python
use mcp github as gh
r: Report = analyze(issue, "triage this", tools=gh.tools)
```

MCP servers are separate processes, so each is a labeled sink:
`declassify x for github` is checkable exactly like a model sink. Highest
leverage per line of code in the project.

**A tool server that does not answer is `Failed`, and is never called twice.**
Three decisions, all forced by the same fact: a tool call is a side effect
someone else runs.

*It has a deadline.* Every request carries `[mcp] timeout_secs`, default 60,
overridable per server for one that reaches a slow API. A blocking read on a
pipe cannot be given a deadline portably, so the transport reads on its own
thread and waits with one — without which a server that accepts a request and
never answers stops the program forever. That is the worst failure of the
three, because there is no error, no exit, and nothing to match on.

*It ends the `analyze` call as `Failed(reason)`, not as a crash.* The same
outcome a provider that does not answer produces, for the same reason: the
program, not the runtime, decides what an unreachable dependency is worth. It
is deliberately *not* handed back to the model as a tool result. A model
offered "that tool failed" will reasonably try it again, paying the timeout
every remaining turn until the budget is gone — and then report `Exhausted`,
which names the wrong cause and sends whoever reads it to the wrong fix. A
tool that *runs* and reports its own failure is different: that is an answer,
the model can act on it, and it still comes back as a tool result.

*It is never retried.* This is where MCP parts company with the model
transport, which retries freely. Generating twice costs tokens and nothing
else; calling a tool twice may open two issues, send two messages, or charge a
card twice — and a timeout is precisely the case where whether it ran is
unknown. MCP does carry `readOnlyHint` and `idempotentHint`, but those are
claims made by the server, which sits outside the trust boundary; believing
one in order to run a side effect a second time is not a trade worth making.
Starting a server and shaking hands *is* retried, because nothing has run yet.
So the timeout is the whole protection for a tool call, which is why it is not
optional and why zero is clamped rather than honoured.

One consequence worth stating: after a timeout the server may still answer.
That stale reply is matched by request id and skipped, so it cannot be read as
the answer to whatever the program asked next — a wrong answer being worse
than an error.

**3. Python via sidecar worker — the long-tail escape hatch.** A separate
Python process, data in / data out over IPC. No live object handles, no
Python callbacks into Kora.

```python
use python statistics as stats
match stats.mean(readings):
    case Ok(m):
        print(m)
    case Err(why):
        print(why)
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

**Kora's own packages** are being built, starting with local path
dependencies. One tool, no global installs, reproducible. A new registry is a
ghost town for years, so layers 1 and 2 carry the weight meanwhile, and
fetching is deliberately the *last* piece rather than the first.

Four decisions distinguish it from what everyone else ships:

- **The graph is derived from the source, not the manifest.** `[dependencies]`
  says where a package comes from; the `use pkg` statements say whether it is
  needed. Declaring a hundred and importing four resolves four, transitively.
  This is exact rather than heuristic because a package name is always a
  literal token and Kora has no dynamic import — the property that makes
  `depcheck` and its equivalents guess elsewhere. A typo'd dependency is
  therefore never fetched at all, which is where dependency-confusion attacks
  start.
- **There is no `[dev-dependencies]` table.** `test` is a language construct,
  so test-only reachability is computed rather than declared, and there is no
  wrong half to put something in. Runtime and test reachability come from
  running the same walk twice rather than propagating a label down the graph:
  a package reached by both a test path and a runtime path is a runtime
  dependency, and propagating "dev" would drop a shared transitive dependency
  from a shipped program. A dependency's own tests are never roots for its
  consumer.
- **Names resolve against the manifest that wrote them**, never a global
  table — the same rule as file paths resolving against the importing file.
  Two packages may bind one bare name to different sources, and a program
  cannot reach its dependencies' dependencies.
- **Confinement lands before fetching.** Per-package capability grants come
  before git dependencies, so no package is ever published into an
  unconfined ecosystem. npm's ordering — fetch first, security retrofitted —
  is the mistake being avoided, and it is not recoverable afterwards.
  Grants are checked where the effect happens, and confinement follows
  *execution* rather than the call site: a package cannot shed it by
  spawning, by being reached through a `tool` a model called, or by handing
  the work to a dependency of its own. A parent may only pass on what it
  holds, so compromising a leaf gains an attacker nothing that every link
  above it lacked. One package granted two different ways by two importers is
  refused: the union would let a permissive importer widen what a careful one
  withheld, and the intersection would break the permissive one's code.

The lockfile is **authoritative**: once a repository is locked, its commit is
what gets fetched, never the tag again. Re-resolving the reference is how a
force-pushed tag lands on a machine with a cold cache — the lockfile would be
rewritten to the attacker's commit and nothing would look wrong. That case is
a test, because it is the whole reason the file exists.

An append-only checksum log closes the window the lockfile cannot: a
project's *first* fetch has nothing to check against, so a backdoor published
briefly and withdrawn leaves no trace in any lockfile. The first sighting of a
commit fixes what it contains, and a later disagreement is refused rather than
resolved. Two logs are consulted — the project's committed `kora.sums`, and a
machine-level one shared across every project on the computer, so an honest
fetch in one project protects the next. It is deliberately not a hosted
transparency log: that needs a service somebody runs, and this narrows the
window without one.

A manifest has **no field for install scripts**, and will not gain one. That
is the whole of the `postinstall` attack class, refused by the file format
rather than by a setting someone can turn off.

`kora update` is the only sanctioned way past the lockfile, and so the place a
new version's *authority* is examined: it refuses a bump that asks for
capabilities the old version did not, or that declassifies in more places,
until someone says they have looked. Advisory rather than load-bearing — the
runtime still refuses to grant what kora.toml did not — because two
independent gates beat one.

For third-party *native* packages, the destination is WASM components rather
than dynamic libraries: sandboxed by construction, language-agnostic, and a
sandboxed package cannot exfiltrate classified data. A component declares its
capabilities, which makes it a named sink like any other, so `declassify x
for pkg:weather` checks the same way MCP servers already do. Immature today.

Layers 1 through 3 are built. Packages are under way; WASM components wait.

## Deferred, and what would start them

Neither of these is a hole in what exists. Both *extend* the package system,
and each is waiting on something that is not code.

### A hosted checksum log

**What is built.** `kora.sums` records what a commit contained the first time
it was seen, in two places: the project's own file, committed and shared with
everyone who clones, and a machine-level one under `~/.kora` shared across
every project on that computer. A later fetch that disagrees is refused.

**The window it does not close.** If nobody in your world has ever fetched a
package, and the attacker's version is live the first time anyone does, that
version becomes the record. The log is protecting the wrong bytes and there
was never anything to compare against.

**What a hosted log adds.** One log everyone reports to, so the first fetch
*by anyone* fixes what a version means for everyone after. Being the second
person on Earth to fetch a package is then enough. This is what
`sum.golang.org` does for Go.

**Why it is deferred.** It is a server: somebody runs it, pays for it, keeps
it up, and every Kora user has to trust it. That is an operational and trust
commitment, not a coding decision, and committing code cannot make it.

**What would start it.** Packages being fetched by people who did not write
them — that is, a real third-party ecosystem rather than a handful of
first-party ones. Until then the two local logs cover everyone who exists.

### WASM components for native packages

**The gap.** A package is `.ko` source. Someone who wants to ship a fast PDF
parser or an image codec, written in Rust, cannot: there is no way to include
compiled code.

**The answer that must never be taken.** Loading a native shared library.
A `.so` runs inside the process with full operating-system rights — it can
read `~/.ssh`, open sockets, and write anywhere, and none of it passes through
`call_module_fn`, so no grant check ever sees it. One native package and the
capability system is gone. This is a permanent refusal, not a deferral.

**Why WASM instead.** A component runs in a sandbox the runtime controls and
can only touch what it is handed. A WASM package that declares `net` gets the
network; one that does not, physically cannot reach it — the same rule `.ko`
packages already follow, enforced by the sandbox rather than by our checks.
Being language-agnostic is a bonus; being unable to escape is the reason.

**Why it is deferred.** Nothing needs it yet. MCP covers tools, the Python
sidecar covers the long tail, and the component tooling is still young.

**What would start it.** A package that genuinely cannot be written in Kora or
reached through MCP or Python — a codec, a parser, something CPU-bound enough
that the sidecar's per-call serialization dominates.

## Parked / non-goals

- Auto-parallelization (explicit `parallel for` only)
- Embedded CPython (PyO3). Python support ships as a sidecar worker instead
  — see Ecosystem strategy.
- GPU tensor compiler (that is Mojo's war, not ours)
- Native/JIT compilation, semantic-assert judging, label lattice beyond
  binary, `unverified` labels (designed, waiting)
- Public release: personal-use first; polish/marketing gloss lowest priority

## Status

Phases 0 through 6 are complete, as are the standard library, MCP
integration, the Python sidecar, and images as values. Packages have begun
with path and git dependencies, capability grants, and a content-hashed
lockfile, a checksum log, and the packaging commands; what remains is a
hosted checksum log and WASM components — see the ecosystem strategy above.

Reference documentation lives in [docs/](docs): the
[language](docs/language.md), the [standard library](docs/stdlib.md), and the
[CLI](docs/cli.md). This file records *why*; those record *what*.

## Phases

0. Freeze + skeleton (`kora --version`) — **done**
1. Core Python-like language + types + good errors + VS Code basics
   (highlighting, icon, run command) — **done**
2. `analyze` (OpenAI + Ollama), typed results, cassettes — **done**
3. `agent`, `tool`, `parallel for`, budgets — **done**
4. `classified` / `declassify` + `kora audit` — **done**
5. Durability (journal/replay, `ask_human`) — **done**
6. `test`/`mock`, LSP (squiggles, hover, go-to-def, outline, completion),
   and OpenTelemetry tracing — **done**
7. (parked) in-process GPU inference

Ecosystem work, sequenced alongside the phases above:
- Native stdlib: `json`, `fs`, `time`, `re`, `http`, `csv`, `sql`, `env` —
  **done**
- MCP integration (`use mcp <server> as <alias>`) — **done**
- Python sidecar (`use python <module> as <alias>`) — **done**
- Images as values (`fs.image`, multimodal `analyze`, `fs.glob`) — **done**
- Package dependencies (`use pkg`, path dependencies, reachability-derived
  graph) — **done**
- Per-package type namespacing — **done**
- Per-package capability grants — **done**
- Git dependencies, content-hashed lockfile, parallel fetch — **done**
- Append-only checksum log — **done**
- `kora add` / `remove` / `update` / `vendor`, `kora audit --deps` — **done**
- A hosted checksum log, and WASM components — deferred; see
  [Deferred, and what would start them](#deferred-and-what-would-start-them)

Each phase ends with a runnable demo program. Demo programs live in
`examples/`.

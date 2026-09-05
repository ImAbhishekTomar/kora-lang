# TODO

Where development stands. Updated as work moves, per the development
principle in [AGENTS.md](AGENTS.md).

## Current

- [x] **Low-noise syntax highlighting.** Added Kora light and dark
      VS Code themes and aligned the grammar and public code preview with
      deliberate colors for literals, constants, comments, and definitions.

## Completed

### Packages (shipped, merged to main)

The problem other ecosystems cannot fix, and what Kora does instead:

- [x] **Declared is not downloaded.** `[dependencies]` says where a package
      comes from; the `use pkg` statements say whether it is needed. Exact
      rather than heuristic, because Kora has no dynamic import — so a typo'd
      dependency name never reaches the disk, which is where
      dependency-confusion attacks begin. npm and pip cannot do this: their
      imports are dynamic, so tools like `depcheck` are forced to guess.
- [x] **No `[dev-dependencies]` table.** `test` is a language construct, so
      test-only reachability is computed rather than declared. There is no
      wrong half to put something in. A package reached by both a test path
      and a runtime path is a runtime dependency.
- [x] **Per-package type namespaces.** Two dependencies may each declare
      `Config`. A flat type table would be an error the consumer cannot fix,
      owning neither package.
- [x] **Capability grants.** A dependency has no ambient authority.
      Confinement follows execution, so it cannot be shed by spawning, by
      being called through a tool, or by delegating to a sub-dependency. A
      parent passes on only what it holds.
- [x] **No install-script field.** Not off by default: the manifest format
      has nowhere to put one. That is the whole `postinstall` attack class,
      refused by the file format.
- [x] **Authoritative lockfile.** Once a repository is locked, its commit is
      fetched, never the tag again — so a force-pushed tag changes nothing,
      including on a cold cache. Verified on every run, not only at install.
- [x] **Append-only checksum log.** Project-level and machine-level, so an
      honest fetch in one project protects the next.
- [x] `kora add` / `remove` / `update` / `install` / `vendor` / `tree` /
      `audit --deps`. `update` refuses a version that asks for more authority
      or declassifies in more places, until someone says they have looked.
- [x] Docs, site pages, examples, VS Code, and editor support.

### Provider failure is a value (shipped)

- [x] **`Failed(reason)` is a fourth model-call outcome.** A refused
      connection, a timeout, a 429, a 5xx: the most common failure in a real
      agent program was the one failure the language had no value for, so
      "failure is a value" was true of everything except the part that fails
      most. It is deliberately not folded into `Uncertain` — the model saying
      no and nobody answering are fixed in different places.
- [x] **Model calls retry with jittered backoff.** `http` already retried a
      GET; the model transport was the one place the "timeouts and retries are
      mandatory" rule went unapplied. `[models] max_retries`, default 2. A
      provider's `Retry-After` wins, capped. Only the HTTP request is retried,
      not the tool loop around it.
- [x] **A provider outage no longer discards finished `parallel for` work.**
      One rate-limited branch used to take every completed branch with it.
      Expected failures are values in their own slot now; a branch that
      genuinely raises still fails the loop, but the error says how many
      results were lost with it.
- [x] Journaled, never recorded to a cassette: a durable resume must take the
      failure branch again, and a fixture must not freeze one afternoon's
      outage.
- [x] Docs, site, `DECISIONS.md`, examples, and the unmatched-value hint that
      names the arm to add.

### Tool-server failure is a value (shipped)

The same rule, applied where it had been missed. `Failed(reason)` fixed the
model transport; the tool side of the same loop still had no timeout at all.

- [x] **Every MCP request has a deadline.** `[mcp] timeout_secs`, default 60,
      overridable per server. There was none before, and a blocking read on a
      pipe cannot be given one portably — so the transport reads on its own
      thread and waits with a deadline. A server that accepted a request and
      never answered used to stop the program forever: no error, no exit,
      nothing to match on. That is a worse failure than crashing.
- [x] **A server that does not answer is `Failed(reason)`, not a dead run.**
      It ends the `analyze` call the way an unreachable provider does, naming
      the server and the tool. Deliberately not handed back to the model as a
      tool result: a model told "that tool failed" will try it again, pay the
      timeout every remaining turn, and the call then reports `Exhausted` —
      which names the wrong cause. A tool that *runs* and reports its own
      failure is still a result the model sees.
- [x] **A tool call is never retried.** Where MCP parts company with the model
      transport: generating twice costs tokens, but calling a tool twice may
      open two issues or charge a card twice, and a timeout is precisely when
      whether it ran is unknown. MCP's `readOnlyHint` and `idempotentHint` are
      claims by the server, which is outside the trust boundary. Starting a
      server and shaking hands *is* retried, because nothing has run yet.
- [x] A late answer to a timed-out request is matched by id and skipped, so it
      cannot be read as the reply to the next call — a wrong answer being
      worse than an error.
- [x] End-to-end tests against a real child process and a real model
      transport, including one that proves the side effect ran exactly once.
      Docs, site, `DECISIONS.md`, and the example.

### The pattern set (shipped)

- [x] **`examples/patterns/`** — the seven agent and workflow patterns from
      LangGraph's *Workflows and agents* guide, written in Kora so the two can
      be read side by side: augmented LLM, prompt chaining, parallelization,
      routing, orchestrator-worker, evaluator-optimizer, and the agent loop.
- [x] Each carries its own `test` blocks and needs no model, no API key, and
      no cassette, so all 29 run on every push. Every pattern forces
      `Uncertain`, `Exhausted`, and `Failed` — the paths nobody tests, because
      provoking them normally means making a real model misbehave.
- [x] **A mock now crosses a `parallel for` boundary.** Workers inherited the
      budget, the cassette, and the journal but not the mock stack, so a test
      that fanned out reached for a real model and failed in replay mode. The
      fan-out was the one path most worth testing and the one that could not
      be.
- [x] `patterns/README.md` states what is *not* good yet, and is kept current
      as each gap closes rather than left to describe an older version of the
      language — see below.

### The pattern set's remaining gaps, closed (shipped)

Everything `patterns/README.md` originally listed under "what is not good
yet" except streaming's own scope (see Language and runtime status) is now
fixed:

- [x] **`analyze` requests a nested declared type.** `list[Section]` reaches
      the model directly; `05_orchestrator_worker.ko`'s parallel-array
      workaround and its ragged-plan case are gone. (This had already shipped
      on `main` before the pattern set called it out — the example just
      hadn't been updated to use it yet.)
- [x] **The tool loop is open at one point.** `analyze(..., tools=[...]) on
      tool_call(name, args):` runs before each call the model asks for. `args`
      is a mutable `dict`, so rewriting it is ordinary index-assignment;
      `return`ing a string from the block skips the tool and hands that string
      back as its result instead. Hangs off the assignment the same way
      `on token(t):` does, for the same reason — the tool loop still produces
      one outcome to match on. Only one `on ...` block per call.
- [x] **An agent is a valid tool.** `tools=[specialist]` accepts an `agent`
      the same as a `tool`, dispatched through the same `call_function` a
      direct call would use — same heap, own `budget:` line honored. Not
      given the isolated heap a `parallel for` worker gets: that isolation
      exists to make concurrent threads safe, and a tool call is one more
      synchronous step in the same loop, not a second thread.
- [x] **A mock is no longer one value for the whole flow.** `with mock`
      already nested; the fix was searching the mock stack innermost-first
      for the one matching the call's declared type instead of erroring on
      the first mismatch. A flow calling `analyze` for two different types
      mocks each with its own nested block — no new syntax.

### Durable pipelines: writes are exactly-once (shipped)

`--durable` covered model calls, tools, `ask_human`, output, the clock, HTTP
and Python — everything except the call a data pipeline exists to make. A
killed run re-ran its writes from the top, so the language delivered
exactly-once for the model's tokens but not for the customer's rows.

- [x] **`fs.write`, `fs.append` and `sql.execute` are journaled effects.** A
      resume replays the recorded outcome instead of performing the write
      again.
- [x] **Recorded in two lines, not one.** The attempt is journaled and synced
      before the call runs; its outcome supersedes it after. A resume that
      finds an attempt with no outcome stops and names the call, rather than
      repeating a write that may have landed — the rule the tool loop already
      applies to a timed-out MCP call.
- [x] **Reads are verified, not replayed.** `fs.read`, `fs.lines`, `fs.list`,
      `fs.glob` and `sql.query` re-read live with a digest journaled
      (`Effect::Input`). Input data does not belong in a log of decisions,
      but a resume that reads *different* data stops instead of mixing two
      inputs into one output.
- [x] **Append-only journal, fsync policy, and run locking.** One line per
      record; every record synced except output (a lost `print` repeats a
      line, and syncing thousands of them costs more than the work it
      protects); one process per run, held with an OS advisory lock that a
      kill releases.
- [x] **Crash-injection tests.** `crates/kora-cli/tests/durable_crash_test.rs`
      kills a running pipeline against the real binary and resumes it: no row
      written twice, a second resume of a live run refused, and a resume
      against edited input stopped.
- [x] Docs, site, `DECISIONS.md`, and `examples/19_durable_pipeline.ko`.

Still open, and deliberately not in this pass: per-write cost is two fsyncs,
capping pure-write throughput near a hundred rows a second. The fix is group
commit across `parallel for` workers, not a weaker guarantee.

## Development

- [x] Refresh the documentation welcome page with a more playful guided
      experience, responsive styling, and reduced-motion-safe animation.
- [x] Add dedicated Configuration, Packages, and Developer guide pages, and
      link them into the public docs navigation.
- [x] Combine the selected editorial, maximalist, graffiti, and friendly-green
      visual directions into one final welcome-page design.
- [x] Rework the selected welcome-page direction with dark mode as the default,
      light mode as the alternative, and restrained motion.
- [x] Rebuild the welcome page from the selected Figma composition, including
      the hero, guided routes, Figma-exported icons, and responsive dark/light
      treatments.
- [x] Add and connect every sidebar page shown in the selected documentation
      reference, then verify the full navigation.
- [x] Run the site build and documentation checks after the content refresh.
- [x] Build the standalone React marketing landing page from the approved Kora
      visual, with direct links to Getting started and the documentation.
- [x] Tighten the landing page hero, proof row, and footer spacing to match the supplied reference composition.
- [x] Audit and refine landing navigation, syntax highlighting, execution trace, mascot presentation, viewport fit, and documentation routes.
- [x] Add workflow ergonomics: concise `stream`, outcome-status bindings, and
      match alternatives. Terminal output now redacts classified leaves with a
      configurable project marker.

## Language and runtime status

- [x] `if` / `let` / `match` / `case` — core language constructs, implemented.
- [x] `Ok` / `Uncertain` / `Exhausted` / `Failed` — model-call outcome values.
- [x] `analyze(...)` — implemented, blocking or streamed.
- [x] Classified / `declassify` — label enforcement, implemented.
- [x] `type` / `int` / `str` / `bool` — core types, implemented.
- [x] `fs` / `csv` / `http` / `json` stdlib modules.
- [x] `mcp` — tool-server integration (`kora-mcp` crate).
- [x] **Token-by-token model streaming.** `answer: str = analyze(...) stream`
      is the concise terminal-output form; `on token(t):` remains available
      for custom handlers. Both hand over the answer as the model writes it, and the call
- [x] **Lexical context policy.** `with context(max_input_tokens = N,
      reserve_output_tokens = N):` deterministically bounds model request
      context without spending from the lexical `budget`. It retains newest
      whole tool exchanges and fails rather than clipping base prompt or data.
- [ ] **Prompt-injection resilience.** Tool output is attributed as untrusted
      data before a later model turn. Delimiters reduce confusion but are not
      a security boundary; add explicit tool-call policies, bounded result
      projections, provenance, and adversarial MCP/retrieval tests.
- [x] **Token-by-token model streaming.** `answer: str = analyze(...) on
      token(t):` hands over the answer as the model writes it, and the call
      still returns an outcome to match on — a loop over the pieces would end
      identically on success and on outage, which is the one failure this
      language exists to remove. Only a `str` result streams: a declared type
      arrives as JSON, so its pieces are syntax rather than prose. `str` is
      therefore a result type now, carried in a one-field object so
      `Uncertain` survives, with the refusal field written first so a watcher
      knows which one it is reading. A stream that breaks after emitting is
      `Failed` and is never retried, because the answer would be written
      twice over output the program already acted on. Piece boundaries are
      recorded to the cassette and the journal, since a handler that counts
      them must see the same run twice. `write` is `print` without the
      newline.
- [ ] Streaming alongside tools, and streaming across `parallel for` — both
      refused today rather than silently degraded. Budget metering is still
      per call, not per token: providers report usage only when a stream
      ends, so an in-flight call cannot be stopped at an exact ceiling.
- [ ] SSE / `events` / `Streams` — no server-sent-events or generic event/stream
      stdlib support yet.
- [ ] `xml` / `yaml` stdlib modules — not implemented (only `fs`, `csv`,
      `http`, `json`, `glob`, `re`, `sql`, `time`, `env` exist today).
- [ ] `network` — no dedicated stdlib module beyond `http`.
- [ ] CLI beautification — no dedicated polish pass tracked yet.

### Runtime robustness (stress-test pass, 2026-08-31)

Found with `scripts/stress.py` (new — breaking-point probes under a memory-
safe watchdog, separate from `scripts/bench.py`'s throughput numbers). Full
writeup in [DECISIONS.md](DECISIONS.md), "Execution strategy".

- [x] **Deep recursion crashed the process instead of erroring.** Past
      ~1900 nested calls the host stack overflowed (SIGABRT, uncatchable).
      Fixed: errors cleanly at 1000 nested calls (`call_depth` guard in
      `interp.rs`).
- [x] **`s = s + x` / `s += x` accumulation was O(n²).** Fixed for the plain
      local-name case via an in-place append when nothing else references
      the string (`try_inplace_str_concat`). 1M-char loop: 15s → 0.44s.
      `strings` benchmark: 317ms → 67ms.
- [x] **The `--durable` journal was O(n²).** Fixed: `.kora/runs/<id>.jsonl`
      is append-only, one line per record, so an effect costs one write
      instead of a rewrite of the whole run. 40K effects in 0.27s, against a
      previous ceiling where 15K could not finish in 45s. A torn last line is
      dropped on load (that effect's result reached nobody); a bad line
      earlier is reported. Old `.json` runs are migrated the first time they
      are resumed or answered.

## Capability roadmap

Status legend: **Have** means the capability is implemented and exercised;
**Partial** means the core exists but important semantics or production pieces
remain; **Build** means it is not implemented yet.

| Priority | Capability | Status | Current coverage | What remains to build or improve |
|---|---|---|---|---|
| P0 | Agent primitive | **Have** | `agent` functions, isolated heaps, budgets, tools, and durable runs | Add explicit agent lifecycle, cancellation, handoff, and supervision semantics |
| P0 | Model abstraction | **Have** | Provider abstraction for OpenAI and Ollama, named models, schema requests | Add provider capability negotiation, richer provider errors, and fallback policy |
| P0 | Typed tools | **Have** | Typed Kora tools and typed MCP tool schemas | Add richer parameter types, result schemas, validation, and tool cancellation |
| P0 | Structured output | **Have** | Declared Kora types become validated model JSON schemas | Add schema evolution/versioning and better provider compatibility diagnostics |
| P0 | Async/concurrency | **Partial** | Real OS-thread `parallel for` with isolated worker heaps | Add explicit cancellation, backpressure, bounded queues, fair scheduling, and a clear async/event model |
| P0 | Streaming | **Partial** | `str` streaming with `on token`, replay chunks, `write`, crash-safe durable resume, budget accounting, and retry state, all covered by live-transport tests | Tool streaming, parallel streaming, and per-token in-flight enforcement |
| P0 | Timeouts + cancellation | **Partial** | Model, HTTP, and MCP timeouts; `budget: max_seconds` bounding a scope and every `parallel for` branch under it; handler can stop reading | Interrupt work already in flight, let a program stop a fan-out early, and define cleanup guarantees |
| P0 | Retry/backoff | **Have** | Jittered model retries and HTTP retries; MCP handshake retries | Add shared retry policy, observability for attempts, and cancellation-aware backoff |
| P1 | Durable execution | **Partial** | Append-only replay journal for model calls, tools, writes, human input, output, time, and Python; per-effect fsync, run locking, torn-tail recovery, interrupted-stream semantics | Group commit for write-heavy fan-out, and retention/compaction |
| P1 | Checkpoint/resume | **Partial** | Replay-based resume, `ask_human` suspension, exactly-once writes, and crash-injection tests against the real binary | Add explicit checkpoints, resumable in-flight effects, and versioned state migration |
| P1 | Human approval | **Have** | `ask_human`, durable suspension, classified-data checks | Add approval identity, expiry, denial/revocation, and audit metadata |
| P1 | Guardrails | **Partial** | Labels, declassification, unverified data direction, schema validation, budgets | Complete `unverified` enforcement, policy composition, prompt/output controls, and configurable safety policies |
| P1 | Tracing/metrics | **Partial** | OpenTelemetry spans and local trace output | Add a metrics pipeline, stream/token/tool counters, stable event IDs, and export backpressure |
| P1 | Context management | **Partial** | Prompt construction and tool history within one model call | Add token-aware context windows, truncation, summarization, retention policy, and typed context objects |
| P1 | Sessions/memory | **Build** (designed, see below) | No persistent user-facing session or memory abstraction | Build durable session IDs, scoped memory, retrieval/update rules, privacy labels, eviction, and replay semantics |
| P1 | MCP | **Have** | Server discovery, typed tools, timeouts, failure values, capability checks | Add richer MCP schemas, cancellation, reconnect policy, server health, and protocol-version negotiation |
| P2 | Multi-agent/handoffs | **Partial** | Agents and parallel workers exist as isolated execution units | Build first-class messages, handoff contracts, ownership transfer, supervision, and failure semantics |
| P2 | Sandboxed execution | **Partial** | Package grants, process boundaries, Python sidecar, and no native shared libraries | Build OS-level sandboxing and WASM components with capability enforcement |
| P2 | Model routing/fallback | **Partial** | Named model roles and per-call model selection | Build policy-based routing, health-aware fallback, cost/latency rules, and deterministic replay of route decisions |
| P2 | RAG/embeddings | **Build** (designed, see below) | No embeddings, chunking, retrieval, or vector index | Build embedding effects, document ingestion, chunk identity, retrieval APIs, labels, and cassette/journal behavior |
| P2 | Scheduler/cron | **Build** | No language-level scheduled execution | Build a scheduler primitive, durable triggers, retries, overlap policy, time zones, and operational inspection |
| P2 | Distributed agents | **Build** | Execution is local to one process and host | Build remote workers, transport/authentication, placement, shared journal semantics, and network partitions |
| P3 | GPU/local inference | **Partial** | Local Ollama provider works; in-process inference is parked | Measure and, if justified, build sandboxed in-process inference or a stronger local runtime integration |
| P3 | Native vector store | **Build** | No native vector-store module | Build a capability-scoped vector index with persistence, filtering, migrations, and embedding compatibility |
| P3 | Browser/computer-use runtime | **Build** | No browser or computer-use runtime | Build a separate sandboxed runtime with screenshots, actions, permissions, replay, and human takeover |

### Suggested capability build order

- [ ] **P0 correctness gate:** streaming accounting, retry state, durable
      crash semantics, live transport tests, and a time budget
      (`max_seconds`) have shipped. What remains under "cancellation" is the
      half a deadline does not cover: interrupting work already in flight,
      and a way for a program to stop a fan-out early (first-success-wins).
      Both are the same "did it happen" problem that makes a tool call
      unretryable, and worth solving once, deliberately.
- [ ] **P1 reliability layer:** explicit checkpoints remain; fsync policy,
      run locking, exactly-once writes, interrupted-stream semantics, and
      fault-injection tests have shipped.
- [ ] **P1 agent product layer:** add sessions/memory and context management
      before multi-agent handoffs; otherwise agents have no durable state to
      hand over safely. See "Context engineering, phase 2" immediately below
      for the sequencing inside this layer itself.
- [ ] **P2 execution layer:** add policy-based routing, sandboxed WASM, and
      first-class handoffs before distributed workers.
- [ ] **P2 data layer:** add embeddings and retrieval with labels and replay
      semantics before calling the language RAG-ready.
- [ ] **P3 integrations:** evaluate GPU inference, vector stores, and browser
      runtime only after the local execution and durability contracts are
      stable.

### Context engineering, phase 2 (designed, not built)

Full design for each item — syntax, runtime semantics, rejected alternatives,
and what is explicitly out of scope — is in
[DECISIONS.md](DECISIONS.md#context-engineering-phase-2). It builds on the
typed context fence (`with context(max_input_tokens=N,
reserve_output_tokens=N):`) landing separately. Recommended build order,
argued rather than assumed:

- [ ] **Notes** (`use notes`) before **sessions** (`use session <name> as
      <alias>`). This reverses how the two are usually named, but not the
      dependency: a session is a note store addressed by an explicit key
      instead of an implicit one (the current run's own id). Building the
      single-key form first proves the underlying mechanic — a storage tier
      distinct from the journal, label propagation on read and write, and a
      journaled `Effect::Memory` entry so replay does not diverge when the
      backing store keeps moving — before also taking on the harder
      questions sessions add: capability grants per store, key collisions
      across callers, and eviction across many keys instead of one.
- [ ] **Sub-agent handoff with a distilled return** next, ahead of retrieval.
      Most of it is already true by construction — every `analyze()` call
      already builds an explicit prompt with no ambient conversation state to
      leak, so a delegated agent's context is already clean. The only real
      gap is an opt-in isolated heap for an agent called through
      `tools=[...]` (`agent specialist(...) -> Digest: isolated`), which is a
      small, contained addition to code that already exists
      (`crates/kora-runtime/src/interp.rs`'s `tool_list` and
      `run_parallel`), not a new subsystem — cheap leverage, so it goes
      before the more expensive work.
- [ ] **Tool-result curation** (`on tool_result(name, args, result):`) third.
      It is a second hook symmetric with the existing `on tool_call`, in the
      same file and the same tool loop — contained scope, and it directly
      serves the context-rot problem this whole roadmap answers: a curated
      result is exactly what the context fence should be pruning from,
      rather than an uncurated one.
- [ ] **Full sessions** (the explicit-key generalization of notes, with
      config-declared stores, grants, and eviction) fourth, once the notes
      primitive it builds on has shipped and been exercised.
- [ ] **Just-in-time retrieval / RAG primitives** last, despite being the
      article's headline idea. Its most natural first consumer inside Kora is
      recalling from a session or notes store — "what has this agent already
      learned" is itself a retrieval query — so building it before sessions
      exist leaves it with no concrete caller to validate against. It is also
      the largest new surface of the five (a new model-transport effect,
      chunking, a config-declared vector index, and a provenance rule for
      `unverified` chunks), which argues for landing it once the smaller
      primitives it composes with — the context fence and a memory store to
      search from — are both in place.

## Test coverage

Measured with `cargo llvm-cov --workspace --summary-only`.

- Baseline before this pass: **78.44%** regions / 78.69% lines.
- After: **82.92%** regions / 83.06% lines, with the largest single hole
  closed --
  `stdlib/notes.rs` was at **0.00%**: a shipped feature with a label rule, a
  journal rule and a file outside the run, and not one test.
- New suites: `notes_test.rs`, `http_test.rs`, `config_test.rs`,
  `stdlib_paths_test.rs`, `durable_stream_test.rs` (kora-runtime);
  `commands_test.rs` (kora-cli, every documented command against the real
  binary); `rendering_test.rs` (kora-syntax, every `TokenKind` a diagnostic
  can name); `diagnostics_test.rs` (kora-types).

**99% workspace-wide is not the target, and chasing it would make the suite
worse.** What is left uncovered is mostly code whose tests would assert
nothing a person cares about: `Debug` impls, platform branches that cannot
both run on one machine (the Windows file lock against the Unix `flock`),
exhaustive `match` arms that exist so the compiler can prove a case is
impossible, and the CLI's own failure-to-print-an-error paths. The honest
target is high coverage of behaviour, and the gaps worth closing next are
named by size rather than by percentage:

- [ ] `interp.rs` — the biggest remaining absolute gap (~1500 regions). Needs
      language-level tests for agents, `parallel for` edge cases, and the
      mock/test machinery, not more unit tests.
- [ ] `kora-pkg` `commands.rs` / `edit.rs` — dependency resolution and
      manifest editing, ~470 regions between them.
- [ ] `kora-lsp` and `kora-dap` — editor and debugger servers, exercised by
      hand today.

## Audit follow-up

Findings from [COMPILER_AUDIT.md](COMPILER_AUDIT.md) and
[COMPILER_AUDIT.html](COMPILER_AUDIT.html). Prioritize the first two before
extending streaming with tools or `parallel for`.

- [x] **Fix streaming budget accounting.** Done. `max_calls` and reported
      token usage go through the shared `Budget` on every streamed ending,
      including a `Failed` one. The hole that remained was a handler that
      raises: the call had been made and the tokens spent, but the run
      unwound before charging, which made raising in a handler the cheapest
      way to reach a provider. It is charged on the way out now. Streaming
      inside `parallel for` is still refused, so there is no worker case to
      account for yet.
- [x] **Define crash semantics for durable streaming.** Done. A streamed
      call marks its journal slot before it is sent, so a resume can tell an
      interrupted stream from one that never started. An interrupted stream
      with output recorded under it returns `Failed` and is never sent again
      (the live no-retry-after-emit rule, made durable); one that wrote
      nothing is sent again like ordinary unfinished work. A broken stream's
      pieces are now kept on the recorded outcome, so `Failed` and
      `Uncertain` replay their output in place instead of diverging.
      Covered by `kora-runtime/tests/durable_stream_test.rs` and a
      kill-and-resume test against the real binary in
      `kora-cli/tests/durable_crash_test.rs`.
- [x] **Correct streaming retry state.** Done. `Observed::Text` was already
      set only by a frame that yields characters of the answer, but the
      read-error path defeated it: a body that broke mid-response was built
      as unretryable on the assumption that characters had arrived, so a
      stream that died after nothing but a keep-alive or a usage frame lost
      its retry. The error kind is retryable now and `emitted` is the single
      authority. Both directions are covered in
      `kora-runtime/tests/stream_transport_test.rs`.
- [x] **Add live transport end-to-end tests.** Done.
      `kora-runtime/tests/stream_transport_test.rs` drives the real `ureq`
      path against loopback fixtures for SSE framing and keep-alives, Ollama
      JSON lines, chunked bodies, retries in both directions, deadlines,
      provider errors delivered after a 200, usage frames, budget
      accounting, and handler failures.
- [x] **Complete public streaming documentation.** Done.
      `site/app/model-calls/page.mdx` (the page that replaced
      `language/page.mdx`) and `docs/language.md` now cover `stream`,
      `on token`, `write`, what a broken stream does to output, the durable
      resume rule, and the combinations that are refused.
- [x] **Strengthen cassette identity.** Done. Keys are SHA-256 over the
      length-prefixed key material, and image fingerprints are SHA-256 of the
      bytes. The old FNV-1a algorithm is kept as a read-only lookup fallback
      so committed cassettes -- including the image one, which cannot be
      regenerated without the model that recorded it -- keep replaying;
      nothing is written with it. Both algorithms are pinned by golden tests
      against a real committed key.
- [x] **Separate provider framing from language semantics.** Done.
      `stream::frame` is the one place wire framing lives: it turns a raw
      line -- SSE `data:` prefixes, keep-alive comments, the `[DONE]` marker,
      or a bare JSON line -- into one internal payload protocol, and
      `parse_delta` now sees only a provider's JSON shape. It normalizes both
      wire forms rather than choosing by configured provider, deliberately:
      an OpenAI-compatible proxy behind an `[models.local]` endpoint answers
      a locally-configured model in SSE, and picking framing from the
      provider would fail that deployment for a reason the user cannot see.
- [x] **Specify effect state transitions.** Done, in
      [DECISIONS.md](DECISIONS.md) under "The four states every effect passes
      through": prepared, sent, observed, and terminal, with the retry,
      budget, journal, telemetry, and resume rule for each and a table of what
      a resume does with every shape of slot. Writing it down surfaced a real
      hole and closed it: a tool-using `analyze()` was journaled as one effect
      with the tools it ran leaving no trace, so a crash mid-loop silently
      re-ran every tool. It now leaves a mark before it is sent, and a resume
      that finds one stops and names the call in doubt -- the same answer
      writes already give. Cancellation is the one column with nothing to
      document yet; it is still only a handler returning `Stop` and the
      configured deadlines, and remains open under the P0 gate.
### Editor and documentation, checked against the code

Walked the AGENTS.md list for this pass. What needed changing, and what did
not and why:

- [x] **Docs.** `docs/language.md` (streaming and the durable rule),
      `docs/cli.md` and `site/app/cli/page.mdx` (the resume table, the
      unknown-id refusal, the no-`main` refusal, and the fact that moving a
      call's line invalidates its cassette entry), `site/app/model-calls`,
      `site/app/roadmap`, `README.md`, `DECISIONS.md` (+ the generated
      `/decisions`). `scripts/check_docs.py` passes clean, and the two pages
      it was not checking are in its list now.
- [x] **VS Code extension.** README corrected: it named the constructs the
      grammar highlights incompletely, its install symlink used a version
      that did not match `package.json`, and it did not say that a debug
      session is never durable -- which is a deliberate limit, not a missing
      feature, since stepping through a durable run would write the stepping
      into the run's own record.
- [x] **Grammar: verified, not assumed.** Every keyword in the lexer's table
      is highlighted by `kora.tmLanguage.json`, plus the contextual words
      (`analyze`, `ask_human`, `on`, `token`, `tool_call`, `stream`, `with`)
      that are language surface rather than identifiers. Diffed mechanically
      rather than read.
- [x] **Checker and runtime agree on names.** Locked by two new tests: every
      name in `builtin_names()` resolves at runtime, and every module in
      `module_names()` can be imported. This is the drift that costs most --
      a name the editor offers and `kora check` accepts, which fails when the
      line runs.
- **LSP and DAP needed nothing.** This pass added no construct, no keyword,
      and no stdlib name; hover, completion, and go-to-definition all read the
      same analysis, and the debug adapter's launch options are unchanged.
      Said out loud rather than skipped, per the rule in AGENTS.md.
- **CHANGELOG.md is generated** by release-please from commit history, so it
      is not hand-edited here.

### Found while raising coverage

- [x] **`--resume <unknown-id>` silently started a brand-new run.** The run
      was loaded with `unwrap_or_else(|_| Run::new(...))`, so a typo'd id did
      not resume anything -- it began a fresh run under that name. For a
      durable pipeline that is the exact failure `--durable` exists to
      prevent: the user believes the work was picked up where it stopped, and
      it is instead done a second time from the top. It now fails, naming the
      id, and points at `kora runs`.
- [x] **`kora run` on a file with no `main()` did nothing and said nothing.**
      Exit code 0, no output -- indistinguishable from a program whose output
      was swallowed. It now says so, unless the file has top-level statements
      to execute, which is a real (if rare) way to write a script.
- [ ] **Arity, unknown fields, and duplicate definitions are runtime errors,
      not check-time ones.** `kora check` passes a program that builds a
      2-field type with 1 argument; the message only arrives when the line
      runs, which in an agent program can be minutes and several model calls
      in. The messages themselves are good -- this is about *when* they
      arrive. All three are statically decidable for declared types and
      top-level functions. Pinned as the current boundary by
      `kora-types/tests/diagnostics_test.rs::arity_and_fields_are_left_to_the_runtime`,
      which is the test to convert when this moves.

- [x] **Structural operation ids.** Done. An effect is identified by which
      call it is -- enclosing function, plus position among that function's
      calls (`kora_syntax::ops`) -- rather than by the line it sits on.
      Comments, blank lines, and reformatting no longer invalidate a cassette
      entry or stop a durable run from resuming. Numbered per module, since
      spans are byte offsets into one file. Old cassettes keep replaying
      through the line-based fallback but stay line-sensitive until
      re-recorded; the journal format is bumped, and an older run is refused
      with a sentence rather than replayed into a misleading divergence.

- [ ] **Introduce a typed effect-aware IR.** Still not started, and still
      deliberately: it is a new lowering stage and a retarget of the
      tree-walking interpreter, and it gates streaming-with-tools and the
      bytecode VM, neither of which is being built. Landing half of it would
      leave a second representation of the program with no consumer.

      The complaint that made it urgent is gone -- effect identity is
      structural now, and that took a side table over the AST rather than a
      lowering. What the IR is still worth doing for is explicit effect nodes
      the checker and a future VM can both read, which is a reason to start
      it when one of those two is actually being built.

### Context engineering delivery order

- [x] Bound short-term tool-loop context with a lexical policy, whole-exchange
      retention, conservative deterministic estimation, and untrusted-result
      attribution.
- [ ] Add explicit tool-call policies, bounded result projections, provenance,
      and adversarial prompt-injection tests.
- [ ] Add durable structured notes, then compaction with labels, provenance,
      and replay semantics.
- [ ] Add just-in-time retrieval references and bounded excerpts before
      embeddings or a vector store.
- [ ] Add typed handoff contracts that transfer a compact brief and references,
      not raw agent history.

## Queue

- [ ] **Language-surface stabilization.** Tighten compatibility guarantees,
      diagnostics, and configuration behavior before calling Kora
      production-ready.
- [ ] **Broader distribution.** Publish the CLI on crates.io and the VS Code
      extension on Open VSX, alongside the existing Homebrew, npm, and release
      archive channels.
- [ ] **Project-aware editor workflows.** Add richer refactoring, debugging
      views, project configuration, and code actions to the existing VS Code
      and language-server support.

Neither is a hole in what exists; both extend it, and each waits on
something that is not code. Full reasoning in
[DECISIONS.md](DECISIONS.md#deferred-and-what-would-start-them).

- [ ] **Hosted checksum log.** Closes the last window: a package nobody in
      your world has ever fetched, where the attacker's version is live the
      first time anyone does. Needs a server somebody runs, pays for, and
      everyone trusts. **Starts when** packages are being fetched by people
      who did not write them — a real third-party ecosystem.
- [ ] **WASM components for native packages.** Lets a package ship compiled
      code without ending confinement. Loading a native `.so` never becomes
      an option: it bypasses the capability checks entirely. **Starts when** a
      package genuinely cannot be written in Kora or reached through MCP or
      Python.

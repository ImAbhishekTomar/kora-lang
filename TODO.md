# TODO

Where development stands. Updated as work moves, per the development
principle in [AGENTS.md](AGENTS.md).

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

## Language and runtime status

- [x] `if` / `let` / `match` / `case` — core language constructs, implemented.
- [x] `Ok` / `Uncertain` / `Exhausted` / `Failed` — model-call outcome values.
- [x] `analyze(...)` — implemented, blocking or streamed.
- [x] Classified / `declassify` — label enforcement, implemented.
- [x] `type` / `int` / `str` / `bool` — core types, implemented.
- [x] `fs` / `csv` / `http` / `json` stdlib modules.
- [x] `mcp` — tool-server integration (`kora-mcp` crate).
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
| P0 | Streaming | **Partial** | `str` streaming with `on token`, replay chunks, and `write` | Fix budget accounting, crash-safe durable streaming, retry state, live transport tests, tool streaming, and parallel streaming |
| P0 | Timeouts + cancellation | **Partial** | Model, HTTP, and MCP timeouts; handler can stop reading | Add language-level cancellation tokens, cancellation propagation across workers/tools, and cleanup guarantees |
| P0 | Retry/backoff | **Have** | Jittered model retries and HTTP retries; MCP handshake retries | Add shared retry policy, observability for attempts, and cancellation-aware backoff |
| P1 | Durable execution | **Partial** | Replay journal for model calls, tools, human input, output, time, and Python | Define stream transactions, fsync guarantees, run locking, corruption recovery, and retention/compaction |
| P1 | Checkpoint/resume | **Partial** | Replay-based resume and `ask_human` suspension | Add explicit checkpoints, resumable in-flight effects, versioned state migration, and crash-injection tests |
| P1 | Human approval | **Have** | `ask_human`, durable suspension, classified-data checks | Add approval identity, expiry, denial/revocation, and audit metadata |
| P1 | Guardrails | **Partial** | Labels, declassification, unverified data direction, schema validation, budgets | Complete `unverified` enforcement, policy composition, prompt/output controls, and configurable safety policies |
| P1 | Tracing/metrics | **Partial** | OpenTelemetry spans and local trace output | Add a metrics pipeline, stream/token/tool counters, stable event IDs, and export backpressure |
| P1 | Context management | **Partial** | Prompt construction and tool history within one model call | Add token-aware context windows, truncation, summarization, retention policy, and typed context objects |
| P1 | Sessions/memory | **Build** | No persistent user-facing session or memory abstraction | Build durable session IDs, scoped memory, retrieval/update rules, privacy labels, eviction, and replay semantics |
| P1 | MCP | **Have** | Server discovery, typed tools, timeouts, failure values, capability checks | Add richer MCP schemas, cancellation, reconnect policy, server health, and protocol-version negotiation |
| P2 | Multi-agent/handoffs | **Partial** | Agents and parallel workers exist as isolated execution units | Build first-class messages, handoff contracts, ownership transfer, supervision, and failure semantics |
| P2 | Sandboxed execution | **Partial** | Package grants, process boundaries, Python sidecar, and no native shared libraries | Build OS-level sandboxing and WASM components with capability enforcement |
| P2 | Model routing/fallback | **Partial** | Named model roles and per-call model selection | Build policy-based routing, health-aware fallback, cost/latency rules, and deterministic replay of route decisions |
| P2 | RAG/embeddings | **Build** | No embeddings, chunking, retrieval, or vector index | Build embedding effects, document ingestion, chunk identity, retrieval APIs, labels, and cassette/journal behavior |
| P2 | Scheduler/cron | **Build** | No language-level scheduled execution | Build a scheduler primitive, durable triggers, retries, overlap policy, time zones, and operational inspection |
| P2 | Distributed agents | **Build** | Execution is local to one process and host | Build remote workers, transport/authentication, placement, shared journal semantics, and network partitions |
| P3 | GPU/local inference | **Partial** | Local Ollama provider works; in-process inference is parked | Measure and, if justified, build sandboxed in-process inference or a stronger local runtime integration |
| P3 | Native vector store | **Build** | No native vector-store module | Build a capability-scoped vector index with persistence, filtering, migrations, and embedding compatibility |
| P3 | Browser/computer-use runtime | **Build** | No browser or computer-use runtime | Build a separate sandboxed runtime with screenshots, actions, permissions, replay, and human takeover |

### Suggested capability build order

- [ ] **P0 correctness gate:** finish streaming accounting, cancellation
      semantics, and live transport tests before expanding the streaming API.
- [ ] **P1 reliability layer:** complete durable stream transactions,
      checkpoints, fsync/run locking, and fault-injection tests.
- [ ] **P1 agent product layer:** add sessions/memory and context management
      before multi-agent handoffs; otherwise agents have no durable state to
      hand over safely.
- [ ] **P2 execution layer:** add policy-based routing, sandboxed WASM, and
      first-class handoffs before distributed workers.
- [ ] **P2 data layer:** add embeddings and retrieval with labels and replay
      semantics before calling the language RAG-ready.
- [ ] **P3 integrations:** evaluate GPU inference, vector stores, and browser
      runtime only after the local execution and durability contracts are
      stable.

## Audit follow-up

Findings from [COMPILER_AUDIT.md](COMPILER_AUDIT.md) and
[COMPILER_AUDIT.html](COMPILER_AUDIT.html). Prioritize the first two before
extending streaming with tools or `parallel for`.

- [ ] **Fix streaming budget accounting.** Charge `max_calls` and known token
      usage through the shared `Budget`; update `tokens_spent()` and
      `calls_spent()`; include failed streams and parallel workers.
- [ ] **Define crash semantics for durable streaming.** Refuse durable
      streaming until it is atomic, or journal stream start, chunks, terminal
      outcome, and a resumable provider identity. Add a kill-and-resume test
      proving no duplicate request or output.
- [ ] **Correct streaming retry state.** Mark a stream observed only after a
      meaningful answer fragment or an explicitly defined refusal boundary;
      do not let `[DONE]`, usage frames, or keep-alives suppress retries.
- [ ] **Add live transport end-to-end tests.** Use a deterministic local HTTP
      fixture through the real `ureq` path for SSE, Ollama JSON lines, retries,
      timeouts, provider errors, usage frames, and handler failures.
- [ ] **Complete public streaming documentation.** Update
      `site/app/language/page.mdx` and the public reference with `on token`,
      `write`, failure behavior, durability limits, and current restrictions.
- [ ] **Strengthen cassette identity.** Replace FNV-1a with a cryptographic
      hash or verify the full key material; version the key algorithm if old
      cassettes need migration.
- [ ] **Separate provider framing from language semantics.** Introduce
      provider-specific stream adapters that normalize SSE and JSON-lines into
      one internal delta protocol.
- [ ] **Specify effect state transitions.** Document prepared, sent, observed,
      and terminal states with retry, budget, journal, cancellation, telemetry,
      and resume rules.
- [ ] **Introduce a typed effect-aware IR.** Lower checked AST into an IR with
      explicit effect nodes and stable operation IDs before building streaming
      tools or a bytecode VM.
- [ ] **Define OS durability guarantees.** Decide whether journals promise
      process-crash durability, machine-crash durability, and concurrent-resume
      safety; add fsync and per-run locking where required.

## Queue

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

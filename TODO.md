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
- [x] `patterns/README.md` states what is *not* good yet — nested schemas, the
      closed tool loop, single-valued mocks, agents not being tools, no
      streaming — since the honest half of a comparison is the half that says
      where it loses.

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

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

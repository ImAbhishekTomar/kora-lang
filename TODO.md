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

## Development

Nothing in flight. The package system is complete and self-contained.

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

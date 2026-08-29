# Security policy

## Supported versions

Kora is pre-alpha. Only the latest release receives fixes, and there are no
backports.

| Version | Supported |
| --- | --- |
| `0.1.x` | Yes |
| `< 0.1` | No — upgrade |

## Reporting a vulnerability

Report privately through GitHub's
[security advisory form](https://github.com/ImAbhishekTomar/kora-lang/security/advisories/new).
Please do not open a public issue for a vulnerability.

Include the program or manifest that reproduces it, the `kora --version`
output, and what you expected the language to prevent. A runnable `.ko` file
is worth more than a description.

Expect an acknowledgement within a week. Because this is a personal project
rather than a funded one, a fix may take longer than that, and the advisory
will say so rather than going quiet.

## What counts as a vulnerability

Kora makes specific, checkable promises. A vulnerability is a way to break
one of them:

- **`classified` data reaching a sink** without a `declassify` that named that
  sink — including through slicing, arithmetic, f-strings, containers,
  function returns, or the copy into a `parallel for` branch.
- **`unverified` data reaching a dangerous sink** without being narrowed: a
  path, a SQL statement, a URL, or a model name that came from outside the
  program.
- **A dependency exceeding its capability grants** — reaching the network,
  the filesystem, a database, the environment, an MCP server, or Python
  without being granted it. Escaping confinement by spawning a `parallel for`,
  by being reached through a `tool` a model called, or by delegating to a
  sub-dependency all count.
- **A dependency passing on authority it does not hold**, or declassifying
  without both the permission and the named sink.
- **Running package code that does not match the lockfile**, or a fetch that
  accepts different bytes than `kora.sums` recorded for a commit.
- **Executing anything at install time.** The manifest has no field for it;
  a way to make a package run code on `kora add` or `kora install` is a bug.
- **A budget being exceeded**, or `Exhausted` failing to stop work it should
  have stopped.
- **A durable run resuming into a different branch** than the one it
  suspended in, or replaying an effect that should have come from the
  journal.

## Known limits, which are not vulnerabilities

These are documented decisions, not oversights. Reports about them are
welcome as discussion, but they are not treated as advisories.

- **The root program is unrestricted.** Capability grants confine
  dependencies. A program is bounded by its own `kora.toml` and nothing else.
- **A first fetch that nobody has ever made cannot be checked.** `kora.sums`
  records what a commit contained the first time it was seen, in the project
  and on the machine. If nobody in your world has fetched a package before
  and the attacker's version is live, that version becomes the record.
  Closing this needs a hosted log; see
  [DECISIONS.md](DECISIONS.md#deferred-and-what-would-start-them).
- **A package you grant something can use it.** Granting `fs` means the
  filesystem, not part of it. Narrower grants are a planned superset of the
  current shape, not a promise the current shape makes.
- **MCP servers and the Python sidecar are separate processes** and are
  trusted once reached. Kora controls whether a package may reach them, not
  what they do afterwards.
- **`--replay` and cassettes are for determinism, not isolation.** A cassette
  is not a sandbox.

## Reporting something in a dependency

Kora's own dependencies are listed in `Cargo.lock`. A vulnerability in one of
those belongs upstream first; tell us too, so the pin can move.

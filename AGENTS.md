# Working on Kora

Notes for anyone — human or agent — changing this repository.

Read [DECISIONS.md](DECISIONS.md) first. It records *why* the language is the
way it is; the docs record *what*. A change that contradicts a decision needs
that file updated in the same commit, not a comment explaining the exception.

## The rule that matters most

**A language change is not done when the runtime works.** Kora ships a
compiler, a language server, an editor extension, three reference documents, a
public docs site, and a runnable example set. All of them describe the same
language. Any one of them left behind is a bug that reaches a user before it
reaches CI.

So: when you add or change a construct, walk the whole list below and either
update the item or say out loud why it needs nothing.

## Checklist for a language change

Adding, changing, or removing syntax or semantics touches these, roughly in
order:

**Compiler**

- `crates/kora-syntax/src/token.rs` — new keyword? add the token
- `crates/kora-syntax/src/lexer.rs` — and the word that produces it
- `crates/kora-syntax/src/ast.rs` — the node
- `crates/kora-syntax/src/parser.rs` — the parse, plus a test in the same file
- `crates/kora-syntax/src/lines.rs` — a new statement kind with a body must
  list its lines, or breakpoints inside it silently never fire
- `crates/kora-runtime/src/interp.rs` — execution
- `crates/kora-runtime/src/value.rs` — new runtime value shape?
- `crates/kora-runtime/src/portable.rs` — if a value must cross a
  `parallel for` boundary, it needs a portable form or workers break
- `crates/kora-runtime/src/audit.rs` — every `StmtKind` match lives here too;
  a new statement must at minimum be listed as inert
- `crates/kora-types/src/lib.rs` — name resolution and diagnostics, or the
  editor reports errors on code that plainly runs
- `crates/kora-cli/src/main.rs` — new command, flag, or usage text

**Editor**

- `crates/kora-lsp/src/lib.rs` — hover, completion, go-to-definition
- `crates/kora-dap/src/variables.rs` — a new runtime value shape needs a
  summary line and, if it has parts, children in the variables pane
- `crates/kora-dap/src/session.rs` — new launch options, capabilities, or
  requests
- `editors/vscode/syntaxes/kora.tmLanguage.json` — highlighting
- `editors/vscode/package.json` — add keywords if the feature is something
  people would search for. Do **not** hand-edit `version`: release-please owns
  every version in the repository (see [Releasing](#releasing)). A change here
  is only picked up when VS Code restarts, and a dev install's symlink should
  be renamed to match the manifest version or the extension scanner may serve
  the manifest it cached under the old one. Test a manifest change with
  `npx @vscode/vsce package --no-dependencies`, which runs the same validation
  the release does
- `editors/vscode/README.md` — the feature list is the marketplace page

**Documentation** (all of it, every time)

- `README.md` — the pitch and the status list ("not built yet")
- `docs/language.md` — the reference, including the contents list and the
  "Differences from Python" table
- `docs/stdlib.md` — if a module or function changed
- `docs/cli.md` — if a command's behavior changed, including `kora lsp` and
  `kora dap`
- `DECISIONS.md` — the *why*, and the trade-offs deliberately accepted
- `app/language/page.mdx` and `app/reference/page.mdx` — the public docs site
  is a separate copy; it goes stale silently. `app/ecosystem/page.mdx` and
  `app/installation/page.mdx` describe the editor experience

**Examples and tests**

- `examples/` — a runnable program per feature, listed in `examples/README.md`
- `crates/*/tests/` — end-to-end tests that exercise the feature the way a
  user hits it, not just the unit underneath
- `.github/workflows/ci.yml` — a new example that runs deterministically
  belongs in the examples job; a new `test` block belongs in the Kora suite
- `benches/` — a construct on a hot path (evaluation, calls, values, the
  journal) needs a benchmark, or its cost is invisible until someone
  complains. Run `python3 scripts/bench.py --against main` before claiming a
  change is free; see `benches/README.md`

A change to the *debugger* rather than the language touches
`crates/kora-runtime/src/debug.rs`, `crates/kora-dap/`, the `debuggers`
contribution in `editors/vscode/package.json`, `editors/vscode/src/extension.js`,
and the debugging sections of `docs/cli.md`, `README.md`, and
`editors/vscode/README.md`.

## Before you say it works

```bash
cargo fmt --all
cargo clippy --all-targets --all-features
cargo test
cargo build && python3 scripts/check_docs.py --kora ./target/debug/kora
./target/debug/kora check examples/*.ko examples/lib/*.ko
```

`scripts/check_docs.py` parses every code block in the docs, verifies every
documented command and flag exists, and checks every internal link. It is the
backstop for the rule above, not a replacement for it: it cannot tell that a
feature is missing from a page, only that what is written is wrong.

## Releasing

Nobody picks a version number by hand. Every push to `main` is read as
[Conventional Commits](https://www.conventionalcommits.org), and
release-please keeps one open `chore: release` pull request holding the next
version and the generated `CHANGELOG.md`:

| commit prefix | effect on the version |
|---|---|
| `fix:` | patch — `0.1.0` to `0.1.1` |
| `feat:` | minor — `0.1.0` to `0.2.0` |
| `feat!:` or a `BREAKING CHANGE:` footer | major, and minor while below `1.0.0` |
| `docs:`, `test:`, `chore:`, `ci:`, `refactor:` | none |

So the commit message *is* the release decision, which is why the prefix is
not decoration. Merging the release PR tags `vX.Y.Z`, and that tag is what
`release.yml` already listens for — the build, the archives, Homebrew,
crates.io, npm, and both extension marketplaces are unchanged.

Four files carry a version, and all four move together in that PR:
`Cargo.toml` (the workspace and every internal path dependency, marked with
`# x-release-please-version`), `npm/package.json`, and
`editors/vscode/package.json`. `release.yml` verifies the tag matches
`Cargo.toml` and refuses to publish a mismatch, so a missed bump fails the
release instead of shipping a wrong number.

A release PR is a pull request like any other: if the version or the notes
look wrong, say so there rather than tagging by hand.

## House style

- Errors carry a span, a plain-language message, and usually a hint naming the
  fix. Error messages are half the product.
- Comments explain *why*, not *what*. If a line needs a comment to say what it
  does, rename something instead.
- Prefer the design that stays correct as the language grows over the one
  that is quickest to write.
